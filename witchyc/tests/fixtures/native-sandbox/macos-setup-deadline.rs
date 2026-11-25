// The socket-entry worker's two deadlines under Seatbelt (deadline.rs): the setup bound ends a
// worker with SETUP_TIMED_OUT, not SIGALRM; once the sandbox is installed, re-arming with the
// connection deadline replaces both the setup timer and its handler.
#![no_std]
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/deadline.rs"]
mod deadline;
use core::ffi::{c_char, c_int, c_void};
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { abort() }
}
unsafe extern "C" {
    fn abort() -> !;
    fn fork() -> c_int;
    fn pause() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn write(fd: c_int, data: *const c_void, len: usize) -> isize;
    fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
    fn _exit(code: c_int) -> !;
}

fn sandbox() -> bool {
    let mut error = core::ptr::null_mut();
    unsafe { sandbox_init(c"(version 1)\n(deny default)\n".as_ptr(), 0, &mut error) == 0 }
}

/// Runs `child` in a fork and returns its wait status and how long it took.
fn run(child: fn()) -> Option<(c_int, u64)> {
    let started = deadline::monotonic_ms()?;
    let pid = unsafe { fork() };
    if pid < 0 {
        return None;
    }
    if pid == 0 {
        child();
        unsafe { _exit(5) }
    }
    let mut status = 0;
    if unsafe { waitpid(pid, &raw mut status, 0) } != pid {
        return None;
    }
    Some((status, deadline::monotonic_ms()?.saturating_sub(started)))
}

fn say(text: &[u8]) -> c_int {
    let written = unsafe { write(1, text.as_ptr().cast(), text.len()) };
    if written == text.len() as isize { 0 } else { 6 }
}

#[unsafe(no_mangle)]
extern "C" fn main() -> c_int {
    // A setup that outlasts its bound (here: sandboxed, then stalled) exits SETUP_TIMED_OUT.
    let Some((status, _)) = run(|| {
        if !deadline::arm_setup_deadline(50) || !sandbox() {
            unsafe { _exit(3) }
        }
        unsafe { pause() };
    }) else {
        return 1;
    };
    if status & 0x7f != 0 || (status >> 8) & 0xff != deadline::SETUP_TIMED_OUT {
        return 2;
    }
    // A long setup bound, then the sandbox, then the connection deadline: the worker ends by
    // SIGALRM at the connection deadline, long before the setup bound.
    let Some((status, took)) = run(|| {
        if !deadline::arm_setup_deadline(10_000) || !sandbox() || !deadline::witchy_arm_deadline(50) {
            unsafe { _exit(3) }
        }
        unsafe { pause() };
    }) else {
        return 1;
    };
    if status & 0x7f != 14 || took >= 5_000 {
        return 4;
    }
    say(b"setup bound exits 124; connection deadline re-armed under Seatbelt\n")
}
