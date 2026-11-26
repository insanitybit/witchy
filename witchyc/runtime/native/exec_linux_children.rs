//! Linux subreaper cleanup. Only this process's unreaped children are signalled,
//! and each signal is sent through a pidfd to prevent PID-reuse mistakes.
use super::global::{Global, State};
use super::sys::{self, Timespec};
use core::{
    ffi::{CStr, c_char, c_int, c_long, c_void},
    ptr,
};

const HELD: usize = 16384;
const SWEEP_MS: u64 = 5000;
static CHILDREN_FD: Global<c_int> = Global::new(-1);
static OWNER: Global<c_int> = Global::new(0);
// The sweeps below run on the one thread of a warden or a runner, never from
// a signal handler. Each use of these reads and signals processes; it starts
// none, and nothing it calls uses the same one again.
static INPUT: State<[u8; 65536]> = State::new([0; 65536]);
static PIDS: State<[c_int; HELD]> = State::new([0; HELD]);
static HANDLES: State<[c_int; HELD]> = State::new([-1; HELD]);

unsafe extern "C" {
    safe fn getpid() -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn snprintf(out: *mut c_char, size: usize, format: *const c_char, ...) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
}

fn now_ms() -> Option<u64> {
    let mut time = Timespec {
        seconds: 0,
        nanos: 0,
    };
    if sys::clock_gettime(1, &mut time) != 0 {
        return None;
    }
    if time.seconds < 0 || !(0..1_000_000_000).contains(&time.nanos) {
        return None;
    }
    Some(
        (time.seconds as u64)
            .saturating_mul(1000)
            .saturating_add(time.nanos as u64 / 1_000_000),
    )
}

/// Install subreaping before creating the runner or any program children.
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_supervise() -> bool {
    let pid = getpid();
    let mut path = [0u8; 64];
    let n = unsafe {
        snprintf(
            path.as_mut_ptr().cast(),
            path.len(),
            c"/proc/self/task/%d/children".as_ptr(),
            pid,
        )
    };
    if n <= 0 || n as usize >= path.len() || sys::set_child_subreaper() != 0 {
        return false;
    }
    // The text written above ends before the end of `path`, which began as zeros.
    let Ok(path) = CStr::from_bytes_until_nul(&path) else {
        return false;
    };
    let fd = sys::open(path, 0o2000000);
    if fd < 0 {
        return false;
    }
    if OWNER.get() == pid && CHILDREN_FD.get() >= 0 {
        close(CHILDREN_FD.get());
    }
    CHILDREN_FD.set(fd);
    OWNER.set(pid);
    true
}

pub(super) fn children_descriptor() -> c_int {
    if OWNER.get() == getpid() {
        CHILDREN_FD.get()
    } else {
        -1
    }
}

pub(super) fn discard_inherited() {
    if OWNER.get() != getpid() && CHILDREN_FD.get() >= 0 {
        close(CHILDREN_FD.get());
        CHILDREN_FD.set(-1);
        OWNER.set(0);
    }
}

// A near-full read could end in the middle of a PID. Discard that last token
// and force another sweep rather than interpreting a truncated PID.
fn read_children(fd: c_int, output: &mut [c_int]) -> Result<(usize, bool), ()> {
    INPUT.with(|input| {
        let wanted = input.len() - 1;
        let n = sys::pread(fd, &mut input[..wanted], 0);
        if n < 0 {
            return Err(());
        }
        let mut used = n as usize;
        let mut more = used >= input.len() - 64;
        if more {
            used = input[..used].iter().rposition(|&b| b == b' ').unwrap_or(0);
        }
        let mut count = 0;
        for part in input[..used]
            .split(|&b| b.is_ascii_whitespace())
            .filter(|p| !p.is_empty())
        {
            let mut pid = 0i32;
            for &digit in part {
                if !digit.is_ascii_digit() {
                    return Err(());
                }
                pid = pid
                    .checked_mul(10)
                    .and_then(|p| p.checked_add(i32::from(digit - b'0')))
                    .ok_or(())?;
            }
            if pid > 1 {
                if count == output.len() {
                    more = true;
                    break;
                }
                output[count] = pid;
                count += 1;
            }
        }
        Ok((count, more))
    })
}

/// Kill and reap this subreaper's direct children. Each round is bounded and
/// repeats until the kernel's child list is empty, including adopted children.
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_sweep_own() -> bool {
    let (fd, owner) = (CHILDREN_FD.get(), OWNER.get());
    if fd < 0 || owner != getpid() {
        return true;
    }
    let Some(start) = now_ms() else {
        return false;
    };
    let deadline = start.saturating_add(SWEEP_MS);
    loop {
        let round = PIDS.with(|pids| {
            let (count, more) = read_children(fd, pids)?;
            HANDLES.with(|handles| {
                let mut opened = 0;
                for &pid in &pids[..count] {
                    let handle = sys::pidfd_open(pid);
                    if handle >= 0 {
                        sys::pidfd_send_signal(handle, 19);
                        handles[opened] = handle;
                        opened += 1;
                    }
                }
                for &handle in &handles[..opened] {
                    sys::pidfd_send_signal(handle, 9);
                    close(handle);
                }
            });
            for &pid in &pids[..count] {
                sys::waitpid(pid, None, 1);
            }
            Ok::<_, ()>((count, more))
        });
        let Ok((count, more)) = round else {
            return false;
        };
        if count == 0 && !more {
            return true;
        }
        if now_ms().is_none_or(|now| now >= deadline) {
            return false;
        }
        let delay = Timespec {
            seconds: 0,
            nanos: 1_000_000,
        };
        sys::nanosleep(&delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_rejects_invalid_and_tracks_capacity() {
        // Exercise the actual proc-file parser against a temporary file.
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join(format!("witchy-pids-{}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"12 34 56 ").unwrap();
        drop(f);
        let f = std::fs::File::open(&path).unwrap();
        let mut out = [0; 2];
        assert_eq!(read_children(f.as_raw_fd(), &mut out), Ok((2, true)));
        assert_eq!(out, [12, 34]);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn subreaper_stops_its_own_child() {
        unsafe extern "C" {
            fn fork() -> c_int;
            safe fn pause() -> c_int;
            safe fn _exit(code: c_int) -> !;
        }
        let supervisor = unsafe { fork() };
        assert!(supervisor >= 0);
        if supervisor == 0 {
            if !witchy_exec_supervise() {
                _exit(1)
            }
            let child = unsafe { fork() };
            if child < 0 {
                _exit(2)
            }
            if child == 0 {
                pause();
                _exit(3)
            }
            if !witchy_exec_sweep_own() {
                _exit(4)
            }
            _exit(0)
        }
        let mut status = 0;
        assert_eq!(sys::waitpid(supervisor, Some(&mut status), 0), supervisor);
        assert_eq!(status, 0);
    }

    #[test]
    fn runner_stops_a_child_that_created_a_new_session() {
        unsafe extern "C" {
            fn fork() -> c_int;
            safe fn setsid() -> c_int;
            safe fn pause() -> c_int;
            safe fn _exit(code: c_int) -> !;
        }
        let supervisor = unsafe { fork() };
        assert!(supervisor >= 0);
        if supervisor == 0 {
            if !witchy_exec_supervise() {
                _exit(1)
            }
            let mut ready = [-1; 2];
            if sys::pipe(&mut ready) != 0 {
                _exit(2)
            }
            let leader = unsafe { fork() };
            if leader < 0 {
                _exit(3)
            }
            if leader == 0 {
                close(ready[0]);
                let child = unsafe { fork() };
                if child < 0 {
                    _exit(4)
                }
                if child == 0 {
                    if setsid() < 0 {
                        _exit(5)
                    }
                    if sys::write(ready[1], b"X") != 1 {
                        _exit(6)
                    }
                    pause();
                    _exit(7)
                }
                close(ready[1]);
                pause();
                _exit(8)
            }
            close(ready[1]);
            let mut marker = [0u8];
            if sys::read(ready[0], &mut marker) != 1 || marker[0] != b'X' {
                _exit(9)
            }
            close(ready[0]);
            if !sweep_descendants(leader) {
                _exit(10)
            }
            sys::waitpid(leader, None, 0);
            if !witchy_exec_sweep_own() {
                _exit(11)
            }
            _exit(0)
        }
        let mut status = 0;
        assert_eq!(sys::waitpid(supervisor, Some(&mut status), 0), supervisor);
        assert_eq!(status, 0);
    }
}

use super::linux_stat::Dirent;
unsafe extern "C" {
    fn fdopendir(fd: c_int) -> *mut c_void;
    fn readdir(dir: *mut c_void) -> *const Dirent;
    fn closedir(dir: *mut c_void) -> c_int;
    fn dirfd(dir: *mut c_void) -> c_int;
    fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
}
use super::sys::{PollFd, SignalInfo};
#[derive(Copy, Clone)]
struct Held {
    fd: c_int,
    pid: c_int,
    parent: c_int,
}
static TREE: State<[Held; HELD]> = State::new(
    [Held {
        fd: -1,
        pid: 0,
        parent: 0,
    }; HELD],
);
static BELOW: State<[c_int; HELD]> = State::new([0; HELD]);

fn list_children(parent: c_int, out: &mut [c_int]) -> (usize, bool) {
    let mut path = [0u8; 64];
    let n = unsafe {
        snprintf(
            path.as_mut_ptr().cast(),
            path.len(),
            c"/proc/%d/task".as_ptr(),
            parent,
        )
    };
    if n <= 0 || n as usize >= path.len() {
        return (0, true);
    }
    // The text written above ends before the end of `path`, which began as zeros.
    let Ok(path) = CStr::from_bytes_until_nul(&path) else {
        return (0, true);
    };
    let fd = sys::open(path, 0o2040000);
    if fd < 0 {
        let errno = super::errno::get();
        return (0, !matches!(errno, 2 | 3));
    }
    let dir = unsafe { fdopendir(fd) };
    if dir.is_null() {
        close(fd);
        return (0, true);
    }
    let mut count = 0;
    let mut more = false;
    loop {
        {
            super::errno::set(0);
        }
        let entry = unsafe { readdir(dir) };
        if entry.is_null() {
            more |= super::errno::get() != 0;
            break;
        }
        let name = unsafe { CStr::from_ptr((&raw const (*entry).name).cast()) }.to_bytes();
        if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let mut relative = [0u8; 300];
        if name.len() + 9 >= relative.len() {
            more = true;
            continue;
        }
        relative[..name.len()].copy_from_slice(name);
        relative[name.len()..name.len() + 9].copy_from_slice(b"/children");
        let child_fd = unsafe { openat(dirfd(dir), relative.as_ptr().cast(), 0o2000000) };
        if child_fd < 0 {
            // Threads can disappear while enumerating; other errors must not
            // make an incomplete process tree look empty.
            more |= !matches!(super::errno::get(), 2 | 3);
            continue;
        }
        match read_children(child_fd, &mut out[count..]) {
            Ok((n, truncated)) => {
                count += n;
                more |= truncated;
            }
            Err(()) => more = true,
        }
        close(child_fd);
    }
    unsafe {
        closedir(dir);
    }
    (count, more)
}

fn parent_of(pid: c_int) -> Option<c_int> {
    let mut path = [0u8; 64];
    let n = unsafe {
        snprintf(
            path.as_mut_ptr().cast(),
            path.len(),
            c"/proc/%d/stat".as_ptr(),
            pid,
        )
    };
    if n <= 0 || n as usize >= path.len() {
        return None;
    }
    // The text written above ends before the end of `path`, which began as zeros.
    let path = CStr::from_bytes_until_nul(&path).ok()?;
    let fd = sys::open(path, 0o2000000);
    if fd < 0 {
        return None;
    }
    let mut data = [0u8; 512];
    let n = sys::read(fd, &mut data);
    close(fd);
    if n <= 0 {
        return None;
    }
    let data = &data[..n as usize];
    let right = data.iter().rposition(|&b| b == b')')?;
    let mut fields = data[right + 1..]
        .split(|b| b.is_ascii_whitespace())
        .filter(|p| !p.is_empty());
    let state = fields.next()?;
    if state.len() != 1 {
        return None;
    }
    let mut parent = 0i32;
    for &digit in fields.next()? {
        if !digit.is_ascii_digit() {
            return None;
        }
        parent = parent
            .checked_mul(10)?
            .checked_add(i32::from(digit - b'0'))?;
    }
    Some(parent)
}

fn still_running(fd: c_int) -> bool {
    let mut probe = PollFd {
        fd,
        events: 1,
        returned: 0,
    };
    let zero = Timespec {
        seconds: 0,
        nanos: 0,
    };
    unsafe {
        syscall(
            73,
            &raw mut probe,
            1usize,
            &raw const zero,
            ptr::null::<c_void>(),
            0usize,
        ) == 0
    }
}

fn hold(
    pid: c_int,
    parent: c_int,
    parent_fd: c_int,
    tree: &mut [Held],
    count: &mut usize,
    more: &mut bool,
) {
    if *count == tree.len() {
        *more = true;
        return;
    }
    let fd = sys::pidfd_open(pid);
    if fd < 0 {
        return;
    }
    if parent_of(pid) != Some(parent) || (parent_fd >= 0 && !still_running(parent_fd)) {
        close(fd);
        return;
    }
    sys::pidfd_send_signal(fd, 19);
    tree[*count] = Held { fd, pid, parent };
    *count += 1;
}

/// Stop a running or exited program and every descendant. Keep its leader
/// unreaped for the caller; every descendant is stopped through a held pidfd.
pub(super) fn sweep_descendants(keep: c_int) -> bool {
    if keep <= 1 {
        return false;
    }
    let Some(start) = now_ms() else {
        return false;
    };
    let deadline = start.saturating_add(SWEEP_MS);
    let myself = getpid();
    loop {
        // One round: neither the lists nor the pidfds outlive it.
        let (others, more, ended) = TREE.with(|tree| {
            PIDS.with(|direct| {
                BELOW.with(|below| {
                    let (children, mut more) = list_children(myself, direct);
                    let mut held = 0;
                    for &pid in &direct[..children] {
                        hold(pid, myself, -1, tree, &mut held, &mut more);
                    }
                    let mut at = 0;
                    while at < held {
                        let parent = tree[at];
                        let (found, truncated) = list_children(parent.pid, below);
                        more |= truncated;
                        if still_running(parent.fd) {
                            for &pid in &below[..found] {
                                hold(pid, parent.pid, parent.fd, tree, &mut held, &mut more);
                            }
                        }
                        at += 1;
                    }
                    let mut others = 0;
                    for process in &tree[..held] {
                        sys::pidfd_send_signal(process.fd, 9);
                        if process.pid != keep {
                            others += 1;
                        }
                    }
                    for process in &tree[..held] {
                        if process.parent == myself && process.pid != keep {
                            sys::waitpid(process.pid, None, 1);
                        }
                        close(process.fd);
                    }
                    let mut seen = SignalInfo::zeroed();
                    let ended = sys::waitid(1, keep as u32, &mut seen, 4 | 1 | 0x01000000) == 0
                        && seen.pid == keep;
                    (others, more, ended)
                })
            })
        });
        if others == 0 && !more && ended {
            return true;
        }
        if now_ms().is_none_or(|now| now >= deadline) {
            return false;
        }
        let delay = Timespec {
            seconds: 0,
            nanos: 1_000_000,
        };
        sys::nanosleep(&delay);
    }
}
