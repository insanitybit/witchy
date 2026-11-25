#![no_std]

use core::ffi::{c_int, c_long, c_void};

#[unsafe(no_mangle)]
static witchy_entry_grants: u64 =
    cfg!(console) as u64 | ((cfg!(allocator) as u64) << 1) | ((cfg!(args) as u64) << 6);

#[unsafe(no_mangle)]
#[cfg(not(valid_manifest))]
static witchy_launch_plan: [u8; 2] = [cfg!(manifest) as u8, 0];

#[unsafe(no_mangle)]
#[cfg(valid_manifest)]
static witchy_launch_plan: [u8; VALID_PLAN.len()] = *VALID_PLAN;
#[cfg(valid_manifest)]
const VALID_PLAN: &[u8; 98] = b"witchy-launch-plan\t1\t0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\noutput\nend\n\0";

// An empty secret-slot table, as emitted for guests without fixed secrets.
#[unsafe(no_mangle)]
static witchy_secret_slots: u8 = 0;

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn __errno_location() -> *const c_int;
    fn _exit(code: c_int) -> !;
    fn write(fd: c_int, bytes: *const u8, length: usize) -> isize;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

fn denied(result: c_long, code: c_int) {
    if result != -1 || unsafe { *__errno_location() } != 1 {
        unsafe { _exit(code) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_entry(grants: u64) {
    const OPEN_AT: c_long = 56;
    const SOCKET: c_long = 198;
    const GET_PID: c_long = 172;
    const WRITE: c_long = 64;
    const READ: c_long = 63;
    const CLONE: c_long = 220;
    const MMAP: c_long = 222;
    const MPROTECT: c_long = 226;
    const BRK: c_long = 214;
    const PRIVATE: c_int = 2;
    const ANONYMOUS: c_int = 0x20;
    const RW: c_int = 3;
    const RX: c_int = 5;
    const RWX: c_int = 7;
    let mut byte = 0u8;
    let marker = b"x";
    let output = b"output\n";
    // Run through the normal guest entry after the launcher confines us.
    unsafe {
        denied(
            syscall(OPEN_AT, -100i32, c"/dev/null".as_ptr(), 0i32, 0i32),
            10,
        );
        denied(syscall(SOCKET, 2i32, 1i32, 0i32), 11);
        denied(syscall(GET_PID), 12);
        denied(syscall(WRITE, 2i32, marker.as_ptr(), marker.len()), 13);
        denied(syscall(READ, 0i32, &raw mut byte, 1usize), 14);
        denied(syscall(CLONE, 0i64, 0i64, 0i64, 0i64, 0i64), 15);
        if grants & 1 != 0 {
            if write(1, output.as_ptr(), output.len()) != output.len() as isize {
                _exit(16);
            }
        } else {
            denied(syscall(WRITE, 1i32, marker.as_ptr(), marker.len()), 17);
        }
        denied(syscall(MMAP, 0i64, 4096usize, RW, PRIVATE, 1i32, 0i64), 18);
        denied(
            syscall(MMAP, 0i64, 4096usize, RWX, PRIVATE | ANONYMOUS, -1i32, 0i64),
            19,
        );
        if grants & 2 != 0 {
            let page = mmap(core::ptr::null_mut(), 4096, RW, PRIVATE | ANONYMOUS, -1, 0);
            if page.addr() == usize::MAX {
                _exit(20);
            }
            denied(syscall(MPROTECT, page, 4096usize, RX), 21);
            if munmap(page, 4096) != 0 {
                _exit(22);
            }
        } else {
            denied(
                syscall(MMAP, 0i64, 4096usize, RW, PRIVATE | ANONYMOUS, -1i32, 0i64),
                23,
            );
            denied(syscall(BRK, 0i64), 24);
        }
    }
}
