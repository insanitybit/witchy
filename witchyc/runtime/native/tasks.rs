//! Cooperative tasks on guarded stacks. Only the launching thread may
//! access these globals. No Rust reference spans a context switch.

use super::global::Global;
#[path = "task_switch.rs"]
mod task_switch;
#[path = "task_stack_layout.rs"]
mod task_stack_layout;
use task_stack_layout::{GUARD, MAX_STACK, stack_layout};

use core::{
    ffi::{c_int, c_void},
    ptr,
};

const SLOTS: usize = 256;
const LANES: usize = 256;
const CONTEXT_X19: usize = 0;
const CONTEXT_LR: usize = 11;
const CONTEXT_SP: usize = 20;

#[repr(C, align(16))]
struct Context([usize; 22]);

// task_switch.rs uses eight-byte slots and writes SP at byte offset 160.
const _: () = {
    assert!(size_of::<usize>() == 8);
    assert!(size_of::<Context>() == 176);
    assert!(align_of::<Context>() == 16);
    assert!(core::mem::offset_of!(Context, 0) == 0);
};

type Entry = unsafe extern "C" fn(*const u64);

#[derive(Clone, Copy)]
#[repr(u64)]
enum Status {
    New,
    Paused,
    Finished,
    Running,
}

struct Task {
    task: Context,
    caller: Context,
    parent: *mut Task,
    handle: u64,
    word: u64,
    status: Status,
    entry: Entry,
    mapping: *mut c_void,
    size: usize,
    input: [u64; LANES],
}

// Task allocation requires 16-byte alignment. Assembly receives pointers to
// the individual contexts, regardless of Task field order.
const _: () = {
    use core::mem::offset_of;
    assert!(align_of::<Task>() == 16);
    assert!(offset_of!(Task, task) % align_of::<Context>() == 0);
    assert!(offset_of!(Task, caller) % align_of::<Context>() == 0);
    assert!(size_of::<Option<Entry>>() == size_of::<*const ()>());
};

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    static vm_page_size: usize;
    #[cfg(target_os = "linux")]
    fn getauxval(kind: usize) -> usize;
    fn mmap(
        addr: *mut c_void,
        size: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn mprotect(addr: *mut c_void, size: usize, prot: c_int) -> c_int;
    fn munmap(addr: *mut c_void, size: usize) -> c_int;
    fn malloc(size: usize) -> *mut c_void;
    fn free(addr: *mut c_void);
    fn witchy_task_bootstrap();
    fn witchy_task_switch(from: *mut Context, to: *mut Context);
}

static mut TASKS: [*mut Task; SLOTS] = [ptr::null_mut(); SLOTS];
static mut CURRENT: *mut Task = ptr::null_mut();
static NEXT: Global<u64> = Global::new(SLOTS as u64);

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn slot(handle: u64) -> *mut *mut Task {
    unsafe {
        (&raw mut TASKS)
            .cast::<*mut Task>()
            .add(handle as usize % SLOTS)
    }
}

fn lookup(handle: u64) -> *mut Task {
    let task = unsafe { slot(handle).read() };
    if task.is_null() || unsafe { (*task).handle } != handle {
        trap()
    }
    task
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_task_spawn(
    allocator: u64,
    bytes: usize,
    entry: Option<Entry>,
    input: *const u64,
    lanes: usize,
) -> u64 {
    let Some(entry) = entry else { trap() };
    if allocator != 2
        || !(512..=MAX_STACK).contains(&bytes)
        || bytes % 16 != 0
        || lanes > LANES
        || (lanes != 0 && input.is_null())
    {
        trap();
    }
    let mut handle = 0;
    for _ in 0..SLOTS {
        let candidate = NEXT.get();
        let Some(next) = candidate.checked_add(1) else {
            break;
        };
        NEXT.set(next);
        if unsafe { slot(candidate).read().is_null() } {
            handle = candidate;
            break;
        }
    }
    if handle == 0 {
        return 0;
    }
    #[cfg(target_os = "macos")]
    let (page, flags) = (unsafe { vm_page_size }, 0x1002); // MAP_PRIVATE | MAP_ANON
    #[cfg(target_os = "linux")]
    let (page, flags) = (unsafe { getauxval(6) }, 0x22); // AT_PAGESZ, MAP_PRIVATE | MAP_ANONYMOUS
    let Some((stack, size)) = stack_layout(bytes, page) else {
        return 0;
    };
    // Linux's sandbox permits removing access with mprotect, so start from
    // non-executable writable memory there and protect both ends.
    const NONE: c_int = 0;
    const READ_WRITE: c_int = 3;
    #[cfg(target_os = "macos")]
    let protection = NONE;
    #[cfg(target_os = "linux")]
    let protection = READ_WRITE;
    let mapping = unsafe { mmap(ptr::null_mut(), size, protection, flags, -1, 0) };
    if mapping.addr() == usize::MAX {
        return 0;
    }
    // SAFETY: mmap returned a live mapping of size bytes. The checked stack
    // layout places both GUARD and GUARD + stack strictly inside it.
    let start = unsafe { mapping.cast::<u8>().add(GUARD) };
    #[cfg(target_os = "macos")]
    let guarded = unsafe { mprotect(start.cast(), stack, READ_WRITE) == 0 };
    #[cfg(target_os = "linux")]
    let guarded = unsafe {
        mprotect(mapping, GUARD, NONE) == 0
            && mprotect(start.add(stack).cast(), GUARD, NONE) == 0
    };
    if !guarded {
        unsafe { munmap(mapping, size) };
        return 0;
    }
    let task = unsafe { malloc(size_of::<Task>()).cast::<Task>() };
    if task.is_null() {
        unsafe { munmap(mapping, size) };
        return 0;
    }
    let mut context = [0; 22];
    context[CONTEXT_X19] = task.expose_provenance();
    context[CONTEXT_LR] = (witchy_task_bootstrap as *const ()).expose_provenance();
    context[CONTEXT_SP] = unsafe { start.add(stack) }.expose_provenance();
    unsafe {
        task.write(Task {
            task: Context(context),
            caller: Context([0; 22]),
            parent: ptr::null_mut(),
            handle,
            word: 0,
            status: Status::New,
            entry,
            mapping,
            size,
            input: [0; LANES],
        });
        if lanes != 0 {
            ptr::copy_nonoverlapping(
                input.cast::<u8>(),
                (&raw mut (*task).input).cast::<u8>(),
                lanes * size_of::<u64>(),
            );
        }
        slot(handle).write(task);
    }
    handle
}

unsafe fn yield_task(task: *mut Task, status: Status) {
    unsafe {
        (*task).status = status;
        CURRENT = (*task).parent;
        witchy_task_switch(&raw mut (*task).task, &raw mut (*task).caller);
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_task_enter(task: *mut Task) -> ! {
    unsafe {
        ((*task).entry)((&raw const (*task).input).cast());
        yield_task(task, Status::Finished);
    }
    trap()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_pause(token: u64) -> u64 {
    let task = unsafe { CURRENT };
    if task.is_null() {
        trap();
    }
    unsafe {
        (*task).word = token;
        yield_task(task, Status::Paused);
        (*task).word
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_step(handle: u64, word: u64) -> u64 {
    let task = lookup(handle);
    unsafe {
        if !matches!((*task).status, Status::New | Status::Paused) {
            trap();
        }
        (*task).parent = CURRENT;
        (*task).word = word;
        (*task).status = Status::Running;
        CURRENT = task;
        witchy_task_switch(&raw mut (*task).caller, &raw mut (*task).task);
        (*task).word
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_status(handle: u64) -> u64 {
    unsafe { (*lookup(handle)).status as u64 }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_word() -> u64 {
    let task = unsafe { CURRENT };
    if task.is_null() {
        0
    } else {
        unsafe { (*task).word }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_active() -> u64 {
    u64::from(!unsafe { CURRENT }.is_null())
}

#[unsafe(no_mangle)]
extern "C" fn witchy_task_finish(handle: u64) {
    let task = lookup(handle);
    unsafe {
        if !matches!((*task).status, Status::Finished) {
            trap();
        }
        slot(handle).write(ptr::null_mut());
        if munmap((*task).mapping, (*task).size) != 0 {
            trap();
        }
        free(task.cast());
    }
}
