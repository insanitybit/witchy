//! Linux Exec process limits and runner-liveness checks.
use super::global::Global;
use super::linux_stat::Dirent;
use core::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub(super) struct Limit {
    current: u64,
    maximum: u64,
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    returned: i16,
}
unsafe extern "C" {
    fn opendir(path: *const c_char) -> *mut c_void;
    fn readdir(dir: *mut c_void) -> *const Dirent;
    fn dirfd(dir: *mut c_void) -> c_int;
    fn closedir(dir: *mut c_void) -> c_int;
    fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn read(fd: c_int, out: *mut c_void, len: usize) -> isize;
    safe fn close(fd: c_int) -> c_int;
    safe fn getuid() -> u32;
    fn getrlimit(resource: c_int, limit: *mut Limit) -> c_int;
    fn setrlimit(resource: c_int, limit: *const Limit) -> c_int;
    fn prctl(option: c_int, ...) -> c_int;
    fn syscall(number: core::ffi::c_long, ...) -> core::ffi::c_long;
}

static PROCESS_LIMIT: Global<u32> = Global::new(256);
static ALIVE_READ: Global<c_int> = Global::new(-1);
static ALIVE_WRITE: Global<c_int> = Global::new(-1);

pub(super) fn set_liveness(read: c_int, write: c_int) {
    ALIVE_READ.set(read);
    ALIVE_WRITE.set(write);
}

pub(super) fn prepare_program(limit: &Limit) -> bool {
    let (read, write) = (ALIVE_READ.get(), ALIVE_WRITE.get());
    prepare_child(read, write, limit)
}
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_process_limit(limit: u32) {
    PROCESS_LIMIT.set(limit);
}

fn decimal(input: &[u8]) -> Option<u64> {
    let input = input.trim_ascii_start();
    let digits = input.iter().take_while(|b| b.is_ascii_digit());
    let mut n = 0u64;
    let mut found = false;
    for &d in digits {
        n = n.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
        found = true;
    }
    found.then_some(n)
}
fn process_status(text: &[u8], uid: u32) -> Option<u64> {
    let mut owner = None;
    let mut threads = None;
    for line in text.split(|&b| b == b'\n') {
        if let Some(value) = line.strip_prefix(b"Uid:") {
            owner = decimal(value);
        }
        if let Some(value) = line.strip_prefix(b"Threads:") {
            threads = decimal(value);
        }
    }
    (owner == Some(u64::from(uid))).then_some(threads?)
}
fn user_processes() -> u64 {
    let dir = unsafe { opendir(c"/proc".as_ptr()) };
    if dir.is_null() {
        return 0;
    }
    let uid = getuid();
    let mut total = 0u64;
    loop {
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            break;
        }
        let name =
            unsafe { core::ffi::CStr::from_ptr((&raw const (*entry).name).cast()) }.to_bytes();
        if name.is_empty() || !name.iter().all(u8::is_ascii_digit) || name.len() > 255 {
            continue;
        }
        let mut path = [0u8; 264];
        path[..name.len()].copy_from_slice(name);
        path[name.len()..name.len() + 7].copy_from_slice(b"/status");
        let fd = unsafe { openat(dirfd(dir), path.as_ptr().cast(), 0o2000000) };
        if fd < 0 {
            continue;
        }
        let mut bytes = [0u8; 4096];
        let count = unsafe { read(fd, bytes.as_mut_ptr().cast(), bytes.len() - 1) };
        close(fd);
        if count > 0 {
            if let Some(n) = process_status(&bytes[..count as usize], uid) {
                total = total.saturating_add(n);
            }
        }
    }
    unsafe {
        closedir(dir);
    }
    total
}

pub(super) fn current_limit() -> Limit {
    let mut amount = user_processes().saturating_add(u64::from(PROCESS_LIMIT.get()));
    let mut hard = Limit {
        current: 0,
        maximum: 0,
    };
    if unsafe { getrlimit(6, &raw mut hard) } == 0 && hard.maximum != u64::MAX {
        amount = amount.min(hard.maximum);
    }
    Limit {
        current: amount,
        maximum: amount,
    }
}

/// Called only in the just-forked program. The runner keeps the sole write end
/// of the liveness pipe, so a dead runner cannot leave a program running.
pub(super) fn prepare_child(alive_read: c_int, alive_write: c_int, limit: &Limit) -> bool {
    if alive_read < 0 || alive_write < 0 {
        return false;
    }
    close(alive_write);
    let mut poll = PollFd {
        fd: alive_read,
        events: 1,
        returned: 0,
    };
    let zero = super::clock_random::Timespec {
        seconds: 0,
        nanos: 0,
    };
    (unsafe {
        prctl(1, 9, 0, 0, 0) == 0
            && syscall(
                73,
                &raw mut poll,
                1usize,
                &raw const zero,
                core::ptr::null::<c_void>(),
                0usize,
            ) == 0
            && setrlimit(6, limit) == 0
    }) && super::exec_linux_security::drop_capabilities()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_only_real_uid_and_thread_count() {
        let status = b"Name:\texample\nUid:\t1000\t1001\t1001\t1001\nThreads:\t7\n";
        assert_eq!(process_status(status, 1000), Some(7));
        assert_eq!(process_status(status, 1001), None);
        assert_eq!(process_status(b"Uid: 1000\n", 1000), None);
        assert!(user_processes() > 0);
        assert!(current_limit().current > 0);
    }
}
