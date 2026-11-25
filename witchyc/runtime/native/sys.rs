//! The C library calls that take a buffer, a path or a place for a result, behind
//! signatures that say so. Each wrapper is the one `unsafe` use of its call.
//! Not every build uses every wrapper.
#![allow(dead_code)]

use core::ffi::{CStr, c_char, c_int, c_void};

mod c {
    use super::{c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn read(fd: c_int, out: *mut c_void, len: usize) -> isize;
        pub fn write(fd: c_int, bytes: *const c_void, len: usize) -> isize;
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn unlinkat(fd: c_int, name: *const c_char, flags: c_int) -> c_int;
        pub fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        pub fn pipe(fds: *mut c_int) -> c_int;
        pub fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
    }
}

/// Reads at most `out.len()` bytes. The count, or a negative value with `errno` set.
pub fn read(fd: c_int, out: &mut [u8]) -> isize {
    // SAFETY: `out` is writable for its length.
    unsafe { c::read(fd, out.as_mut_ptr().cast(), out.len()) }
}

/// Writes at most `bytes.len()` bytes. The count, or a negative value with `errno` set.
pub fn write(fd: c_int, bytes: &[u8]) -> isize {
    // SAFETY: `bytes` is readable for its length.
    unsafe { c::write(fd, bytes.as_ptr().cast(), bytes.len()) }
}

/// Opens an existing object. Were `flags` to ask for one to be made, it is
/// made with no permissions.
pub fn open(path: &CStr, flags: c_int) -> c_int {
    // SAFETY: `path` is a C string, and the call keeps nothing of it. A mode is
    // always passed, so the call never reads an argument that is not there.
    unsafe { c::open(path.as_ptr(), flags, 0 as c_int) }
}

/// Opens `path` under the directory `fd`, as `open` does.
pub fn openat(fd: c_int, path: &CStr, flags: c_int) -> c_int {
    openat_mode(fd, path, flags, 0)
}

/// As `openat`, with the mode an object the call makes is given.
pub fn openat_mode(fd: c_int, path: &CStr, flags: c_int, mode: c_int) -> c_int {
    // SAFETY: as `open`.
    unsafe { c::openat(fd, path.as_ptr(), flags, mode) }
}

pub fn unlinkat(fd: c_int, name: &CStr, flags: c_int) -> c_int {
    // SAFETY: `name` is a C string, and the call keeps nothing of it.
    unsafe { c::unlinkat(fd, name.as_ptr(), flags) }
}

/// Waits for `pid` and gives its status when one is asked for.
pub fn waitpid(pid: c_int, status: Option<&mut c_int>, options: c_int) -> c_int {
    let status = status.map_or(core::ptr::null_mut(), |status| status as *mut c_int);
    // SAFETY: `status` is null or one writable `c_int`.
    unsafe { c::waitpid(pid, status, options) }
}

pub fn pipe(fds: &mut [c_int; 2]) -> c_int {
    // SAFETY: the call writes two descriptors.
    unsafe { c::pipe(fds.as_mut_ptr()) }
}

pub fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: &mut [c_int; 2]) -> c_int {
    // SAFETY: the call writes two descriptors.
    unsafe { c::socketpair(domain, kind, protocol, fds.as_mut_ptr()) }
}
