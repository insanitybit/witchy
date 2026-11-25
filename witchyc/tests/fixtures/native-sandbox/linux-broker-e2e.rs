#[path = "../../../runtime/native/errno.rs"]
mod errno;
#[path = "../../../runtime/native/global.rs"]
mod global;
#[path = "../../../runtime/native/broker_directory.rs"]
mod broker_directory;
#[path = "../../../runtime/native/broker_endpoint.rs"]
mod broker_endpoint;
#[path = "../../../runtime/native/broker_loop.rs"]
mod broker_loop;
#[path = "../../../runtime/native/broker_path.rs"]
mod broker_path;
#[path = "../../../runtime/native/broker_socket.rs"]
mod broker_socket;
#[path = "../../../runtime/native/broker_transport.rs"]
mod broker_transport;
#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/clock_random.rs"]
mod clock_random;
#[path = "../../../runtime/native/connection_queue.rs"]
mod connection_queue;
#[path = "../../../runtime/native/tls_grant.rs"]
mod tls_grant;
#[path = "../../../runtime/native/deadline.rs"]
mod deadline;
#[path = "../../../runtime/native/directory.rs"]
mod directory;
#[path = "../../../runtime/native/entry_budget.rs"]
mod entry_budget;
#[path = "../../../runtime/native/exec_arguments.rs"]
mod exec_arguments;
#[path = "../../../runtime/native/exec_broker.rs"]
mod exec_broker;
#[path = "../../../runtime/native/exec_capture.rs"]
mod exec_capture;
#[path = "../../../src/compiler/exec_env_policy.rs"]
mod exec_env_policy;
#[path = "../../../runtime/native/exec_environment.rs"]
mod exec_environment;
#[path = "../../../runtime/native/exec_linux_children.rs"]
mod exec_linux_children;
#[path = "../../../runtime/native/exec_linux_limits.rs"]
mod exec_linux_limits;
#[path = "../../../runtime/native/exec_linux_runner.rs"]
mod exec_linux_runner;
#[path = "../../../runtime/native/exec_linux_security.rs"]
mod exec_linux_security;
#[path = "../../../runtime/native/exec_linux_transport.rs"]
mod exec_linux_transport;
#[path = "../../../runtime/native/exec_pending.rs"]
mod exec_pending;
#[path = "../../../runtime/native/exec_programs.rs"]
mod exec_programs;
#[path = "../../../runtime/native/exec_temp.rs"]
mod exec_temp;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/file_reader.rs"]
mod file_reader;
#[path = "../../../runtime/native/input_stream.rs"]
mod input_stream;
#[path = "../../../runtime/native/io_budget.rs"]
mod io_budget;
#[path = "../../../runtime/native/launch_args.rs"]
mod launch_args;
#[path = "../../../runtime/native/limit_labels.rs"]
mod limit_labels;
#[path = "../../../runtime/native/linux_sandbox.rs"]
mod linux_sandbox;
#[path = "../../../runtime/native/linux_stat.rs"]
mod linux_stat;
#[path = "../../../runtime/native/secret_guard.rs"]
mod secret_guard;
#[path = "../../../runtime/native/socket_primitives.rs"]
mod socket_primitives;

#[test]
fn broker_cleanup_failure_is_reported() {
    use std::ffi::c_int;
    unsafe extern "C" {
        fn fork() -> c_int;
        fn pipe(fds: *mut c_int) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn _exit(status: c_int) -> !;
        fn witchy_broker_stop_checked(channel: *mut c_int, relay: c_int, interrupted: bool)
        -> bool;
    }
    for (status, expected) in [(0, true), (4, false)] {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0);
        let child = unsafe { fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe { _exit(status) }
        }
        unsafe { close(fds[1]) };
        assert_eq!(
            unsafe { witchy_broker_stop_checked(&mut fds[0], child, false) },
            expected
        );
        assert_eq!(fds[0], -1);
    }
}

#[test]
fn broker_reads_only_the_granted_directory() {
    use std::ffi::{CString, c_char, c_int};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
        fn witchy_net_start(
            endpoints: *const *const c_char,
            count: u32,
            listen: *const c_char,
            read: *const c_char,
            write: *const c_char,
            read_fd: c_int,
            write_fd: c_int,
            exec_fd: c_int,
            programs: *const *const c_char,
            programs_count: u32,
            output: *const c_char,
            identity: *const file_identity::Identity,
            entries: u64,
            data_only: bool,
            port: *mut u16,
            relay: *mut c_int,
        ) -> c_int;
        fn witchy_broker_file_close(channel: c_int, slot: u32) -> c_int;
    }
    let root = std::env::temp_dir().join(format!("witchy-linux-broker-e2e-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("allowed"), b"safe").unwrap();
    let name = CString::new(root.as_os_str().as_bytes()).unwrap();
    let fd = unsafe { open(name.as_ptr(), 0o40000 | 0o2000000) };
    assert!(fd >= 0);
    let mut port = 0;
    let mut relay = -1;
    let channel = unsafe {
        witchy_net_start(
            ptr::null(),
            0,
            ptr::null(),
            name.as_ptr(),
            ptr::null(),
            fd,
            -1,
            -1,
            ptr::null(),
            0,
            ptr::null(),
            ptr::null(),
            u64::MAX,
            false,
            &mut port,
            &mut relay,
        )
    };
    assert!(channel >= 0 && relay > 0, "broker failed to start");
    unsafe {
        close(fd);
    }
    let path = b"\x10allowed";
    assert_eq!(
        broker_transport::witchy_broker_file_open(channel, 0, path, 1),
        0
    );
    let mut bytes = [0; 8];
    assert_eq!(
        broker_transport::witchy_broker_file_read(channel, 0, &mut bytes),
        4
    );
    assert_eq!(&bytes[..4], b"safe");
    assert_eq!(unsafe { witchy_broker_file_close(channel, 0) }, 0);
    let denied = b"\x10../etc/passwd";
    assert_ne!(
        broker_transport::witchy_broker_file_open(channel, 0, denied, 1),
        0
    );
    unsafe {
        close(channel);
    }
    let mut status = 0;
    assert_eq!(unsafe { waitpid(relay, &raw mut status, 0) }, relay);
    assert_eq!(status, 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn confined_broker_runs_only_the_granted_executable() {
    use std::ffi::{CString, c_char, c_int};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;
    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
        fn witchy_net_start(
            endpoints: *const *const c_char,
            count: u32,
            listen: *const c_char,
            read: *const c_char,
            write: *const c_char,
            read_fd: c_int,
            write_fd: c_int,
            exec_fd: c_int,
            programs: *const *const c_char,
            programs_count: u32,
            output: *const c_char,
            identity: *const file_identity::Identity,
            entries: u64,
            data_only: bool,
            port: *mut u16,
            relay: *mut c_int,
        ) -> c_int;
    }
    let executable = std::env::current_exe().unwrap();
    let parent = CString::new(executable.parent().unwrap().as_os_str().as_bytes()).unwrap();
    let name = CString::new(executable.file_name().unwrap().as_bytes()).unwrap();
    let fd = unsafe { open(parent.as_ptr(), 0o40000 | 0o2000000) };
    assert!(fd >= 0);
    let mut port = 0;
    let mut relay = -1;
    let channel = unsafe {
        witchy_net_start(
            ptr::null(),
            0,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            -1,
            -1,
            fd,
            [name.as_ptr()].as_ptr(),
            1,
            ptr::null(),
            ptr::null(),
            u64::MAX,
            false,
            &mut port,
            &mut relay,
        )
    };
    assert!(channel >= 0 && relay > 0, "broker failed to start");
    unsafe {
        close(fd);
    }
    let args = b"--exact\0exec_broker::linux_subprocess_helper\0--nocapture\0";
    assert_eq!(
        broker_transport::witchy_broker_exec_args(channel, args),
        0
    );
    let mut code = 0;
    let mut size = 0;
    let mut errors = 0;
    assert_eq!(
        broker_transport::witchy_broker_exec_run(
            channel,
            0,
            1000,
            1024,
            3,
            &mut code,
            &mut size,
            &mut errors
        ),
        0
    );
    assert_eq!(code, 0);
    let mut output = [0u8; 1024];
    let got = broker_transport::witchy_broker_exec_out(channel, &mut output);
    assert!(
        got > 0
            && output[..got as usize]
                .windows(b"linux-exec-hello".len())
                .any(|s| s == b"linux-exec-hello")
    );
    assert_eq!(
        broker_transport::witchy_broker_exec_run(
            channel,
            1,
            1000,
            1024,
            0,
            &mut code,
            &mut size,
            &mut errors
        ),
        4
    );
    unsafe {
        close(channel);
    }
    let mut status = 0;
    assert_eq!(unsafe { waitpid(relay, &raw mut status, 0) }, relay);
    assert_eq!(status, 0);
}

#[test]
fn confined_broker_connects_to_the_granted_endpoint() {
    use std::ffi::{CString, c_char, c_int};
    use std::io::{Read, Write};
    use std::ptr;
    unsafe extern "C" {
        fn close(fd: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
        fn witchy_net_start(
            endpoints: *const *const c_char,
            count: u32,
            listen: *const c_char,
            read: *const c_char,
            write: *const c_char,
            read_fd: c_int,
            write_fd: c_int,
            exec_fd: c_int,
            programs: *const *const c_char,
            programs_count: u32,
            output: *const c_char,
            identity: *const file_identity::Identity,
            entries: u64,
            data_only: bool,
            port: *mut u16,
            relay: *mut c_int,
        ) -> c_int;
        fn witchy_net_connect(channel: c_int, slot: u32, endpoint: u32) -> c_int;
        fn witchy_net_close(channel: c_int, slot: u32) -> c_int;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = CString::new(listener.local_addr().unwrap().to_string()).unwrap();
    let names = [endpoint.as_ptr()];
    let mut port = 0;
    let mut relay = -1;
    let channel = unsafe {
        witchy_net_start(
            names.as_ptr(),
            1,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            -1,
            -1,
            -1,
            ptr::null(),
            0,
            ptr::null(),
            ptr::null(),
            u64::MAX,
            false,
            &mut port,
            &mut relay,
        )
    };
    assert!(channel >= 0 && relay > 0, "broker failed to start");
    assert_eq!(unsafe { witchy_net_connect(channel, 0, 0) }, 0);
    let (mut peer, _) = listener.accept().unwrap();
    assert_eq!(broker_transport::witchy_net_send(channel, 0, b"ping"), 4);
    let mut bytes = [0; 4];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"ping");
    peer.write_all(b"pong").unwrap();
    assert_eq!(
        broker_transport::witchy_net_receive(channel, 0, &mut bytes),
        4
    );
    assert_eq!(&bytes, b"pong");
    assert_eq!(unsafe { witchy_net_close(channel, 0) }, 0);
    unsafe {
        close(channel);
    }
    let mut status = 0;
    assert_eq!(unsafe { waitpid(relay, &raw mut status, 0) }, relay);
    assert_eq!(status, 0);
}
