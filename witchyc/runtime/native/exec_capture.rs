//! Bounded I/O and process lifetime for an Exec child.
#[cfg(target_os = "linux")]
pub(super) use super::exec_linux_children::SignalInfo;
use super::exec_pending::Buffer;
#[cfg(target_os = "macos")]
use super::exec_supervision::{stop_live, stop_session};
use core::ffi::{c_int, c_void};

#[repr(C)]
#[cfg(target_os = "macos")]
struct PollFd {
    fd: c_int,
    events: i16,
    returned: i16,
}
#[cfg(target_os = "linux")]
use super::socket_primitives::PollFd;

#[repr(C)]
#[cfg(target_os = "macos")]
pub(super) struct SignalInfo {
    signal: c_int,
    error: c_int,
    code: c_int,
    pub(super) pid: c_int,
    uid: u32,
    status: c_int,
    address: *mut c_void,
    value: u64,
    band: i64,
    reserved: [u64; 7],
}

unsafe extern "C" {
    fn waitid(kind: c_int, id: u32, info: *mut SignalInfo, options: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
    #[cfg(target_os = "macos")]
    fn poll(fds: *mut PollFd, count: u32, timeout: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, length: usize) -> isize;
    fn write(fd: c_int, buffer: *const c_void, length: usize) -> isize;
    safe fn close(fd: c_int) -> c_int;
    #[cfg(target_os = "macos")]
    safe fn kill(pid: c_int, signal: c_int) -> c_int;
}

#[cfg(target_os = "linux")]
fn stop_live(child: c_int) {
    if !super::exec_linux_children::sweep_descendants(child) {
        unsafe extern "C" {
            safe fn _exit(code: c_int) -> !;
        }
        _exit(3)
    }
}
#[cfg(target_os = "linux")]
fn stop_session(child: c_int) {
    stop_live(child);
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Status {
    Success,
    TooLarge,
    TimedOut,
    Failed,
    Gone,
}

pub(super) struct Capture {
    pub(super) status: Status,
    pub(super) code: i64,
    pub(super) output: Buffer,
    pub(super) errors: Buffer,
}

fn now() -> u64 {
    super::deadline::monotonic_ms().unwrap_or(u64::MAX)
}

fn reap(child: c_int, status: &mut c_int) {
    loop {
        if unsafe { waitpid(child, status, 0) } >= 0 || super::errno::get() != 4 {
            return;
        }
    }
}

/// The caller owns stdout/stderr FDs and provides a mutable stdin FD; this
/// routine closes stdin when it is finished. The child is always reaped.
pub(super) fn collect(
    channel: c_int,
    child: c_int,
    stdout: c_int,
    stderr: c_int,
    stdin: &mut c_int,
    input: &[u8],
    allowed: u64,
    timeout_ms: u32,
    stopped: fn() -> bool,
) -> Capture {
    let mut result = Capture {
        status: Status::Success,
        code: 0,
        output: Buffer::EMPTY,
        errors: Buffer::EMPTY,
    };
    let mut input_at = 0usize;
    let mut ended = [false; 2];
    let mut exited = false;
    let mut wait_status = 0;
    let deadline = now().saturating_add(u64::from(timeout_ms));
    let mut after_exit = 0;
    while !ended[0] || !ended[1] || !exited {
        if !exited {
            let mut seen: SignalInfo = unsafe { core::mem::zeroed() };
            #[cfg(target_os = "macos")]
            const WNOWAIT: c_int = 0x20;
            #[cfg(target_os = "linux")]
            const WNOWAIT: c_int = 0x01000000;
            if unsafe { waitid(1, child as u32, &mut seen, 4 | 1 | WNOWAIT) } == 0
                && seen.pid == child
            {
                exited = true;
                stop_session(child);
                #[cfg(target_os = "macos")]
                kill(-child, 9);
                reap(child, &mut wait_status);
                after_exit = now().saturating_add(200);
                if *stdin >= 0 {
                    close(*stdin);
                    *stdin = -1;
                }
            }
        }
        let time = now();
        if stopped() {
            result.status = Status::Gone;
            break;
        }
        if !exited && time >= deadline {
            result.status = Status::TimedOut;
            break;
        }
        if exited && (!ended[0] || !ended[1]) && time >= after_exit {
            result.status = Status::Failed;
            break;
        }
        let mut ready = [
            PollFd {
                fd: channel,
                events: 1,
                returned: 0,
            },
            PollFd {
                fd: if ended[0] { -1 } else { stdout },
                events: 1,
                returned: 0,
            },
            PollFd {
                fd: if ended[1] { -1 } else { stderr },
                events: 1,
                returned: 0,
            },
            PollFd {
                fd: *stdin,
                events: 4,
                returned: 0,
            },
        ];
        let limit = if exited { after_exit } else { deadline };
        let wait = if ended[0] && ended[1] && exited {
            // Check the broker channel once without waiting after reaping a fully drained child.
            0
        } else if ended[0] && ended[1] {
            2
        } else {
            limit.saturating_sub(time).min(50) as c_int
        };
        let polled = unsafe { poll(ready.as_mut_ptr(), 4, wait) };
        if polled < 0 && super::errno::get() != 4 {
            result.status = Status::Failed;
            break;
        }
        if polled > 0 && ready[0].returned & (0x10 | 0x08 | 0x20) != 0 {
            result.status = Status::Gone;
            break;
        }
        if polled > 0 && *stdin >= 0 && ready[3].returned != 0 {
            let writable = ready[3].returned & 4 != 0;
            let count = if writable {
                unsafe {
                    write(
                        *stdin,
                        input[input_at..].as_ptr().cast(),
                        input.len() - input_at,
                    )
                }
            } else {
                -1
            };
            if count > 0 {
                input_at += count as usize;
            }
            let error = if count < 0 { super::errno::get() } else { 0 };
            if !writable || (count < 0 && error != 35 && error != 4) || input_at == input.len() {
                close(*stdin);
                *stdin = -1;
            }
        }
        let mut scratch = [0u8; 16384];
        for stream in 0..2 {
            if polled <= 0 || ready[stream + 1].returned == 0 || ready[stream + 1].fd < 0 {
                continue;
            }
            let count = unsafe {
                read(
                    ready[stream + 1].fd,
                    scratch.as_mut_ptr().cast(),
                    scratch.len(),
                )
            };
            if count > 0 {
                let total = result
                    .output
                    .bytes()
                    .len()
                    .saturating_add(result.errors.bytes().len())
                    .saturating_add(count as usize);
                if total as u64 > allowed {
                    result.status = Status::TooLarge;
                    break;
                }
                let buffer = if stream == 0 {
                    &mut result.output
                } else {
                    &mut result.errors
                };
                if !buffer.append(&scratch[..count as usize]) {
                    result.status = Status::Failed;
                    break;
                }
            } else if count == 0 {
                ended[stream] = true;
            } else if super::errno::get() != if cfg!(target_os = "linux") { 11 } else { 35 }
                && super::errno::get() != 4
            {
                result.status = Status::Failed;
                break;
            }
        }
        if result.status != Status::Success {
            break;
        }
    }
    if !exited {
        stop_live(child);
        reap(child, &mut wait_status);
    }
    result.code = if wait_status & 0x7f == 0 {
        i64::from((wait_status >> 8) & 0xff)
    } else {
        256
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" {
        fn fork() -> c_int;
        fn pipe(fds: *mut c_int) -> c_int;
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
        safe fn setsid() -> c_int;
        safe fn _exit(code: c_int) -> !;
        safe fn pause() -> c_int;
    }

    fn run(hang: bool, limit: u64) -> Capture {
        let mut output = [-1; 2];
        let mut errors = [-1; 2];
        let mut channel = [-1; 2];
        assert_eq!(unsafe { pipe(output.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { pipe(errors.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
        let child = unsafe { fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe {
                setsid();
                close(output[0]);
                close(errors[0]);
                close(channel[0]);
                close(channel[1]);
                if hang {
                    pause();
                }
                write(output[1], b"hello".as_ptr().cast(), 5);
                write(errors[1], b"bad".as_ptr().cast(), 3);
                _exit(7);
            }
        }
        unsafe {
            close(output[1]);
            close(errors[1]);
        }
        let mut stdin = -1;
        let result = collect(
            channel[0],
            child,
            output[0],
            errors[0],
            &mut stdin,
            b"",
            limit,
            500,
            || false,
        );
        unsafe {
            close(output[0]);
            close(errors[0]);
            close(channel[0]);
            close(channel[1]);
        }
        result
    }

    #[test]
    fn collects_both_streams_and_exit_status() {
        let result = run(false, 32);
        assert_eq!(result.status, Status::Success);
        assert_eq!(result.code, 7);
        assert_eq!(result.output.bytes(), b"hello");
        assert_eq!(result.errors.bytes(), b"bad");
    }

    #[test]
    fn enforces_output_and_time_limits() {
        assert_eq!(run(false, 3).status, Status::TooLarge);
        assert_eq!(run(true, 32).status, Status::TimedOut);
    }
}
