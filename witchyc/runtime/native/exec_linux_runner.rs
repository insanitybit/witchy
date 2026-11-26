//! Linux's unconfined Exec runner and its subreaper warden. The broker starts
//! both before installing Landlock, and retains only their private channel.
use super::global::Global;
use super::linux_sandbox::witchy_close_inherited_keeping;
use super::sys::{self, PollFd, SignalInfo};
use super::{
    exec_broker, exec_linux_children as children, exec_linux_transport as transport, exec_programs,
};
use core::ffi::c_int;

const BUFFER: usize = 1 << 17;
static RUNNER: Global<c_int> = Global::new(-1);
static WARDEN_FD: Global<c_int> = Global::new(-1);
static WARDEN_PIPE: Global<c_int> = Global::new(-1);
static FAILED: Global<bool> = Global::new(false);
// Not a `State`: the runner holds this buffer across the request it serves,
// and serving a run forks the program.
static mut SCRATCH: [u8; BUFFER] = [0; BUFFER];

unsafe extern "C" {
    fn fork() -> c_int;
    safe fn close(fd: c_int) -> c_int;
    safe fn shutdown(fd: c_int, how: c_int) -> c_int;
    safe fn getpid() -> c_int;
    safe fn getppid() -> c_int;
    safe fn witchy_exec_supervise() -> bool;
    safe fn witchy_exec_sweep_own() -> bool;
    safe fn _exit(code: c_int) -> !;
}

fn poll_one(fd: c_int, timeout: c_int) -> c_int {
    let mut p = PollFd {
        fd,
        events: 1,
        returned: 0,
    };
    loop {
        let n = sys::poll(core::slice::from_mut(&mut p), timeout);
        if n < 0 && super::errno::get() == 4 {
            continue;
        }
        return if n > 0 && p.returned != 0 { 1 } else { n };
    }
}

fn runner_loop(channel: c_int, broker_pid: c_int) -> ! {
    let parent = getppid();
    if !sys::default_signal(17)
        || sys::set_parent_death_signal(15) != 0
        || getppid() != parent
        || parent == broker_pid
        || sys::set_child_subreaper() != 0
        || sys::set_no_new_privileges() != 0
        || sys::clear_dumpable() != 0
    {
        _exit(2)
    }
    let mut alive = [-1; 2];
    if sys::pipe2(&mut alive, 0o2000000) != 0 {
        _exit(2)
    }
    super::exec_linux_limits::set_liveness(alive[0], alive[1]);
    let mut keep = [-1; 19];
    keep[..3].copy_from_slice(&[channel, alive[0], alive[1]]);
    let count = exec_programs::witchy_exec_descriptors(&mut keep[3..]);
    if count > 16 || !witchy_close_inherited_keeping(false, false, &keep[..3 + count]) {
        _exit(2)
    }
    loop {
        let mut head = [0u8; 9];
        let passed = match transport::receive_head(channel, &mut head) {
            Ok(fd) => fd,
            Err(()) => _exit(0),
        };
        let count = u32::from_be_bytes(head[1..5].try_into().unwrap()) as usize;
        let capacity = u32::from_be_bytes(head[5..9].try_into().unwrap()) as usize;
        if count > BUFFER || capacity > BUFFER {
            if let Some(fd) = passed {
                close(fd);
            }
            _exit(2)
        }
        // SAFETY: this loop is the buffer's one user, on the runner's one
        // thread, and no reference from an earlier turn is still live.
        let buffer = unsafe { &mut *(&raw mut SCRATCH) };
        if !transport::read_exact(channel, &mut buffer[..count]) {
            if let Some(fd) = passed {
                close(fd);
            }
            _exit(2)
        }
        let mut answer = 0usize;
        let ok = if head[0] == 32 {
            if count != 0 || capacity != 0 {
                if let Some(fd) = passed {
                    close(fd);
                }
                false
            } else if let Some(fd) = passed {
                exec_broker::set_cwd_here(fd)
            } else {
                false
            }
        } else if let Some(fd) = passed {
            close(fd);
            false
        } else {
            match exec_broker::request_here(channel, head[0], &mut buffer[..capacity], count) {
                Some(size) => {
                    answer = size;
                    true
                }
                None => false,
            }
        };
        if answer > capacity {
            _exit(2)
        }
        let size = (answer as u32).to_be_bytes();
        let response = [u8::from(ok), size[0], size[1], size[2], size[3]];
        if !transport::send_all(channel, &response)
            || !transport::send_all(channel, &buffer[..answer])
        {
            _exit(0)
        }
    }
}

fn start_warden(pair: [c_int; 2], broker: c_int) -> c_int {
    let mut alive = [-1; 2];
    if sys::pipe2(&mut alive, 0o2000000) != 0 {
        return -1;
    }
    let warden = unsafe { fork() };
    if warden < 0 {
        close(alive[0]);
        close(alive[1]);
        return -1;
    }
    if warden > 0 {
        close(alive[0]);
        WARDEN_PIPE.set(alive[1]);
        return warden;
    }
    super::exec_temp::set_directory_owner(broker);
    close(pair[0]);
    close(alive[1]);
    if !sys::default_signal(17)
        || !witchy_exec_supervise()
        || sys::clear_dumpable() != 0
        || getppid() != broker
    {
        _exit(2)
    }
    let runner = unsafe { fork() };
    if runner == 0 {
        close(alive[0]);
        children::discard_inherited();
        runner_loop(pair[1], broker);
    }
    close(pair[1]);
    let keep = [alive[0], children::children_descriptor()];
    if runner < 0
        || keep[1] < 0
        || !witchy_close_inherited_keeping(false, false, &keep)
    {
        _exit(2)
    }
    loop {
        let ready = poll_one(alive[0], 50);
        let ended = sys::waitpid(runner, None, 1);
        if ready != 0 || ended == runner || ended < 0 {
            break;
        }
    }
    _exit(if witchy_exec_sweep_own() { 0 } else { 3 })
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_start_runner() -> bool {
    if !exec_programs::configured() || RUNNER.get() >= 0 {
        return true;
    }
    let mut pair = [-1; 2];
    if sys::socketpair(1, 1 | 0o2000000, 0, &mut pair) != 0 {
        return false;
    }
    let broker = getpid();
    let warden = start_warden(pair, broker);
    if warden < 0 {
        close(pair[0]);
        close(pair[1]);
        return false;
    }
    close(pair[1]);
    RUNNER.set(pair[0]);
    let pidfd = sys::pidfd_open(warden);
    if pidfd < 0 {
        close(pair[0]);
        RUNNER.set(-1);
        close(WARDEN_PIPE.get());
        WARDEN_PIPE.set(-1);
        sys::waitpid(warden, None, 0);
        return false;
    }
    WARDEN_FD.set(pidfd);
    exec_programs::close_broker_descriptors();
    true
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_signal_descriptor() -> c_int {
    WARDEN_FD.get()
}
#[unsafe(no_mangle)]
extern "C" fn witchy_exec_runner_descriptor() -> c_int {
    RUNNER.get()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_exec_finish() -> bool {
    if RUNNER.get() >= 0 {
        shutdown(RUNNER.get(), 2);
        close(RUNNER.get());
        RUNNER.set(-1);
    }
    if WARDEN_PIPE.get() >= 0 {
        close(WARDEN_PIPE.get());
        WARDEN_PIPE.set(-1);
    }
    if WARDEN_FD.get() >= 0 {
        if poll_one(WARDEN_FD.get(), 7000) != 1 {
            sys::pidfd_send_signal(WARDEN_FD.get(), 9);
            FAILED.set(true);
            let _ = poll_one(WARDEN_FD.get(), 2000);
        }
        let mut seen = SignalInfo::zeroed();
        if sys::waitid(3, WARDEN_FD.get() as u32, &mut seen, 4 | 1) != 0
            || seen.code != 1
            || seen.status != 0
        {
            FAILED.set(true);
        }
        close(WARDEN_FD.get());
        WARDEN_FD.set(-1);
    }
    !FAILED.get()
}

pub(super) fn witchy_exec_set_cwd(fd: c_int) -> bool {
    let runner = RUNNER.get();
    if runner < 0 {
        return exec_broker::set_cwd_here(fd);
    }
    let mut head = [0u8; 9];
    head[0] = 32;
    let sent = transport::send_head(runner, &head, fd);
    close(fd);
    let mut answer = [0u8; 5];
    if !sent || !transport::read_exact(runner, &mut answer) || answer[1..] != [0; 4] {
        witchy_exec_finish();
        return false;
    }
    answer[0] != 0
}

/// Serve one Exec request through the runner, or in this process when no
/// runner was started. The first `count` bytes of `buffer` are the request;
/// the answer replaces the front of `buffer` and its length is returned.
/// `None` refuses the request.
pub(super) fn witchy_exec_request(
    channel: c_int,
    operation: u8,
    buffer: &mut [u8],
    count: usize,
) -> Option<usize> {
    let capacity = buffer.len();
    if count > BUFFER || capacity > BUFFER {
        return None;
    }
    let runner = RUNNER.get();
    if runner < 0 {
        return exec_broker::request_here(channel, operation, buffer, count);
    }
    let input = buffer.get(..count)?;
    let mut head = [0u8; 9];
    head[0] = operation;
    head[1..5].copy_from_slice(&(count as u32).to_be_bytes());
    head[5..9].copy_from_slice(&(capacity as u32).to_be_bytes());
    if !transport::send_all(runner, &head) || !transport::send_all(runner, input) {
        witchy_exec_finish();
        return None;
    }
    let mut ready = [
        PollFd {
            fd: runner,
            events: 1,
            returned: 0,
        },
        PollFd {
            fd: channel,
            events: 1,
            returned: 0,
        },
    ];
    loop {
        let n = sys::poll(&mut ready, -1);
        if n < 0 && super::errno::get() == 4 {
            continue;
        }
        if n < 0 || ready[1].returned & (1 | 8 | 16 | 32) != 0 {
            witchy_exec_finish();
            return None;
        }
        if ready[0].returned != 0 {
            break;
        }
    }
    let mut header = [0u8; 5];
    if !transport::read_exact(runner, &mut header) {
        witchy_exec_finish();
        return None;
    }
    let size = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
    let Some(output) = buffer.get_mut(..size) else {
        witchy_exec_finish();
        return None;
    };
    if !transport::read_exact(runner, output) {
        witchy_exec_finish();
        return None;
    }
    (header[0] != 0).then_some(size)
}

#[cfg(test)]
#[test]
fn warden_runs_granted_program_and_reaps_runner() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        safe fn witchy_exec_program(device: u64, inode: u64) -> bool;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = sys::open(&parent, 0o40000 | 0o2000000);
    assert!(directory >= 0);
    assert!(exec_programs::witchy_exec_setup(directory, &[name.as_c_str()], 4096));
    assert!(witchy_exec_start_runner());
    let identity = std::fs::metadata(&executable).unwrap();
    assert!(witchy_exec_program(identity.dev(), identity.ino()));
    let cwd = sys::open(c"/tmp", 0o40000 | 0o2000000);
    assert!(cwd >= 0);
    assert!(witchy_exec_set_cwd(cwd));
    let mut channel = [-1; 2];
    assert_eq!(sys::socketpair(1, 1, 0, &mut channel), 0);
    let mut args = *b"--exact\0exec_broker::linux_subprocess_helper\0--nocapture\0";
    let count = args.len();
    assert!(witchy_exec_request(channel[0], 27, &mut args, count).is_some());
    let mut request = [0u8; 11];
    request[1..5].copy_from_slice(&1000u32.to_be_bytes());
    request[5..9].copy_from_slice(&1024u32.to_be_bytes());
    request[9] = 3;
    assert!(witchy_exec_request(channel[0], 28, &mut request, 11).is_some());
    assert_eq!(request[0], 0, "program status {}", request[0]);
    let mut output = [0u8; 1024];
    let reply = witchy_exec_request(channel[0], 29, &mut output, 0).unwrap();
    assert!(
        output[..reply]
            .windows(b"linux-exec-hello".len())
            .any(|s| s == b"linux-exec-hello"),
        "packet={request:?} reply={reply} output={}",
        String::from_utf8_lossy(&output[..reply])
    );
    assert!(
        output[..reply]
            .windows(b"cwd=/tmp".len())
            .any(|s| s == b"cwd=/tmp")
    );
    assert!(witchy_exec_finish());
    close(channel[0]);
    close(channel[1]);
}

#[cfg(test)]
#[test]
fn warden_cleans_up_after_runner_is_killed() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = sys::open(&parent, 0o40000 | 0o2000000);
    assert!(directory >= 0);
    assert!(exec_programs::witchy_exec_setup(directory, &[name.as_c_str()], 4096));
    assert!(witchy_exec_start_runner());
    let mut channel = [-1; 2];
    assert_eq!(sys::socketpair(1, 1, 0, &mut channel), 0);
    let mut args = *b"--exact\0exec_broker::linux_kill_runner_helper\0--nocapture\0";
    let count = args.len();
    assert!(witchy_exec_request(channel[0], 27, &mut args, count).is_some());
    let mut request = [0u8; 11];
    request[1..5].copy_from_slice(&1000u32.to_be_bytes());
    request[5..9].copy_from_slice(&1024u32.to_be_bytes());
    request[9] = 3;
    assert!(witchy_exec_request(channel[0], 28, &mut request, 11).is_none());
    assert_eq!(witchy_exec_runner_descriptor(), -1);
    assert_eq!(witchy_exec_signal_descriptor(), -1);
    assert!(witchy_exec_finish());
    close(channel[0]);
    close(channel[1]);
}
