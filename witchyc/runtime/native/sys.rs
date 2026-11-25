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

mod c_directory {
    use super::{c_char, c_int};

    unsafe extern "C" {
        pub fn mkdirat(parent: c_int, name: *const c_char, mode: u16) -> c_int;
        pub fn renameat(from: c_int, source: *const c_char, to: c_int, target: *const c_char) -> c_int;
        #[cfg(target_os = "macos")]
        pub fn renameatx_np(
            from: c_int,
            source: *const c_char,
            to: c_int,
            target: *const c_char,
            flags: u32,
        ) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
        #[cfg(target_os = "linux")]
        pub fn getrandom(out: *mut super::c_void, len: usize, flags: u32) -> isize;
    }
}

/// Makes the directory `name` under the directory `parent`.
pub fn mkdirat(parent: c_int, name: &CStr, mode: u16) -> c_int {
    // SAFETY: `name` is a C string, and the call keeps nothing of it.
    unsafe { c_directory::mkdirat(parent, name.as_ptr(), mode) }
}

/// Moves `source` under `from` to `target` under `to`, replacing what is there.
pub fn renameat(from: c_int, source: &CStr, to: c_int, target: &CStr) -> c_int {
    // SAFETY: both names are C strings, and the call keeps nothing of them.
    unsafe { c_directory::renameat(from, source.as_ptr(), to, target.as_ptr()) }
}

/// As `renameat`, under the kernel's `flags` for an exclusive move or a swap.
#[cfg(target_os = "macos")]
pub fn renameatx_np(from: c_int, source: &CStr, to: c_int, target: &CStr, flags: u32) -> c_int {
    // SAFETY: as `renameat`.
    unsafe { c_directory::renameatx_np(from, source.as_ptr(), to, target.as_ptr(), flags) }
}

/// As `renameat`, under the kernel's `flags` for an exclusive move or a swap.
/// The call number is AArch64's, so no other machine has this function.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn renameat2(
    from: c_int,
    source: &CStr,
    to: c_int,
    target: &CStr,
    flags: u32,
) -> core::ffi::c_long {
    // SAFETY: as `renameat`. The call takes these five arguments.
    unsafe { c_directory::syscall(276, from, source.as_ptr(), to, target.as_ptr(), flags) }
}

/// Fills at most `out.len()` bytes with random ones. The count, or a negative
/// value with `errno` set.
#[cfg(target_os = "linux")]
pub fn getrandom(out: &mut [u8], flags: u32) -> isize {
    // SAFETY: `out` is writable for its length.
    unsafe { c_directory::getrandom(out.as_mut_ptr().cast(), out.len(), flags) }
}

// The calls the Exec files make: waiting, polling, time, and three descriptor
// settings that take only numbers.

/// A time or a length of time, as the C library lays it out.
#[repr(C)]
pub struct Timespec {
    pub seconds: i64,
    pub nanos: i64,
}

/// One descriptor for `poll`, with the events asked for and those returned.
#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub returned: i16,
}

/// What `waitid` reports about a child.
#[repr(C)]
#[cfg(target_os = "linux")]
pub struct SignalInfo {
    signo: i32,
    error: i32,
    pub code: i32,
    pad: i32,
    pub pid: i32,
    uid: u32,
    pub status: i32,
    rest: [u8; 100],
}

/// What `waitid` reports about a child.
#[repr(C)]
#[cfg(target_os = "macos")]
pub struct SignalInfo {
    signal: c_int,
    error: c_int,
    code: c_int,
    pub pid: c_int,
    uid: u32,
    status: c_int,
    address: *mut c_void,
    value: u64,
    band: i64,
    reserved: [u64; 7],
}

impl SignalInfo {
    /// Every field zero, as before a `waitid` that may report nothing.
    #[cfg(target_os = "linux")]
    pub const fn zeroed() -> Self {
        Self {
            signo: 0,
            error: 0,
            code: 0,
            pad: 0,
            pid: 0,
            uid: 0,
            status: 0,
            rest: [0; 100],
        }
    }

    /// Every field zero, as before a `waitid` that may report nothing.
    #[cfg(target_os = "macos")]
    pub const fn zeroed() -> Self {
        Self {
            signal: 0,
            error: 0,
            code: 0,
            pid: 0,
            uid: 0,
            status: 0,
            address: core::ptr::null_mut(),
            value: 0,
            band: 0,
            reserved: [0; 7],
        }
    }
}

mod exec_c {
    use super::{PollFd, SignalInfo, Timespec, c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn pread(fd: c_int, out: *mut c_void, len: usize, offset: i64) -> isize;
        pub fn send(fd: c_int, bytes: *const c_void, len: usize, flags: c_int) -> isize;
        #[cfg(target_os = "macos")]
        pub fn poll(fds: *mut PollFd, count: u32, timeout: c_int) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
        pub fn waitid(kind: c_int, id: u32, info: *mut SignalInfo, options: c_int) -> c_int;
        pub fn clock_gettime(clock: c_int, out: *mut Timespec) -> c_int;
        pub fn nanosleep(time: *const Timespec, remaining: *mut Timespec) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
        pub fn rmdir(path: *const c_char) -> c_int;
        pub fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    }
}

/// Reads at most `out.len()` bytes from `offset`, leaving the descriptor's place alone.
pub fn pread(fd: c_int, out: &mut [u8], offset: i64) -> isize {
    // SAFETY: `out` is writable for its length.
    unsafe { exec_c::pread(fd, out.as_mut_ptr().cast(), out.len(), offset) }
}

/// Sends at most `bytes.len()` bytes on a socket.
pub fn send(fd: c_int, bytes: &[u8], flags: c_int) -> isize {
    // SAFETY: `bytes` is readable for its length.
    unsafe { exec_c::send(fd, bytes.as_ptr().cast(), bytes.len(), flags) }
}

/// Waits `timeout` milliseconds for an event on any of `fds`.
pub fn poll(fds: &mut [PollFd], timeout: c_int) -> c_int {
    // SAFETY: `fds` is writable for its length, which is the count passed.
    unsafe { exec_c::poll(fds.as_mut_ptr(), fds.len() as _, timeout) }
}

/// Asks about the children `kind` and `id` name, and fills `info` when one has changed.
pub fn waitid(kind: c_int, id: u32, info: &mut SignalInfo, options: c_int) -> c_int {
    // SAFETY: `info` is one writable record of the layout the call fills.
    unsafe { exec_c::waitid(kind, id, info, options) }
}

pub fn clock_gettime(clock: c_int, out: &mut Timespec) -> c_int {
    // SAFETY: `out` is one writable time.
    unsafe { exec_c::clock_gettime(clock, out) }
}

/// Sleeps for `time`, and does not report what was left when a signal ends it early.
pub fn nanosleep(time: &Timespec) -> c_int {
    // SAFETY: `time` is readable, and a null remainder is allowed.
    unsafe { exec_c::nanosleep(time, core::ptr::null_mut()) }
}

#[cfg(target_os = "linux")]
pub fn pipe2(fds: &mut [c_int; 2], flags: c_int) -> c_int {
    // SAFETY: the call writes two descriptors.
    unsafe { exec_c::pipe2(fds.as_mut_ptr(), flags) }
}

pub fn rmdir(path: &CStr) -> c_int {
    // SAFETY: `path` is a C string, and the call keeps nothing of it.
    unsafe { exec_c::rmdir(path.as_ptr()) }
}

/// `fcntl(fd, F_SETFD, FD_CLOEXEC)`.
pub fn set_close_on_exec(fd: c_int) -> c_int {
    // SAFETY: this command takes a number, not a pointer.
    unsafe { exec_c::fcntl(fd, 2, 1 as c_int) }
}

/// `fcntl(fd, F_SETFL, flags)`.
pub fn set_status_flags(fd: c_int, flags: c_int) -> c_int {
    // SAFETY: this command takes a number, not a pointer.
    unsafe { exec_c::fcntl(fd, 4, flags) }
}

/// `fcntl(fd, F_DUPFD_CLOEXEC, least)`: a close-on-exec copy of `fd` numbered `least` or above.
pub fn duplicate_close_on_exec(fd: c_int, least: c_int) -> c_int {
    #[cfg(target_os = "macos")]
    const DUP_CLOEXEC: c_int = 67;
    #[cfg(target_os = "linux")]
    const DUP_CLOEXEC: c_int = 1030;
    // SAFETY: this command takes a number, not a pointer.
    unsafe { exec_c::fcntl(fd, DUP_CLOEXEC, least) }
}
