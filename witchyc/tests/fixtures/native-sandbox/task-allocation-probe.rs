//! Offline task fault injection. Build with the symbol redirects documented
//! in native-backend.md.
#![allow(dead_code)]
mod global;
#[cfg(overflow_address)]
mod task_stack_layout;
mod tasks;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
const FAIL_MMAP: usize = 1;
const FAIL_FIRST_PROTECT: usize = 2;
const FAIL_MALLOC: usize = 3;
const FAIL_SECOND_PROTECT: usize = 4;
const FAIL_FINISH_UNMAP: usize = 5;
const FAIL_MALLOC_AND_UNMAP: usize = 6;

static MODE: AtomicUsize = AtomicUsize::new(0);
static PROTECT_CALLS: AtomicUsize = AtomicUsize::new(0);
static MAPPING_SIZE: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" {
    fn mmap(p: *mut c_void, n: usize, prot: c_int, f: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn mprotect(p: *mut c_void, n: usize, prot: c_int) -> c_int;
    fn munmap(p: *mut c_void, n: usize) -> c_int;
    fn malloc(n: usize) -> *mut c_void;
    fn witchy_task_spawn(
        g: u64,
        n: usize,
        e: Option<unsafe extern "C" fn(*const u64)>,
        p: *const u64,
        lanes: usize,
    ) -> u64;
    fn witchy_task_step(h: u64, w: u64) -> u64;
    fn witchy_task_finish(h: u64);
}
#[unsafe(no_mangle)]
unsafe extern "C" fn probe_mmap(
    p: *mut c_void,
    n: usize,
    prot: c_int,
    f: c_int,
    fd: c_int,
    off: i64,
) -> *mut c_void {
    if MODE.load(Relaxed) == FAIL_MMAP {
        return core::ptr::without_provenance_mut(usize::MAX);
    }
    let addr = unsafe { mmap(p, n, prot, f, fd, off) };
    if addr.addr() != usize::MAX {
        MAPPING_SIZE.store(n, Relaxed);
        assert_eq!(LIVE.swap(addr.addr(), Relaxed), 0);
    }
    addr
}
#[unsafe(no_mangle)]
unsafe extern "C" fn probe_mprotect(p: *mut c_void, n: usize, prot: c_int) -> c_int {
    let call = PROTECT_CALLS.fetch_add(1, Relaxed);
    if MODE.load(Relaxed) == FAIL_FIRST_PROTECT
        || (MODE.load(Relaxed) == FAIL_SECOND_PROTECT && call == 1)
    {
        return -1;
    }
    unsafe { mprotect(p, n, prot) }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn probe_munmap(p: *mut c_void, n: usize) -> c_int {
    if matches!(
        MODE.load(Relaxed),
        FAIL_FINISH_UNMAP | FAIL_MALLOC_AND_UNMAP
    ) {
        return -1;
    }
    assert_eq!(LIVE.swap(0, Relaxed), p.addr());
    let result = unsafe { munmap(p, n) };
    assert_eq!(result, 0);
    result
}
#[unsafe(no_mangle)]
unsafe extern "C" fn probe_malloc(n: usize) -> *mut c_void {
    if matches!(MODE.load(Relaxed), FAIL_MALLOC | FAIL_MALLOC_AND_UNMAP) {
        return core::ptr::null_mut();
    }
    unsafe { malloc(n) }
}
unsafe extern "C" fn entry(_: *const u64) {}

#[cfg(overflow_address)]
unsafe extern "C" {
    fn install_overflow_handler(base: usize, size: usize) -> c_int;
}

#[cfg(overflow_address)]
#[inline(never)]
fn consume_stack(depth: usize) {
    let mut page = [0_u8; 2048];
    std::hint::black_box(&mut page);
    if depth != 0 {
        consume_stack(depth - 1);
    }
    std::hint::black_box(&page);
}

#[cfg(overflow_address)]
unsafe extern "C" fn overflow(_: *const u64) {
    consume_stack(128);
}

fn main() {
    #[cfg(overflow_address)]
    if std::env::args().nth(1).as_deref() == Some("overflow-address") {
        let task = unsafe { witchy_task_spawn(2, 4096, Some(overflow), core::ptr::null(), 0) };
        assert_ne!(task, 0);
        assert_eq!(
            unsafe { install_overflow_handler(LIVE.load(Relaxed), task_stack_layout::GUARD) },
            0
        );
        unsafe { witchy_task_step(task, 0) };
        panic!("task overflow returned");
    }
    let spawn = || unsafe { witchy_task_spawn(2, 4096, Some(entry), core::ptr::null(), 0) };
    if std::env::args().len() > 1 {
        let h = spawn();
        assert_ne!(h, 0);
        unsafe {
            witchy_task_step(h, 0);
        }
        MODE.store(FAIL_FINISH_UNMAP, Relaxed);
        unsafe {
            witchy_task_finish(h);
        }
        return;
    }
    MODE.store(FAIL_MALLOC_AND_UNMAP, Relaxed);
    assert_eq!(spawn(), 0);
    let leaked = LIVE.swap(0, Relaxed);
    assert_ne!(leaked, 0);
    let leaked_size = MAPPING_SIZE.load(Relaxed);
    let leaked_ptr = core::ptr::without_provenance_mut(leaked);
    assert_eq!(unsafe { mprotect(leaked_ptr, leaked_size, 0) }, 0);
    MODE.store(0, Relaxed);
    let recovered = spawn();
    assert_ne!(recovered, 0);
    assert_ne!(LIVE.load(Relaxed), leaked);
    unsafe {
        witchy_task_step(recovered, 0);
        witchy_task_finish(recovered);
    }
    assert_eq!(unsafe { munmap(leaked_ptr, leaked_size) }, 0);

    use std::os::unix::process::ExitStatusExt;
    #[cfg(overflow_address)]
    {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("overflow-address")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(42), "overflow fault must be inside lower guard");
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("finish-unmap-failure")
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(5));
    let mut failures = vec![FAIL_MMAP, FAIL_FIRST_PROTECT, FAIL_MALLOC];
    if cfg!(target_os = "linux") {
        failures.push(FAIL_SECOND_PROTECT);
    }
    for mode in failures {
        PROTECT_CALLS.store(0, Relaxed);
        MODE.store(mode, Relaxed);
        assert_eq!(spawn(), 0);
        if mode == FAIL_SECOND_PROTECT {
            assert_eq!(PROTECT_CALLS.load(Relaxed), 2);
        }
        assert_eq!(LIVE.load(Relaxed), 0);
        MODE.store(0, Relaxed);
        let h = spawn();
        assert_ne!(h, 0);
        unsafe {
            witchy_task_step(h, 0);
            witchy_task_finish(h);
        }
        assert_eq!(LIVE.load(Relaxed), 0);
    }
}
