#![allow(dead_code)]
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/tasks.rs"]
mod tasks;

unsafe extern "C" {
    fn witchy_task_spawn(
        grant: u64,
        bytes: usize,
        entry: Option<unsafe extern "C" fn(*const u64)>,
        input: *const u64,
        lanes: usize,
    ) -> u64;
    fn witchy_task_step(handle: u64, word: u64) -> u64;
    fn witchy_task_status(handle: u64) -> u64;
    fn witchy_task_finish(handle: u64);
    fn witchy_task_pause(word: u64) -> u64;
    fn witchy_task_word() -> u64;
}

unsafe extern "C" {
    fn check_switch_registers(
        first: u64,
        word: u64,
        tag: u64,
        call: unsafe extern "C" fn(u64, u64) -> u64,
    ) -> u64;
}

core::arch::global_asm!(
    r#"
    .text
    .p2align 2
    .globl {check}
{check}:
    stp x29, x30, [sp, #-176]!
    mov x29, sp
    stp x19, x20, [sp, #16]
    stp x21, x22, [sp, #32]
    stp x23, x24, [sp, #48]
    stp x25, x26, [sp, #64]
    stp x27, x28, [sp, #80]
    stp d8, d9, [sp, #96]
    stp d10, d11, [sp, #112]
    stp d12, d13, [sp, #128]
    stp d14, d15, [sp, #144]
    mov x20, sp
    str x2, [sp, #160]
    mov x19, x2
    mov x21, x2
    mov x22, x2
    mov x23, x2
    mov x24, x2
    mov x25, x2
    mov x26, x2
    mov x27, x2
    mov x28, x2
    fmov d8, x2
    fmov d9, x2
    fmov d10, x2
    fmov d11, x2
    fmov d12, x2
    fmov d13, x2
    fmov d14, x2
    fmov d15, x2
    blr x3
    mov x9, sp
    cmp x9, x20
    b.ne 1f
    cmp x9, x29
    b.ne 1f
    ldr x10, [sp, #160]
    cmp x19, x10
    b.ne 1f
    cmp x21, x10
    b.ne 1f
    cmp x22, x10
    b.ne 1f
    cmp x23, x10
    b.ne 1f
    cmp x24, x10
    b.ne 1f
    cmp x25, x10
    b.ne 1f
    cmp x26, x10
    b.ne 1f
    cmp x27, x10
    b.ne 1f
    cmp x28, x10
    b.ne 1f
    fmov x9, d8
    cmp x9, x10
    b.ne 1f
    fmov x9, d9
    cmp x9, x10
    b.ne 1f
    fmov x9, d10
    cmp x9, x10
    b.ne 1f
    fmov x9, d11
    cmp x9, x10
    b.ne 1f
    fmov x9, d12
    cmp x9, x10
    b.ne 1f
    fmov x9, d13
    cmp x9, x10
    b.ne 1f
    fmov x9, d14
    cmp x9, x10
    b.ne 1f
    fmov x9, d15
    cmp x9, x10
    b.ne 1f
    b 2f
1:
    brk #0
2:
    ldp d8, d9, [sp, #96]
    ldp d10, d11, [sp, #112]
    ldp d12, d13, [sp, #128]
    ldp d14, d15, [sp, #144]
    ldp x19, x20, [sp, #16]
    ldp x21, x22, [sp, #32]
    ldp x23, x24, [sp, #48]
    ldp x25, x26, [sp, #64]
    ldp x27, x28, [sp, #80]
    ldp x29, x30, [sp], #176
    ret
"#,
    check = sym check_switch_registers,
);

unsafe extern "C" fn pause_for_register_check(word: u64, _: u64) -> u64 {
    unsafe { witchy_task_pause(word) }
}

unsafe extern "C" fn inner(input: *const u64) {
    let input = unsafe { input.read() };
    assert_eq!(unsafe { witchy_task_pause(input) }, 17);
}

unsafe extern "C" fn outer(input: *const u64) {
    let value = unsafe { input.read() };
    let child = unsafe { witchy_task_spawn(2, 4096, Some(inner), &value, 1) };
    assert_ne!(child, 0);
    assert_eq!(unsafe { witchy_task_step(child, 0) }, 42);
    assert_eq!(
        unsafe { check_switch_registers(42, 0, 456, pause_for_register_check) },
        17
    );
    unsafe { witchy_task_step(child, 17) };
    assert_eq!(unsafe { witchy_task_status(child) }, 2);
    unsafe { witchy_task_finish(child) };
}

unsafe extern "C" fn empty(_: *const u64) {}

unsafe extern "C" fn pause_once(_: *const u64) {
    unsafe { witchy_task_pause(7) };
}

unsafe extern "C" fn finish_self(_: *const u64) {
    let handle = unsafe { witchy_task_word() };
    assert_eq!(unsafe { witchy_task_status(handle) }, 3);
    unsafe { witchy_task_finish(handle) };
}

fn finish_before_completion(mode: &str) {
    let entry = match mode {
        "finish-new" => empty,
        "finish-paused" => pause_once,
        "finish-running" => finish_self,
        _ => unreachable!(),
    };
    let task = unsafe { witchy_task_spawn(2, 4096, Some(entry), std::ptr::null(), 0) };
    assert_ne!(task, 0);
    match mode {
        "finish-new" => assert_eq!(unsafe { witchy_task_status(task) }, 0),
        "finish-paused" => {
            assert_eq!(unsafe { witchy_task_step(task, 0) }, 7);
            assert_eq!(unsafe { witchy_task_status(task) }, 1);
        }
        "finish-running" => {
            unsafe { witchy_task_step(task, task) };
            return;
        }
        _ => unreachable!(),
    }
    unsafe { witchy_task_finish(task) };
}

#[path = "../../../runtime/native/task_stack_layout.rs"]
mod task_stack_layout;

static TASK_ENTRY_SP: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

extern "C" fn remember_entry_sp(sp: usize) {
    TASK_ENTRY_SP.store(sp, std::sync::atomic::Ordering::Relaxed);
}

unsafe extern "C" {
    fn capture_entry_sp(input: *const u64);
    fn getpagesize() -> i32;
    fn mprotect(address: *mut core::ffi::c_void, length: usize, protection: i32) -> i32;
}

core::arch::global_asm!(
    r#"
    .text
    .p2align 2
    .globl {entry}
{entry}:
    mov x0, sp
    b {remember}
"#,
    entry = sym capture_entry_sp,
    remember = sym remember_entry_sp,
);

fn completed_task_mapping() -> (u64, usize, usize, usize, usize) {
    use task_stack_layout::{GUARD, stack_layout};

    let task = unsafe { witchy_task_spawn(2, 4096, Some(capture_entry_sp), std::ptr::null(), 0) };
    assert_ne!(task, 0);
    unsafe { witchy_task_step(task, 0) };
    assert_eq!(unsafe { witchy_task_status(task) }, 2);
    let entry_sp = TASK_ENTRY_SP.load(std::sync::atomic::Ordering::Relaxed);
    let page = usize::try_from(unsafe { getpagesize() }).unwrap();
    let (stack, mapping) = stack_layout(4096, page).unwrap();
    // The runtime's entry frame is on the first writable page.
    assert_eq!(entry_sp % 16, 0);
    let top = entry_sp.next_multiple_of(page);
    assert!(top - entry_sp < 256);
    let base = top.checked_sub(stack + GUARD).unwrap();

    (task, base, top, mapping, page)
}

fn read_guard_page(upper: bool, allow_read: bool) {
    let (_, base, top, _, page) = completed_task_mapping();
    let guard = if upper { top } else { base };
    assert_eq!(guard % page, 0);
    if allow_read {
        assert_eq!(unsafe { mprotect(std::ptr::without_provenance_mut(guard), page, 1) }, 0);
    }
    // A machine load lets the OS report the protection fault without
    // constructing a Rust reference to a PROT_NONE page.
    unsafe {
        core::arch::asm!("ldrb wzr, [{address}]", address = in(reg) guard, options(nostack));
    }
}

fn finish_unmaps_stack() {
    let (task, base, top, mapping, page) = completed_task_mapping();
    let writable_page = std::ptr::without_provenance_mut(top - page);
    assert_eq!(unsafe { mprotect(writable_page, page, 3) }, 0);
    unsafe { witchy_task_finish(task) };
    for address in (base..base + mapping).step_by(page) {
        let page_address = std::ptr::without_provenance_mut(address);
        let result = unsafe { mprotect(page_address, page, 0) };
        let error = std::io::Error::last_os_error();
        assert_eq!(result, -1, "page at mapping offset {} remained mapped", address - base);
        assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
    }
}

fn stale_after_slot_reuse() {
    let first = unsafe { witchy_task_spawn(2, 4096, Some(empty), std::ptr::null(), 0) };
    assert_ne!(first, 0);
    unsafe {
        witchy_task_step(first, 0);
        witchy_task_finish(first);
    }
    for _ in 1..256 {
        let task = unsafe { witchy_task_spawn(2, 4096, Some(empty), std::ptr::null(), 0) };
        assert_ne!(task, 0);
        unsafe {
            witchy_task_step(task, 0);
            witchy_task_finish(task);
        }
    }
    let replacement = unsafe { witchy_task_spawn(2, 4096, Some(empty), std::ptr::null(), 0) };
    assert_eq!(replacement, first + 256);
    assert_eq!(unsafe { witchy_task_status(replacement) }, 0);
    unsafe { witchy_task_status(first) };
}

fn full_task_table_recovers_one_slot() {
    let spawn = || unsafe { witchy_task_spawn(2, 4096, Some(empty), std::ptr::null(), 0) };
    let mut handles = [0; 256];
    for handle in &mut handles {
        *handle = spawn();
        assert_ne!(*handle, 0);
    }
    assert_eq!(spawn(), 0);
    let released = handles[127];
    unsafe {
        witchy_task_step(released, 0);
        witchy_task_finish(released);
    }
    handles[127] = spawn();
    assert_ne!(handles[127], 0);
    assert_ne!(handles[127], released);
    assert_eq!(spawn(), 0);
    for handle in handles {
        assert_eq!(unsafe { witchy_task_status(handle) }, 0);
        unsafe {
            witchy_task_step(handle, 0);
            witchy_task_finish(handle);
        }
    }
}

#[inline(never)]
fn consume_stack(depth: usize) {
    let mut page = [0u8; 2048];
    std::hint::black_box(&mut page);
    if depth != 0 {
        consume_stack(depth - 1);
    }
    std::hint::black_box(&page);
}

unsafe extern "C" fn overflow(_: *const u64) {
    consume_stack(128);
}

#[repr(C, align(16))]
struct LinkContext([usize; 22]);

unsafe extern "C" {
    fn check_switch_link(
        caller: *mut LinkContext,
        callee: *mut LinkContext,
        callee_top: *mut u128,
    );
}

#[cfg(target_os = "macos")]
core::arch::global_asm!(".set fixture_task_switch, _witchy_task_switch");
#[cfg(target_os = "linux")]
core::arch::global_asm!(".set fixture_task_switch, witchy_task_switch");

core::arch::global_asm!(
    r#"
    .text
    .p2align 2
    .globl {check}
{check}:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x0, x1, [x1]             // callee x19 and x20 carry the contexts
    adr x9, 2f
    str x9, [x1, #88]           // callee link register
    str x2, [x1, #160]          // callee stack pointer
    bl fixture_task_switch
1:
    adr x9, 1b
    cmp x30, x9
    b.ne 5f
    ldr x10, [x1, #88]         // caller's first saved link register
    cmp x10, x9
    b.ne 5f
    adr x9, 3f
    ldr x10, [x0, #88]         // callee's first saved link register
    cmp x10, x9
    b.ne 5f
    mov x9, x0
    mov x0, x1
    mov x1, x9
    bl fixture_task_switch
4:
    adr x9, 4b
    cmp x30, x9
    b.ne 5f
    ldp x29, x30, [sp], #16
    ret
2:
    adr x9, 2b
    cmp x30, x9
    b.ne 5f
    mov x0, x20
    mov x1, x19
    bl fixture_task_switch
3:
    adr x9, 3b
    cmp x30, x9
    b.ne 5f
    mov x0, x20
    mov x1, x19
    bl fixture_task_switch
5:
    brk #0
"#,
    check = sym check_switch_link,
);

fn check_link_register() {
    let mut caller = LinkContext([0; 22]);
    let mut callee = LinkContext([0; 22]);
    let mut stack = [0u128; 64];
    let top = stack.as_mut_ptr().wrapping_add(stack.len());
    // Both context records and the alternate stack remain live during the round trip.
    unsafe { check_switch_link(&raw mut caller, &raw mut callee, top) };
}

fn main() {
    use std::os::unix::process::ExitStatusExt;

    if std::env::args().nth(1).as_deref() == Some("stale") {
        stale_after_slot_reuse();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("guard") {
        let task = unsafe { witchy_task_spawn(2, 4096, Some(overflow), std::ptr::null(), 0) };
        assert_ne!(task, 0);
        unsafe { witchy_task_step(task, 0) };
        return;
    }
    if let Some(mode) = std::env::args().nth(1) {
        match mode.as_str() {
            "guard-lower" => read_guard_page(false, false),
            "guard-upper" => read_guard_page(true, false),
            "guard-lower-readable" => read_guard_page(false, true),
            "guard-upper-readable" => read_guard_page(true, true),
            _ => finish_before_completion(&mode),
        }
        return;
    }
    check_link_register();
    finish_unmaps_stack();
    full_task_table_recovers_one_slot();
    for mode in ["finish-new", "finish-paused", "finish-running"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(mode)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(5), "{mode} must trap: {status}");
    }
    let guard_signal = if cfg!(target_os = "macos") { 10 } else { 11 };
    for mode in ["guard-lower-readable", "guard-upper-readable"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(mode)
            .status()
            .unwrap();
        assert!(status.success(), "{mode} must succeed after mprotect: {status}");
    }
    for mode in ["guard", "guard-lower", "guard-upper"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(mode)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(guard_signal), "{mode} must fault: {status}");
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("stale")
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(5), "stale handle must trap: {status}");
    let task = unsafe { witchy_task_spawn(2, 4096, Some(outer), &42, 1) };
    assert_ne!(task, 0);
    assert_eq!(
        unsafe { check_switch_registers(task, 0, 123, witchy_task_step) },
        42
    );
    unsafe { check_switch_registers(task, 17, 123, witchy_task_step) };
    assert_eq!(unsafe { witchy_task_status(task) }, 2);
    unsafe { witchy_task_finish(task) };
}
