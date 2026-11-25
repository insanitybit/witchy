//! Runner execution of explicitly granted programs.
use super::{
    deadline::{SignalAction, SignalSet},
    exec_arguments,
    exec_capture::{self, Capture, Status},
    exec_environment,
    exec_pending::{Kind, Pending},
    exec_programs,
    exec_temp::Directory,
};
use core::{
    ffi::{c_char, c_int},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

unsafe extern "C" {
    fn sigaction(signal: c_int, action: *const SignalAction, old: *mut SignalAction) -> c_int;
    fn sigemptyset(set: *mut SignalSet) -> c_int;
    fn sigprocmask(how: c_int, set: *const SignalSet, old: *mut SignalSet) -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(fd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
    fn pipe(fds: *mut c_int) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn fork() -> c_int;
    safe fn dup2(source: c_int, destination: c_int) -> c_int;
    safe fn setsid() -> c_int;
    safe fn fchdir(fd: c_int) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
    fn execve(path: *const c_char, argv: *const *const c_char, env: *const *const c_char) -> c_int;
    safe fn _exit(code: c_int) -> !;
}

struct Broker {
    pending: Pending,
    cwd: c_int,
    capture: Option<Capture>,
    output_at: usize,
}
static mut BROKER: Broker = Broker {
    pending: Pending::EMPTY,
    cwd: -1,
    capture: None,
    output_at: 0,
};
static STOPPED: AtomicBool = AtomicBool::new(false);
extern "C" fn request_stop(_: c_int) {
    STOPPED.store(true, Ordering::Relaxed);
}
fn stopped() -> bool {
    STOPPED.load(Ordering::Relaxed)
}

fn close_inherited() -> bool {
    #[cfg(target_os = "linux")]
    use super::linux_sandbox::witchy_close_inherited_keeping;
    #[cfg(target_os = "macos")]
    use super::seatbelt_fds::witchy_close_inherited_keeping;
    witchy_close_inherited_keeping(true, true, &[])
}

struct Signals {
    term: SignalAction,
    pipe: SignalAction,
}
impl Signals {
    fn install() -> Option<Self> {
        let mut stop = SignalAction::new(request_stop as *const () as usize);
        let mut ignore = SignalAction::new(1);
        let mut previous = SignalAction::new(0);
        let mut piped = SignalAction::new(0);
        if unsafe {
            sigemptyset(&mut stop.mask) != 0
                || sigemptyset(&mut ignore.mask) != 0
                || sigaction(15, &stop, &mut previous) != 0
        } {
            return None;
        }
        if unsafe { sigaction(13, &ignore, &mut piped) } != 0 {
            unsafe {
                sigaction(15, &previous, ptr::null_mut());
            }
            return None;
        }
        Some(Self {
            term: previous,
            pipe: piped,
        })
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            sigaction(15, &self.term, ptr::null_mut());
            sigaction(13, &self.pipe, ptr::null_mut());
        }
    }
}

struct Pipes {
    out: [c_int; 2],
    err: [c_int; 2],
    input: [c_int; 2],
}
impl Pipes {
    fn new(with_input: bool) -> Option<Self> {
        let mut result = Self {
            out: [-1; 2],
            err: [-1; 2],
            input: [-1; 2],
        };
        if unsafe {
            pipe(result.out.as_mut_ptr()) != 0
                || pipe(result.err.as_mut_ptr()) != 0
                || (with_input && pipe(result.input.as_mut_ptr()) != 0)
        } {
            return None;
        }
        unsafe {
            fcntl(result.out[0], 2, 1 as c_int);
            fcntl(result.err[0], 2, 1 as c_int);
            if with_input {
                fcntl(result.input[1], 2, 1 as c_int);
            }
        }
        Some(result)
    }
    fn close_at(pair: &mut [c_int; 2], index: usize) {
        if pair[index] >= 0 {
            close(pair[index]);
            pair[index] = -1;
        }
    }
}
impl Drop for Pipes {
    fn drop(&mut self) {
        for index in 0..2 {
            Self::close_at(&mut self.out, index);
            Self::close_at(&mut self.err, index);
            Self::close_at(&mut self.input, index);
        }
    }
}

fn run_program(
    broker: &mut Broker,
    channel: c_int,
    index: usize,
    timeout: u32,
    asked: u32,
    argc: usize,
) -> (u8, i64) {
    broker.capture = None;
    broker.output_at = 0;
    let mut offsets = [0; 64];
    if !exec_arguments::offsets(broker.pending.arguments(), argc, &mut offsets)
        || !exec_environment::valid_values(broker.pending.environment())
    {
        return (2, 0);
    }
    let Some(program) = exec_programs::program(index) else {
        return (4, 0);
    };
    let Some(temp) = Directory::create() else {
        return (5, 0);
    };
    let name = match program.name.iter().position(|&byte| byte == 0) {
        Some(at) => &program.name[..at],
        None => return (5, 0),
    };
    let mut executable = [0u8; 1024];
    if !temp.program_path(name, &mut executable) {
        return (5, 0);
    }
    #[cfg(target_os = "macos")]
    const NEW_EXECUTABLE: c_int = 0x0100_0b01;
    #[cfg(target_os = "linux")]
    const NEW_EXECUTABLE: c_int = 0o2000000 | 0o100000 | 0o301;
    let target = unsafe {
        openat(
            temp.fd,
            program.name.as_ptr().cast(),
            NEW_EXECUTABLE,
            0o700 as c_int,
        )
    };
    if target < 0 {
        return (5, 0);
    }
    let copied = super::exec_temp::copy_program(program.fd, target);
    if close(target) != 0 || !copied {
        return (5, 0);
    }

    let args = broker.pending.arguments();
    let mut argv = [ptr::null::<c_char>(); 66];
    argv[0] = program.name.as_ptr().cast();
    for i in 0..argc {
        argv[i + 1] = unsafe { args.as_ptr().add(offsets[i]).cast() };
    }
    let environment = broker.pending.environment();
    let mut env = [ptr::null::<c_char>(); 17];
    let (mut at, mut count) = (0, 0);
    while at < environment.len() {
        env[count] = unsafe { environment.as_ptr().add(at).cast() };
        let Some(end) = environment[at..].iter().position(|&byte| byte == 0) else {
            return (2, 0);
        };
        at += end + 1;
        count += 1;
    }
    let input = broker.pending.input();
    let Some(mut pipes) = Pipes::new(!input.is_empty()) else {
        return (5, 0);
    };
    #[cfg(target_os = "linux")]
    let process_limit = super::exec_linux_limits::current_limit();
    let child = unsafe { fork() };
    if child == 0 {
        unsafe {
            #[cfg(target_os = "linux")]
            if !super::exec_linux_limits::prepare_program(&process_limit) {
                _exit(127);
            }
            let mut none = super::deadline::empty_signal_set();
            sigemptyset(&mut none);
            #[cfg(target_os = "macos")]
            sigprocmask(3, &none, ptr::null_mut());
            #[cfg(target_os = "linux")]
            sigprocmask(2, &none, ptr::null_mut());
            signal(13, 0);
            signal(15, 0);
            if setsid() < 0 {
                _exit(127);
            }
            let null = open(c"/dev/null".as_ptr(), 2);
            if null < 0
                || dup2(
                    if input.is_empty() {
                        null
                    } else {
                        pipes.input[0]
                    },
                    0,
                ) < 0
                || dup2(pipes.out[1], 1) < 0
                || dup2(pipes.err[1], 2) < 0
                || if broker.cwd >= 0 {
                    fchdir(broker.cwd) != 0
                } else {
                    chdir(temp.path_ptr()) != 0
                }
                || !close_inherited()
            {
                _exit(127);
            }
            execve(executable.as_ptr().cast(), argv.as_ptr(), env.as_ptr());
            _exit(127);
        }
    }
    Pipes::close_at(&mut pipes.out, 1);
    Pipes::close_at(&mut pipes.err, 1);
    if !input.is_empty() {
        Pipes::close_at(&mut pipes.input, 0);
    }
    if child < 0 {
        return (5, 0);
    }
    #[cfg(target_os = "macos")]
    const NONBLOCK: c_int = 4;
    #[cfg(target_os = "linux")]
    const NONBLOCK: c_int = 0o4000;
    unsafe {
        fcntl(pipes.out[0], 4, NONBLOCK);
        fcntl(pipes.err[0], 4, NONBLOCK);
        if pipes.input[1] >= 0 {
            fcntl(pipes.input[1], 4, NONBLOCK);
        }
    }
    let allowed = u64::from(asked).min(exec_programs::budget());
    let capture = exec_capture::collect(
        channel,
        child,
        pipes.out[0],
        pipes.err[0],
        &mut pipes.input[1],
        input,
        allowed,
        timeout,
        stopped,
    );
    let status = match capture.status {
        Status::Success => 0,
        Status::TooLarge => 3,
        Status::TimedOut => 6,
        Status::Failed => 5,
        Status::Gone => {
            drop(pipes);
            drop(temp);
            _exit(0)
        }
    };
    let code = capture.code;
    if status == 0 {
        exec_programs::spend((capture.output.bytes().len() + capture.errors.bytes().len()) as u64);
        broker.capture = Some(capture);
    }
    (status, code)
}

fn run(
    broker: &mut Broker,
    channel: c_int,
    index: usize,
    timeout: u32,
    asked: u32,
    argc: usize,
) -> (u8, i64) {
    let Some(signals) = Signals::install() else {
        return (5, 0);
    };
    let result = run_program(broker, channel, index, timeout, asked, argc);
    drop(signals);
    if stopped() {
        _exit(2)
    }
    result
}

pub(super) fn set_cwd_here(fd: c_int) -> bool {
    let broker = unsafe { &mut *(&raw mut BROKER) };
    if broker.cwd >= 0 {
        close(broker.cwd);
    }
    broker.cwd = fd;
    true
}

#[cfg(target_os = "macos")]
pub(super) fn witchy_exec_set_cwd(fd: c_int) -> bool {
    set_cwd_here(fd)
}

/// Serve one Exec request in this process. The first `count` bytes of
/// `buffer` are the request; the answer replaces the front of `buffer` and
/// its length is returned. `None` refuses the request.
pub(super) fn request_here(
    channel: c_int,
    operation: u8,
    buffer: &mut [u8],
    count: usize,
) -> Option<usize> {
    if !exec_programs::configured() {
        return None;
    }
    let broker = unsafe { &mut *(&raw mut BROKER) };
    match operation {
        27 | 30 | 31 => broker
            .pending
            .append(
                match operation {
                    27 => Kind::Arguments,
                    30 => Kind::Input,
                    _ => Kind::Environment,
                },
                buffer.get(..count)?,
            )
            .then_some(0),
        28 => {
            if count != 11 {
                return None;
            }
            let packet = buffer.get_mut(..11)?;
            if packet[10] != 0 {
                return None;
            }
            let index = packet[0] as usize;
            let timeout = u32::from_be_bytes([packet[1], packet[2], packet[3], packet[4]]);
            let asked = u32::from_be_bytes([packet[5], packet[6], packet[7], packet[8]]);
            let argc = packet[9] as usize;
            let (status, code) = if timeout > 2000 {
                (2, 0)
            } else {
                run(broker, channel, index, timeout, asked, argc)
            };
            broker.pending.clear();
            if broker.cwd >= 0 {
                close(broker.cwd);
                broker.cwd = -1;
            }
            let (out, err) = if status == 0 {
                broker.capture.as_ref().map_or((0, 0), |capture| {
                    (
                        capture.output.bytes().len() as u32,
                        capture.errors.bytes().len() as u32,
                    )
                })
            } else {
                (0, 0)
            };
            packet[0] = status;
            packet[1..3].copy_from_slice(&(code as u16).to_be_bytes());
            packet[3..7].copy_from_slice(&out.to_be_bytes());
            packet[7..11].copy_from_slice(&err.to_be_bytes());
            Some(11)
        }
        29 => {
            let Some(capture) = broker.capture.as_ref() else {
                return Some(0);
            };
            let out = capture.output.bytes();
            let err = capture.errors.bytes();
            let total = out.len() + err.len();
            if broker.output_at > total {
                return None;
            }
            let amount = (total - broker.output_at).min(buffer.len());
            let mut copied = 0;
            if broker.output_at < out.len() {
                let first = (out.len() - broker.output_at).min(amount);
                buffer[..first].copy_from_slice(&out[broker.output_at..broker.output_at + first]);
                copied = first;
            }
            if copied < amount {
                let start = broker.output_at.saturating_sub(out.len());
                buffer[copied..amount].copy_from_slice(&err[start..start + amount - copied]);
            }
            broker.output_at += amount;
            Some(amount)
        }
        _ => None,
    }
}

/// See `request_here`, which serves every request on macOS.
#[cfg(target_os = "macos")]
pub(super) fn witchy_exec_request(
    channel: c_int,
    operation: u8,
    buffer: &mut [u8],
    count: usize,
) -> Option<usize> {
    request_here(channel, operation, buffer, count)
}

/// The decoder fuzz driver's entry: `tests/fixtures/native-sandbox/exec-fuzz.rs`
/// links the archive by this symbol. `bytes` has `capacity` writable bytes
/// whose first `count` are the request, and `reply` is writable.
#[unsafe(export_name = "witchy_exec_request")]
unsafe extern "C" fn request_from_raw(
    channel: c_int,
    operation: u8,
    bytes: *mut u8,
    count: usize,
    capacity: usize,
    reply: *mut usize,
) -> bool {
    #[cfg(target_os = "linux")]
    use super::exec_linux_runner::witchy_exec_request;
    if reply.is_null() || (bytes.is_null() && (count != 0 || capacity != 0)) {
        return false;
    }
    let buffer = if capacity == 0 {
        &mut [][..]
    } else {
        // SAFETY: the caller supplies `capacity` writable bytes at `bytes`.
        unsafe { core::slice::from_raw_parts_mut(bytes, capacity) }
    };
    let answer = witchy_exec_request(channel, operation, buffer, count);
    // SAFETY: the caller supplies a writable `reply`.
    unsafe {
        *reply = answer.unwrap_or(0);
    }
    answer.is_some()
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn subprocess_helper() {
    if std::env::var_os("TERM").is_some_and(|value| value == "slow") {
        std::thread::sleep(std::time::Duration::from_secs(2));
    } else {
        println!("witchy-exec-hello");
    }
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn executes_granted_program_and_enforces_timeout() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = unsafe { open(parent.as_ptr(), 0x0110_0000) };
    assert!(directory >= 0);
    assert!(exec_programs::witchy_exec_setup(directory, &[name.as_c_str()], 4096));
    assert!(exec_environment::witchy_exec_env_names(c"TERM"));
    let mut channel = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
    let mut argument = *b"--exact\0exec_broker::subprocess_helper\0--nocapture\0";
    let count = argument.len();
    assert!(witchy_exec_request(channel[0], 27, &mut argument, count).is_some());
    let mut environment = *b"TERM=xterm\0";
    let count = environment.len();
    assert!(witchy_exec_request(channel[0], 31, &mut environment, count).is_some());
    let mut packet = [0u8; 11];
    packet[1..5].copy_from_slice(&1000u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    let count = packet.len();
    assert_eq!(witchy_exec_request(channel[0], 28, &mut packet, count), Some(11));
    assert_eq!(packet[0], 0, "run failed with status {}", packet[0]);
    assert_eq!(
        u16::from_be_bytes([packet[1], packet[2]]),
        0,
        "packet {packet:?}"
    );
    let mut output = [0u8; 1024];
    let reply = witchy_exec_request(channel[0], 29, &mut output, 0).unwrap();
    assert!(
        output[..reply]
            .windows(b"witchy-exec-hello".len())
            .any(|window| window == b"witchy-exec-hello")
    );
    let mut sleep = *b"--exact\0exec_broker::subprocess_helper\0--nocapture\0";
    let count = sleep.len();
    assert!(witchy_exec_request(channel[0], 27, &mut sleep, count).is_some());
    let mut environment = *b"TERM=slow\0";
    let count = environment.len();
    assert!(witchy_exec_request(channel[0], 31, &mut environment, count).is_some());
    packet = [0; 11];
    packet[1..5].copy_from_slice(&50u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    let count = packet.len();
    assert!(witchy_exec_request(channel[0], 28, &mut packet, count).is_some());
    assert_eq!(packet[0], 6);
    unsafe {
        close(channel[0]);
        close(channel[1]);
    }
    let mut channel = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
    let child = unsafe { fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            close(channel[0]);
        }
        for _ in 0..3 {
            let mut head = [0u8; 4];
            if !super::broker_transport::witchy_broker_receive(channel[1], &mut head) {
                unsafe {
                    _exit(1);
                }
            }
            let count = u16::from_be_bytes([head[1], head[2]]) as usize;
            let mut data = [0u8; 16384];
            if count > data.len()
                || (head[0] != 29
                    && !super::broker_transport::witchy_broker_receive(
                        channel[1],
                        &mut data[..count],
                    ))
            {
                unsafe {
                    _exit(2);
                }
            }
            let capacity = if head[0] == 29 { count } else { data.len() };
            let Some(answer) = witchy_exec_request(
                channel[1],
                head[0],
                &mut data[..capacity],
                if head[0] == 29 { 0 } else { count },
            ) else {
                _exit(3)
            };
            if !super::broker_transport::witchy_broker_reply(channel[1], 0, answer)
                || !super::broker_transport::witchy_broker_send(channel[1], &data[..answer])
            {
                unsafe {
                    _exit(3);
                }
            }
        }
        unsafe {
            _exit(0);
        }
    }
    unsafe {
        close(channel[1]);
    }
    let arguments = b"--exact\0exec_broker::subprocess_helper\0--nocapture\0";
    assert_eq!(
        super::broker_transport::witchy_broker_exec_args(channel[0], arguments),
        0
    );
    let (mut code, mut out_len, mut err_len) = (0, 0, 0);
    assert_eq!(
        super::broker_transport::witchy_broker_exec_run(
            channel[0],
            0,
            1000,
            1024,
            3,
            &mut code,
            &mut out_len,
            &mut err_len,
        ),
        0
    );
    assert_eq!(code, 0);
    let mut output = [0; 1024];
    let count = super::broker_transport::witchy_broker_exec_out(channel[0], &mut output);
    assert_eq!(count as u32, out_len + err_len);
    assert!(
        output[..count as usize]
            .windows(b"witchy-exec-hello".len())
            .any(|window| window == b"witchy-exec-hello")
    );
    unsafe {
        close(channel[0]);
    }
    let mut status = 0;
    assert_eq!(unsafe { waitpid(child, &mut status, 0) }, child);
    assert_eq!(status, 0);
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn linux_subprocess_helper() {
    println!("linux-exec-hello");
    println!("cwd={}", std::env::current_dir().unwrap().display());
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn linux_runner_executes_granted_program() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        safe fn witchy_exec_supervise() -> bool;
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = unsafe { open(parent.as_ptr(), 0o40000 | 0o2000000) };
    assert!(directory >= 0);
    assert!(exec_programs::witchy_exec_setup(directory, &[name.as_c_str()], 4096));
    assert!(unsafe { witchy_exec_supervise() });
    let mut alive = [-1; 2];
    assert_eq!(unsafe { pipe(alive.as_mut_ptr()) }, 0);
    super::exec_linux_limits::set_liveness(alive[0], alive[1]);
    let mut channel = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
    let mut args = *b"--exact\0exec_broker::linux_subprocess_helper\0--nocapture\0";
    let count = args.len();
    assert!(request_here(channel[0], 27, &mut args, count).is_some());
    let mut packet = [0u8; 11];
    packet[1..5].copy_from_slice(&1000u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    let count = packet.len();
    assert!(request_here(channel[0], 28, &mut packet, count).is_some());
    assert_eq!(packet[0], 0, "run status {}", packet[0]);
    assert_eq!(u16::from_be_bytes([packet[1], packet[2]]), 0);
    let mut output = [0u8; 1024];
    let reply = request_here(channel[0], 29, &mut output, 0).unwrap();
    assert!(
        output[..reply]
            .windows(b"linux-exec-hello".len())
            .any(|s| s == b"linux-exec-hello")
    );
    unsafe {
        close(alive[0]);
        close(alive[1]);
        close(channel[0]);
        close(channel[1]);
    }
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn linux_kill_runner_helper() {
    unsafe extern "C" {
        safe fn getppid() -> c_int;
        safe fn kill(pid: c_int, signal: c_int) -> c_int;
        safe fn pause() -> c_int;
    }
    unsafe {
        kill(getppid(), 9);
        pause();
    }
}
