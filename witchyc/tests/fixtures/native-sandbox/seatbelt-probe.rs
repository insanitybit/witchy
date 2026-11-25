#![no_std]

use core::ffi::{c_char, c_int, c_void};
use core::ptr::null_mut;

#[unsafe(no_mangle)]
static witchy_entry_grants: u64 = cfg!(console) as u64;

#[unsafe(no_mangle)]
static witchy_launch_plan: [u8; 1] = [0];

#[unsafe(no_mangle)]
static witchy_secret_slots: u8 = 0;

#[repr(C)]
struct Limit {
    current: u64,
    maximum: u64,
}

unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn dup2(from: c_int, to: c_int) -> c_int;
    fn getrlimit(resource: c_int, limit: *mut Limit) -> c_int;
    fn setrlimit(resource: c_int, limit: *const Limit) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn sandbox_init(profile: *const c_char, flags: u64, error: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(error: *mut c_char);
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn bind(fd: c_int, address: *const u8, length: u32) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn fork() -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buffer: *const u8, count: usize) -> isize;
    fn __error() -> *const c_int;
    fn _exit(code: c_int) -> !;
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static INIT: extern "C" fn() = simulate_inherited_descriptor;

extern "C" fn simulate_inherited_descriptor() {
    const NOFILE: c_int = 8;
    let fd = unsafe { open(c"/dev/null".as_ptr(), 0) };
    let mut limit = Limit {
        current: 0,
        maximum: 0,
    };
    unsafe {
        if fd < 0 || dup2(fd, 177) != 177 || getrlimit(NOFILE, &mut limit) != 0 {
            _exit(30);
        }
        limit.current = 64;
        if setrlimit(NOFILE, &limit) != 0 {
            _exit(30);
        }
    }
}

fn expect_error(result: isize, errno: c_int, code: c_int) {
    if result != -1 || unsafe { *__error() } != errno {
        unsafe { _exit(code) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_entry(grants: u64) {
    const BAD_FD: c_int = 9;
    const DENIED: c_int = 1;
    const GET_FD: c_int = 1;
    let mut byte = 0u8;
    let mut error = null_mut();
    unsafe {
        expect_error(fcntl(177, GET_FD) as isize, BAD_FD, 31);
        expect_error(fcntl(0, GET_FD) as isize, BAD_FD, 32);
        expect_error(fcntl(2, GET_FD) as isize, BAD_FD, 33);
        // A second profile must not restore filesystem access.
        sandbox_init(c"(version 1)(allow default)".as_ptr(), 0, &mut error);
        sandbox_free_error(error);
        expect_error(open(c"/dev/null".as_ptr(), 0) as isize, DENIED, 34);
        expect_error(
            open(c"sandbox-write-probe".as_ptr(), 0x201, 0o600i32) as isize,
            DENIED,
            35,
        );

        let fd = socket(2, 1, 0);
        if fd >= 0 {
            // Darwin sockaddr_in with a zero port and IPv4 loopback address.
            let address = [16u8, 2, 0, 0, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
            expect_error(
                bind(fd, address.as_ptr(), address.len() as u32) as isize,
                DENIED,
                36,
            );
            close(fd);
        } else {
            expect_error(fd as isize, DENIED, 37);
        }
        let child = fork();
        if child == 0 {
            _exit(0);
        }
        expect_error(child as isize, DENIED, 38);
        expect_error(read(1, (&raw mut byte).cast(), 1), BAD_FD, 39);
        if grants & 1 != 0 {
            if write(1, b"output\n".as_ptr(), 7) != 7 {
                _exit(40);
            }
        } else {
            expect_error(write(1, b"x".as_ptr(), 1), BAD_FD, 41);
        }
    }
}
