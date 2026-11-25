#![no_std]
#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/clock_random.rs"]
mod clock_random;
#[path = "../../../runtime/native/deadline.rs"]
mod deadline;
use core::ffi::{c_int, c_void};
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { abort() }
}
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() -> ! {
    unsafe { abort() }
}
unsafe extern "C" {
    fn abort() -> !;
    fn fork() -> c_int;
    fn pause() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn write(fd: c_int, data: *const c_void, len: usize) -> isize;
    fn _exit(code: c_int) -> !;
}
#[unsafe(no_mangle)]
extern "C" fn main() -> c_int {
    unsafe {
        let pid = fork();
        if pid < 0 {
            return 1;
        }
        if pid == 0 {
            if !deadline::witchy_arm_deadline(30) {
                _exit(2);
            }
            if deadline::left_ms().is_none() {
                _exit(3);
            }
            pause();
            _exit(4);
        }
        let mut status = 0;
        if waitpid(pid, &raw mut status, 0) != pid || status & 0x7f != 14 {
            return 5;
        }
        let message = b"Linux deadline delivered SIGALRM\n";
        if write(1, message.as_ptr().cast(), message.len()) != message.len() as isize {
            return 6;
        }
        0
    }
}
