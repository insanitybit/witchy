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

unsafe fn close_inherited() -> bool {
    unsafe extern "C" {
        fn witchy_close_inherited_keeping(
            output: bool,
            input: bool,
            keep: *const c_int,
            count: u32,
        ) -> bool;
    }
    unsafe { witchy_close_inherited_keeping(true, true, ptr::null(), 0) }
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

pub(super) unsafe fn set_cwd_here(fd: c_int) -> bool {
    let broker = unsafe { &mut *(&raw mut BROKER) };
    if broker.cwd >= 0 {
        close(broker.cwd);
    }
    broker.cwd = fd;
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_exec_set_cwd(fd: c_int) -> bool {
    unsafe { set_cwd_here(fd) }
}

pub(super) unsafe fn request_here(
    channel: c_int,
    operation: u8,
    bytes: *mut u8,
    count: usize,
    capacity: usize,
    reply: *mut usize,
) -> bool {
    if reply.is_null() || (count != 0 && bytes.is_null()) {
        return false;
    }
    unsafe {
        *reply = 0;
    }
    if !exec_programs::configured() {
        return false;
    }
    let broker = unsafe { &mut *(&raw mut BROKER) };
    match operation {
        27 | 30 | 31 => {
            let input = if count == 0 {
                &[]
            } else {
                unsafe { core::slice::from_raw_parts(bytes, count) }
            };
            broker.pending.append(
                match operation {
                    27 => Kind::Arguments,
                    30 => Kind::Input,
                    _ => Kind::Environment,
                },
                input,
            )
        }
        28 => {
            if count != 11 || capacity < 11 || bytes.is_null() {
                return false;
            }
            let packet = unsafe { core::slice::from_raw_parts_mut(bytes, 11) };
            if packet[10] != 0 {
                return false;
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
            unsafe {
                *reply = 11;
            }
            true
        }
        29 => {
            let Some(capture) = broker.capture.as_ref() else {
                return true;
            };
            let out = capture.output.bytes();
            let err = capture.errors.bytes();
            let total = out.len() + err.len();
            if broker.output_at > total || (capacity != 0 && bytes.is_null()) {
                return false;
            }
            let amount = (total - broker.output_at).min(capacity);
            let mut copied = 0;
            if broker.output_at < out.len() {
                let first = (out.len() - broker.output_at).min(amount);
                if first != 0 {
                    unsafe {
                        ptr::copy_nonoverlapping(out.as_ptr().add(broker.output_at), bytes, first);
                    }
                }
                copied = first;
            }
            if copied < amount {
                let start = broker.output_at.saturating_sub(out.len());
                unsafe {
                    ptr::copy_nonoverlapping(
                        err.as_ptr().add(start),
                        bytes.add(copied),
                        amount - copied,
                    );
                }
            }
            broker.output_at += amount;
            unsafe {
                *reply = amount;
            }
            true
        }
        _ => false,
    }
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_exec_request(
    channel: c_int,
    operation: u8,
    bytes: *mut u8,
    count: usize,
    capacity: usize,
    reply: *mut usize,
) -> bool {
    unsafe { request_here(channel, operation, bytes, count, capacity, reply) }
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
        fn witchy_exec_setup(
            directory: c_int,
            names: *const *const c_char,
            count: u32,
            budget: u64,
        ) -> bool;
        fn witchy_exec_env_names(list: *const c_char) -> bool;
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn witchy_broker_exec_args(channel: c_int, bytes: *const u8, len: usize) -> c_int;
        fn witchy_broker_exec_run(
            channel: c_int,
            program: u32,
            timeout: u32,
            budget: u32,
            argc: u32,
            code: *mut i64,
            length: *mut u32,
            error_length: *mut u32,
        ) -> c_int;
        fn witchy_broker_exec_out(channel: c_int, bytes: *mut u8, capacity: usize) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = unsafe { open(parent.as_ptr(), 0x0110_0000) };
    assert!(directory >= 0);
    let programs = [name.as_ptr()];
    assert!(unsafe { witchy_exec_setup(directory, programs.as_ptr(), 1, 4096) });
    assert!(unsafe { witchy_exec_env_names(c"TERM".as_ptr()) });
    let mut channel = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
    let mut reply = 0;
    let mut argument = *b"--exact\0exec_broker::subprocess_helper\0--nocapture\0";
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            27,
            argument.as_mut_ptr(),
            argument.len(),
            argument.len(),
            &mut reply,
        )
    });
    let mut environment = *b"TERM=xterm\0";
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            31,
            environment.as_mut_ptr(),
            environment.len(),
            environment.len(),
            &mut reply,
        )
    });
    let mut packet = [0u8; 11];
    packet[1..5].copy_from_slice(&1000u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            28,
            packet.as_mut_ptr(),
            packet.len(),
            packet.len(),
            &mut reply,
        )
    });
    assert_eq!(reply, 11);
    assert_eq!(packet[0], 0, "run failed with status {}", packet[0]);
    assert_eq!(
        u16::from_be_bytes([packet[1], packet[2]]),
        0,
        "packet {packet:?}"
    );
    let mut output = [0u8; 1024];
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            29,
            output.as_mut_ptr(),
            0,
            output.len(),
            &mut reply,
        )
    });
    assert!(
        output[..reply]
            .windows(b"witchy-exec-hello".len())
            .any(|window| window == b"witchy-exec-hello")
    );
    let mut sleep = *b"--exact\0exec_broker::subprocess_helper\0--nocapture\0";
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            27,
            sleep.as_mut_ptr(),
            sleep.len(),
            sleep.len(),
            &mut reply,
        )
    });
    let mut environment = *b"TERM=slow\0";
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            31,
            environment.as_mut_ptr(),
            environment.len(),
            environment.len(),
            &mut reply,
        )
    });
    packet = [0; 11];
    packet[1..5].copy_from_slice(&50u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    assert!(unsafe {
        witchy_exec_request(
            channel[0],
            28,
            packet.as_mut_ptr(),
            packet.len(),
            packet.len(),
            &mut reply,
        )
    });
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
            if !unsafe {
                super::broker_transport::witchy_broker_transfer(
                    channel[1],
                    head.as_mut_ptr(),
                    ptr::null(),
                    4,
                )
            } {
                unsafe {
                    _exit(1);
                }
            }
            let count = u16::from_be_bytes([head[1], head[2]]) as usize;
            let mut data = [0u8; 16384];
            if count > data.len()
                || (head[0] != 29
                    && !unsafe {
                        super::broker_transport::witchy_broker_transfer(
                            channel[1],
                            data.as_mut_ptr(),
                            ptr::null(),
                            count,
                        )
                    })
            {
                unsafe {
                    _exit(2);
                }
            }
            let mut answer = 0usize;
            if !unsafe {
                witchy_exec_request(
                    channel[1],
                    head[0],
                    data.as_mut_ptr(),
                    if head[0] == 29 { 0 } else { count },
                    if head[0] == 29 { count } else { data.len() },
                    &mut answer,
                )
            } || !super::broker_transport::witchy_broker_reply(channel[1], 0, answer)
                || !unsafe {
                    super::broker_transport::witchy_broker_transfer(
                        channel[1],
                        ptr::null_mut(),
                        data.as_ptr(),
                        answer,
                    )
                }
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
        unsafe { witchy_broker_exec_args(channel[0], arguments.as_ptr(), arguments.len()) },
        0
    );
    let (mut code, mut out_len, mut err_len) = (0, 0, 0);
    assert_eq!(
        unsafe {
            witchy_broker_exec_run(
                channel[0],
                0,
                1000,
                1024,
                3,
                &mut code,
                &mut out_len,
                &mut err_len,
            )
        },
        0
    );
    assert_eq!(code, 0);
    let mut output = [0; 1024];
    let count = unsafe { witchy_broker_exec_out(channel[0], output.as_mut_ptr(), output.len()) };
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
        fn witchy_exec_setup(
            directory: c_int,
            names: *const *const c_char,
            count: u32,
            budget: u64,
        ) -> bool;
        safe fn witchy_exec_supervise() -> bool;
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let directory = unsafe { open(parent.as_ptr(), 0o40000 | 0o2000000) };
    assert!(directory >= 0);
    assert!(unsafe { witchy_exec_setup(directory, [name.as_ptr()].as_ptr(), 1, 4096) });
    assert!(unsafe { witchy_exec_supervise() });
    let mut alive = [-1; 2];
    assert_eq!(unsafe { pipe(alive.as_mut_ptr()) }, 0);
    super::exec_linux_limits::set_liveness(alive[0], alive[1]);
    let mut channel = [-1; 2];
    assert_eq!(unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) }, 0);
    let mut reply = 0;
    let mut args = *b"--exact\0exec_broker::linux_subprocess_helper\0--nocapture\0";
    assert!(unsafe {
        request_here(
            channel[0],
            27,
            args.as_mut_ptr(),
            args.len(),
            args.len(),
            &mut reply,
        )
    });
    let mut packet = [0u8; 11];
    packet[1..5].copy_from_slice(&1000u32.to_be_bytes());
    packet[5..9].copy_from_slice(&1024u32.to_be_bytes());
    packet[9] = 3;
    assert!(unsafe {
        request_here(
            channel[0],
            28,
            packet.as_mut_ptr(),
            packet.len(),
            packet.len(),
            &mut reply,
        )
    });
    assert_eq!(packet[0], 0, "run status {}", packet[0]);
    assert_eq!(u16::from_be_bytes([packet[1], packet[2]]), 0);
    let mut output = [0u8; 1024];
    assert!(unsafe {
        request_here(
            channel[0],
            29,
            output.as_mut_ptr(),
            0,
            output.len(),
            &mut reply,
        )
    });
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
