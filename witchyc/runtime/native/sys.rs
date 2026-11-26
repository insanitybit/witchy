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
#[repr(C, align(8))]
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

// Sockets, descriptor flags and signal dispositions.

#[cfg(target_os = "macos")]
type PollCount = u32;
#[cfg(target_os = "linux")]
type PollCount = usize;

mod net {
    use super::{PollCount, PollFd, c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn poll(fds: *mut PollFd, count: PollCount, timeout: c_int) -> c_int;
        pub fn setsockopt(
            fd: c_int,
            level: c_int,
            option: c_int,
            value: *const c_void,
            len: u32,
        ) -> c_int;
        pub fn getsockopt(
            fd: c_int,
            level: c_int,
            option: c_int,
            value: *mut c_void,
            len: *mut u32,
        ) -> c_int;
        pub fn accept(fd: c_int, address: *mut c_void, length: *mut u32) -> c_int;
        pub fn bind(fd: c_int, address: *const c_void, len: u32) -> c_int;
        pub fn connect(fd: c_int, address: *const c_void, length: u32) -> c_int;
        pub fn getsockname(fd: c_int, address: *mut c_void, len: *mut u32) -> c_int;
        pub fn send(fd: c_int, bytes: *const c_void, len: usize, flags: c_int) -> isize;
        pub fn recv(fd: c_int, bytes: *mut u8, len: usize, flags: c_int) -> isize;
        pub fn inet_pton(family: c_int, text: *const c_char, address: *mut c_void) -> c_int;
        pub fn fcntl(fd: c_int, op: c_int, ...) -> c_int;
        pub fn signal(signal: c_int, handler: usize) -> usize;
        pub fn atexit(callback: extern "C" fn()) -> c_int;
    }
}

/// Sets a socket option whose value is all of `value`.
pub fn setsockopt<T>(fd: c_int, level: c_int, option: c_int, value: &T) -> c_int {
    let len = size_of::<T>() as u32;
    // SAFETY: the call reads `len` bytes of `value` and keeps nothing of it.
    unsafe { net::setsockopt(fd, level, option, core::ptr::from_ref(value).cast(), len) }
}

/// Reads a socket option whose value is one `c_int`.
pub fn getsockopt_int(fd: c_int, level: c_int, option: c_int, value: &mut c_int) -> c_int {
    let mut len = size_of::<c_int>() as u32;
    // SAFETY: the call writes at most `len` bytes, the size of `value`.
    unsafe { net::getsockopt(fd, level, option, core::ptr::from_mut(value).cast(), &mut len) }
}

/// Accepts a connection, without asking who the peer is.
pub fn accept(fd: c_int) -> c_int {
    // SAFETY: a null address and length ask the call to write neither.
    unsafe { net::accept(fd, core::ptr::null_mut(), core::ptr::null_mut()) }
}

/// Binds to the socket address that is all of `address`.
pub fn bind(fd: c_int, address: &[u8]) -> c_int {
    // SAFETY: the call copies `address.len()` bytes in and checks them itself.
    unsafe { net::bind(fd, address.as_ptr().cast(), address.len() as u32) }
}

/// Connects to the socket address that is all of `address`.
pub fn connect(fd: c_int, address: &[u8]) -> c_int {
    // SAFETY: as `bind`.
    unsafe { net::connect(fd, address.as_ptr().cast(), address.len() as u32) }
}

/// Writes the socket's own address to `address`, and its length to `length`.
/// An address longer than `address` is cut short there.
pub fn getsockname(fd: c_int, address: &mut [u8], length: &mut u32) -> c_int {
    *length = address.len() as u32;
    // SAFETY: the call writes at most `*length` bytes, which `address` has.
    unsafe { net::getsockname(fd, address.as_mut_ptr().cast(), length) }
}

/// Receives at most `out.len()` bytes. The count, or a negative value with `errno` set.
pub fn recv(fd: c_int, out: &mut [u8], flags: c_int) -> isize {
    // SAFETY: `out` is writable for its length.
    unsafe { net::recv(fd, out.as_mut_ptr(), out.len(), flags) }
}

/// Parses a dotted IPv4 address. 1 when `text` is one.
pub fn inet_pton_v4(text: &CStr, out: &mut [u8; 4]) -> c_int {
    const AF_INET: c_int = 2;
    // SAFETY: `text` is a C string, and the call writes the four bytes of an IPv4 address.
    unsafe { net::inet_pton(AF_INET, text.as_ptr(), out.as_mut_ptr().cast()) }
}

/// Parses an IPv6 address. 1 when `text` is one.
pub fn inet_pton_v6(text: &CStr, out: &mut [u8; 16]) -> c_int {
    #[cfg(target_os = "macos")]
    const AF_INET6: c_int = 30;
    #[cfg(target_os = "linux")]
    const AF_INET6: c_int = 10;
    // SAFETY: `text` is a C string, and the call writes the sixteen bytes of an IPv6 address.
    unsafe { net::inet_pton(AF_INET6, text.as_ptr(), out.as_mut_ptr().cast()) }
}

/// The descriptor's status flags (`F_GETFL`), or a negative value.
pub fn status_flags(fd: c_int) -> c_int {
    const F_GETFL: c_int = 3;
    // SAFETY: this operation takes no further argument.
    unsafe { net::fcntl(fd, F_GETFL) }
}

/// Replaces the descriptor's own flags (`F_SETFD`); 1 is close-on-exec.
pub fn set_descriptor_flags(fd: c_int, flags: c_int) -> c_int {
    const F_SETFD: c_int = 2;
    // SAFETY: this operation reads one integer.
    unsafe { net::fcntl(fd, F_SETFD, flags) }
}

/// Has the process ignore `signal`. False when the C library refuses.
pub fn ignore_signal(signal: c_int) -> bool {
    const SIG_IGN: usize = 1;
    const SIG_ERR: usize = usize::MAX;
    // SAFETY: no handler is installed, so no code is made to run.
    unsafe { net::signal(signal, SIG_IGN) != SIG_ERR }
}

/// Gives `signal` its default action. False when the C library refuses.
pub fn default_signal(signal: c_int) -> bool {
    const SIG_DFL: usize = 0;
    const SIG_ERR: usize = usize::MAX;
    // SAFETY: as `ignore_signal`.
    unsafe { net::signal(signal, SIG_DFL) != SIG_ERR }
}

/// Runs `callback` when the process exits normally. 0 when it was registered.
pub fn atexit(callback: extern "C" fn()) -> c_int {
    // SAFETY: `callback` is a safe function with the signature the call expects.
    unsafe { net::atexit(callback) }
}

mod c_stream {
    use super::{c_char, c_int, c_void};

    unsafe extern "C" {
        #[cfg_attr(target_os = "macos", link_name = "__stdoutp")]
        #[cfg_attr(target_os = "linux", link_name = "stdout")]
        pub static mut OUT: *mut c_void;
        #[cfg_attr(target_os = "macos", link_name = "__stderrp")]
        #[cfg_attr(target_os = "linux", link_name = "stderr")]
        pub static mut ERR: *mut c_void;
        pub fn fputs(text: *const c_char, stream: *mut c_void) -> c_int;
        pub fn perror(message: *const c_char);
        pub fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    }
}

/// One of the C runtime's two output streams. The only ways to get one are
/// `stdout` and `stderr`, so a `Stream` is always a stream the C runtime
/// keeps open for the life of the process.
#[derive(Clone, Copy)]
pub struct Stream(*mut c_void);

impl Stream {
    pub fn stdout() -> Self {
        // SAFETY: the C runtime sets the variable before `main`, and the
        // runtime's one thread never writes it.
        Self(unsafe { c_stream::OUT })
    }

    pub fn stderr() -> Self {
        // SAFETY: as `stdout`.
        Self(unsafe { c_stream::ERR })
    }

    /// Writes the text as `fputs` does. A failure is not reported.
    pub fn put(self, text: &CStr) {
        // SAFETY: `text` is a C string, and the stream is one of the C
        // runtime's own.
        unsafe { c_stream::fputs(text.as_ptr(), self.0) };
    }

    /// The `FILE *`, for a C function that takes one.
    pub fn as_ptr(self) -> *mut c_void {
        self.0
    }
}

/// Writes `message` and the text of the current `errno` to the C runtime's
/// error stream.
pub fn perror(message: &CStr) {
    // SAFETY: `message` is a C string, and the call keeps nothing of it.
    unsafe { c_stream::perror(message.as_ptr()) }
}

#[cfg(target_os = "macos")]
const F_DUPFD_CLOEXEC: c_int = 67;
#[cfg(target_os = "linux")]
const F_DUPFD_CLOEXEC: c_int = 1030;

// Each `fcntl` command below takes no argument or an integer, so it is safe for
// every descriptor number: one that is not open gives a negative result.

/// The descriptor's own flags (`F_GETFD`), or a negative value when it is not open.
pub fn descriptor_flags(fd: c_int) -> c_int {
    // SAFETY: the command reads no argument.
    unsafe { c_stream::fcntl(fd, 1) }
}

// What the Exec broker and its Linux runner ask of a process: a working
// directory, a private directory's name, resource limits, and the Linux
// calls that take only numbers.

/// A resource's limit now and the most it may be raised to.
#[repr(C)]
pub struct ResourceLimit {
    pub current: u64,
    pub maximum: u64,
}

// Keep these values in sync with the C ABI probes in tests/fixtures/native-sandbox.
const _: () = {
    use core::mem::{align_of, offset_of, size_of};

    assert!(size_of::<PollFd>() == 8 && align_of::<PollFd>() == 4);
    assert!(offset_of!(PollFd, fd) == 0);
    assert!(offset_of!(PollFd, events) == 4);
    assert!(offset_of!(PollFd, returned) == 6);
    assert!(size_of::<Timespec>() == 16 && align_of::<Timespec>() == 8);
    assert!(offset_of!(Timespec, nanos) == 8);
    assert!(size_of::<ResourceLimit>() == 16 && align_of::<ResourceLimit>() == 8);
    assert!(offset_of!(ResourceLimit, maximum) == 8);
    #[cfg(target_os = "macos")]
    {
        assert!(size_of::<SignalInfo>() == 104 && align_of::<SignalInfo>() == 8);
        assert!(offset_of!(SignalInfo, pid) == 12);
        assert!(offset_of!(SignalInfo, status) == 20);
    }
    #[cfg(target_os = "linux")]
    {
        assert!(size_of::<SignalInfo>() == 128 && align_of::<SignalInfo>() == 8);
        assert!(offset_of!(SignalInfo, pid) == 16);
        assert!(offset_of!(SignalInfo, status) == 24);
    }
};

mod c_process {
    use super::{ResourceLimit, c_char, c_int};

    unsafe extern "C" {
        pub fn chdir(path: *const c_char) -> c_int;
        pub fn mkdtemp(template: *mut c_char) -> *mut c_char;
        #[cfg(target_os = "macos")]
        pub fn confstr(name: c_int, buffer: *mut c_char, len: usize) -> usize;
        pub fn getrlimit(resource: c_int, limit: *mut ResourceLimit) -> c_int;
        pub fn setrlimit(resource: c_int, limit: *const ResourceLimit) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn prctl(option: c_int, ...) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
    }
}

pub fn chdir(path: &CStr) -> c_int {
    // SAFETY: `path` is a C string, and the call keeps nothing of it.
    unsafe { c_process::chdir(path.as_ptr()) }
}

/// Makes a private directory named by the C string at the front of
/// `template`, whose last letters the call replaces. False when none was
/// made, or when `template` holds no zero and so no C string.
pub fn mkdtemp(template: &mut [u8]) -> bool {
    if !template.contains(&0) {
        return false;
    }
    // SAFETY: a C string ends inside `template`, and the call writes only
    // letters before that end.
    !unsafe { c_process::mkdtemp(template.as_mut_ptr().cast()) }.is_null()
}

/// Writes the system's text for `name` to `out`, cut short to fit, and gives
/// the length the whole text needs, or 0 when there is none.
#[cfg(target_os = "macos")]
pub fn confstr(name: c_int, out: &mut [u8]) -> usize {
    // SAFETY: the call writes at most `out.len()` bytes.
    unsafe { c_process::confstr(name, out.as_mut_ptr().cast(), out.len()) }
}

pub fn getrlimit(resource: c_int, limit: &mut ResourceLimit) -> c_int {
    // SAFETY: the call writes one limit.
    unsafe { c_process::getrlimit(resource, limit) }
}

pub fn setrlimit(resource: c_int, limit: &ResourceLimit) -> c_int {
    // SAFETY: the call reads one limit.
    unsafe { c_process::setrlimit(resource, limit) }
}

// Each `prctl` below passes numbers only, as the `unsigned long` the C library
// reads its variable arguments as.

/// `prctl(PR_SET_PDEATHSIG, signal)`: sends `signal` when the parent ends.
#[cfg(target_os = "linux")]
pub fn set_parent_death_signal(signal: c_int) -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(1, signal as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_SET_DUMPABLE, 0)`.
#[cfg(target_os = "linux")]
pub fn clear_dumpable() -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(4, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_CAPBSET_DROP, capability)`.
#[cfg(target_os = "linux")]
pub fn drop_bounding_capability(capability: c_int) -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(24, capability as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_SET_SECUREBITS, bits)`.
#[cfg(target_os = "linux")]
pub fn set_securebits(bits: c_int) -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(28, bits as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_SET_CHILD_SUBREAPER, 1)`.
#[cfg(target_os = "linux")]
pub fn set_child_subreaper() -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(36, 1 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_SET_NO_NEW_PRIVS, 1)`.
#[cfg(target_os = "linux")]
pub fn set_no_new_privileges() -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(38, 1 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL)`.
#[cfg(target_os = "linux")]
pub fn clear_ambient_capabilities() -> c_int {
    // SAFETY: the option reads numbers only.
    unsafe { c_process::prctl(47, 4 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong, 0 as core::ffi::c_ulong) }
}

/// `pidfd_open(pid, 0)`: a descriptor for the process, or a negative value.
#[cfg(target_os = "linux")]
pub fn pidfd_open(pid: c_int) -> c_int {
    // SAFETY: the call reads two numbers.
    unsafe { c_process::syscall(434, pid, 0u32) as c_int }
}

/// `pidfd_send_signal(fd, signal, NULL, 0)`: signals the process `fd` names.
#[cfg(target_os = "linux")]
pub fn pidfd_send_signal(fd: c_int, signal: i32) -> core::ffi::c_long {
    // SAFETY: a null record asks for the signal's default one, and the rest are numbers.
    unsafe { c_process::syscall(424, fd, signal, core::ptr::null::<c_void>(), 0u32) }
}

// Pages shared with forked children, the input stream's error mark, and a
// terminal's name.

mod c_shared {
    use super::{c_int, c_void};

    unsafe extern "C" {
        pub fn mmap(
            addr: *mut c_void,
            size: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        #[cfg_attr(target_os = "macos", link_name = "__stdinp")]
        #[cfg_attr(target_os = "linux", link_name = "stdin")]
        pub static mut IN: *mut c_void;
        pub fn ferror(stream: *mut c_void) -> c_int;
        #[cfg(target_os = "macos")]
        pub fn ttyname_r(fd: c_int, path: *mut super::c_char, len: usize) -> c_int;
    }
}

/// Maps `size` bytes of fresh zeroed pages, readable and writable, that this
/// process shares with the children it forks afterwards. `None` when the
/// kernel refuses. The pages stay mapped until someone unmaps them.
pub fn map_shared(size: usize) -> Option<core::ptr::NonNull<c_void>> {
    const READ_WRITE: c_int = 3;
    #[cfg(target_os = "macos")]
    const SHARED_ANONYMOUS: c_int = 0x1001;
    #[cfg(target_os = "linux")]
    const SHARED_ANONYMOUS: c_int = 0x21;
    // SAFETY: with no address asked for, the kernel picks a place nothing
    // else has, and an anonymous mapping reads no descriptor.
    let mapped =
        unsafe { c_shared::mmap(core::ptr::null_mut(), size, READ_WRITE, SHARED_ANONYMOUS, -1, 0) };
    if mapped.addr() == usize::MAX {
        return None;
    }
    core::ptr::NonNull::new(mapped)
}

/// Whether the C runtime's input stream has its error mark set.
pub fn stdin_failed() -> bool {
    // SAFETY: the C runtime sets the variable before `main` and keeps the
    // stream open, and the runtime's one thread never writes the variable.
    unsafe { c_shared::ferror(c_shared::IN) != 0 }
}

/// Writes the name of the terminal `fd` is, and the NUL after it, to `path`.
/// Zero on success.
#[cfg(target_os = "macos")]
pub fn ttyname_r(fd: c_int, path: &mut [u8]) -> c_int {
    // SAFETY: the call writes at most `path.len()` bytes to `path`.
    unsafe { c_shared::ttyname_r(fd, path.as_mut_ptr().cast(), path.len()) }
}

mod c_flush {
    use super::{c_int, c_void};

    unsafe extern "C" {
        pub fn fflush(stream: *mut c_void) -> c_int;
    }
}

impl Stream {
    /// Writes out what the stream holds, as `fflush` does. Negative when
    /// that fails.
    pub fn flush(self) -> c_int {
        // SAFETY: the stream is one of the C runtime's own.
        unsafe { c_flush::fflush(self.0) }
    }
}

/// One `u64` on a shared anonymous mapping of its own: this process and every
/// process forked after `map` read and write the same word. The mapping is
/// never removed here, so the word is readable and writable while the value
/// exists. It cannot be copied, so a process holds one way to the word.
/// The processes that share it are expected to take turns: nothing here
/// orders a write in one against a read in another.
pub struct SharedWord(core::ptr::NonNull<u64>);

impl SharedWord {
    /// Maps the word, which starts at zero. None when the kernel refuses.
    pub fn map() -> Option<Self> {
        map_shared(size_of::<u64>()).map(|word| Self(word.cast()))
    }

    pub fn get(&self) -> u64 {
        // SAFETY: the mapping holds one readable `u64` and is never removed.
        unsafe { self.0.read() }
    }

    pub fn set(&mut self, value: u64) {
        // SAFETY: the mapping holds one writable `u64` and is never removed.
        unsafe { self.0.write(value) }
    }

    /// The word itself, for code that counts a budget down in place.
    ///
    /// # Safety
    /// No other process reads or writes the word while the reference lives.
    pub unsafe fn as_mut(&mut self) -> &mut u64 {
        // SAFETY: the mapping holds one writable `u64`, is page-aligned and is
        // never removed; `self` is the only way to it in this process, and
        // the caller answers for the others.
        unsafe { self.0.as_mut() }
    }

    /// Gives up the value for the address of the word, which stays mapped.
    pub fn into_raw(self) -> *mut u64 {
        self.0.as_ptr()
    }
}

