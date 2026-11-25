#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/linux_sandbox.rs"]
mod linux_sandbox;
use std::ffi::{c_int, c_long, c_void};
unsafe extern "C" {
    fn witchy_install_sandbox_with_relay(
        output: bool,
        allocator: bool,
        input: bool,
        relay: c_int,
    ) -> bool;
    fn socketpair(domain: c_int, kind: c_int, protocol: c_int, result: *mut c_int) -> c_int;
    fn fork() -> c_int;
    fn close(fd: c_int) -> c_int;
    fn read(fd: c_int, data: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, data: *const c_void, count: usize) -> isize;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn open(path: *const std::ffi::c_char, flags: c_int, ...) -> c_int;
    fn _exit(status: c_int) -> !;
    fn getauxval(kind: usize) -> usize;
    fn syscall(number: c_long, ...) -> c_long;
}
fn main() {
    unsafe {
        let mut pair = [-1; 2];
        assert_eq!(socketpair(1, 1, 0, pair.as_mut_ptr()), 0);
        let child = fork();
        assert!(child >= 0);
        if child == 0 {
            close(pair[0]);
            if !witchy_install_sandbox_with_relay(false, true, false, pair[1]) {
                _exit(70);
            }
            let byte = b'X';
            if write(pair[1], (&byte as *const u8).cast(), 1) != 1 {
                _exit(71);
            }
            if write(1, (&byte as *const u8).cast(), 1) >= 0 {
                _exit(72);
            }
            if open(c"/etc/passwd".as_ptr(), 0) >= 0 {
                _exit(73);
            }
            if syscall(64, (pair[1] as u64) | (1u64 << 32), &byte, 1usize) >= 0 {
                _exit(74);
            }
            if syscall(198, 2i32, 1i32, 0i32) >= 0 {
                _exit(75);
            }
            if syscall(222, 0u64, 4096usize, 7i32, 0x22i32, -1i32, 0u64) >= 0 {
                _exit(76);
            }
            let page = getauxval(6); // AT_PAGESZ
            let address = syscall(222, 0u64, page, 3u64, 0x22u64, -1i64, 0u64);
            if address < 0 || syscall(226, address, page, 0u64) != 0 {
                _exit(77);
            }
            for protection in [3, 7, 1u64 << 32] {
                if syscall(226, address, page, protection) >= 0 {
                    _exit(78);
                }
            }
            _exit(0);
        }
        close(pair[1]);
        let mut byte = 0u8;
        let count = read(pair[0], (&mut byte as *mut u8).cast(), 1);
        let mut status = 0;
        assert_eq!(waitpid(child, &mut status, 0), child);
        assert_eq!(status, 0, "sandbox child status {status}");
        assert_eq!((count, byte), (1, b'X'));
        println!("Linux relay confinement passed");
    }
}
