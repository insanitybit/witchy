//! Socket operations used by the broker.

use super::global::Global;
use core::{
    ffi::{c_int, c_void},
    ptr,
};

#[repr(C)]
#[cfg(target_os = "macos")]
struct PollFd {
    fd: c_int,
    events: i16,
    returned: i16,
}
#[cfg(target_os = "linux")]
use super::socket_primitives::PollFd;

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn setsockopt(fd: c_int, level: c_int, option: c_int, value: *const c_void, len: u32) -> c_int;
    #[cfg(target_os = "macos")]
    fn poll(fds: *mut PollFd, count: u32, timeout: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn fcntl(fd: c_int, op: c_int, ...) -> c_int;
    safe fn close(fd: c_int) -> c_int;
}

fn suppress_sigpipe(fd: c_int) -> bool {
    #[cfg(target_os = "macos")]
    {
        let on: c_int = 1;
        unsafe {
            setsockopt(
                fd,
                0xffff,
                0x1022,
                (&raw const on).cast(),
                size_of::<c_int>() as u32,
            ) == 0
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = fd;
        true
    } // Linux transport sends with MSG_NOSIGNAL.
}

unsafe fn close_inherited(output: bool, input: bool, keep: *const c_int, count: u32) -> bool {
    unsafe extern "C" {
        fn witchy_close_inherited_keeping(
            output: bool,
            input: bool,
            keep: *const c_int,
            count: u32,
        ) -> bool;
    }
    unsafe { witchy_close_inherited_keeping(output, input, keep, count) }
}

#[cfg(target_os = "linux")]
unsafe fn confine_linux(
    root: c_int,
    write_root: c_int,
    output: c_int,
    endpoints: *const Endpoint,
    count: u32,
) -> bool {
    unsafe extern "C" {
        safe fn witchy_exec_start_runner() -> bool;
        safe fn witchy_exec_signal_descriptor() -> c_int;
        safe fn witchy_exec_runner_descriptor() -> c_int;
        fn witchy_confine_broker(
            root: c_int,
            write: c_int,
            output: c_int,
            ports: *const u16,
            count: u32,
            signal: c_int,
            message: c_int,
        ) -> bool;
    }
    if count > 16 || (count != 0 && endpoints.is_null()) {
        return false;
    }
    let mut ports = [0u16; 128];
    let mut used = 0usize;
    for i in 0..count as usize {
        let endpoint = unsafe { &*endpoints.add(i) };
        if endpoint.count as usize > endpoint.addresses.len() {
            return false;
        }
        for address in endpoint.addresses.iter().take(endpoint.count as usize) {
            ports[used] = u16::from_be_bytes([address.0[2], address.0[3]]);
            used += 1;
        }
    }
    unsafe {
        witchy_exec_start_runner()
            && witchy_confine_broker(
                root,
                write_root,
                output,
                ports.as_ptr(),
                used as u32,
                witchy_exec_signal_descriptor(),
                witchy_exec_runner_descriptor(),
            )
    }
}

pub(super) use super::socket_primitives::witchy_broker_readiness;
use super::socket_primitives::{
    witchy_broker_accept_stream, witchy_broker_bind_listener, witchy_broker_dial,
    witchy_broker_endpoint_address, witchy_broker_private_descriptor, witchy_broker_set_timeout,
};

use super::broker_endpoint::witchy_broker_resolve_endpoint;
pub(super) use super::broker_endpoint::{Address, Endpoint};

struct ServerRoots {
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: *mut u64,
    connection_entries: u64,
    outbound: [core::mem::MaybeUninit<Endpoint>; 16],
    outbound_count: u32,
}

static mut SERVER_ROOTS: ServerRoots = ServerRoots {
    root: -1,
    write_root: -1,
    combined: false,
    data_only: false,
    entries: ptr::null_mut(),
    connection_entries: u64::MAX,
    outbound: [const { core::mem::MaybeUninit::uninit() }; 16],
    outbound_count: 0,
};

static OUTPUT_PARENT: Global<c_int> = Global::new(-1);
static OUTPUT_EXCLUSIVE: Global<bool> = Global::new(false);
static EXEC_BUDGET: Global<u64> = Global::new(1048576);

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_output(parent: c_int, exclusive: bool) {
    OUTPUT_PARENT.set(parent);
    OUTPUT_EXCLUSIVE.set(exclusive);
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_exec_budget(budget: u64) {
    EXEC_BUDGET.set(budget);
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_server_endpoints(
    names: *const *const core::ffi::c_char,
    count: u32,
) -> bool {
    if count > 16 || (count != 0 && names.is_null()) {
        return false;
    }
    let roots = unsafe { &mut *(&raw mut SERVER_ROOTS) };
    for i in 0..count as usize {
        if !unsafe { witchy_broker_resolve_endpoint(*names.add(i), roots.outbound[i].as_mut_ptr()) }
        {
            return false;
        }
    }
    roots.outbound_count = count;
    true
}

#[unsafe(no_mangle)]
extern "C" fn witchy_broker_server_connection_entries(entries: u64) {
    unsafe {
        (*(&raw mut SERVER_ROOTS)).connection_entries = entries;
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_server_roots(
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: *mut u64,
) {
    let roots = unsafe { &mut *(&raw mut SERVER_ROOTS) };
    roots.root = root;
    roots.write_root = write_root;
    roots.combined = combined;
    roots.data_only = data_only;
    roots.entries = entries;
}

/// The endpoint was initialized by the Rust resolver before broker startup.
unsafe fn witchy_broker_dial_endpoint(endpoint: *const Endpoint) -> c_int {
    let endpoint = unsafe { &*endpoint };
    for (address, &len) in endpoint
        .addresses
        .iter()
        .zip(&endpoint.lengths)
        .take(endpoint.count as usize)
    {
        let stream = unsafe { witchy_broker_dial(address.0.as_ptr(), len) };
        if stream >= 0 {
            return stream;
        }
    }
    -1
}

/// The launcher provides a live C string and writable bound-port output.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_listen(address: *const core::ffi::c_char, port: *mut u16) -> c_int {
    let mut bound = Address([0; 128]);
    let size = unsafe { witchy_broker_endpoint_address(address, bound.0.as_mut_ptr(), true) };
    if size == 0 {
        return -1;
    }
    unsafe { witchy_broker_bind_listener(bound.0.as_mut_ptr(), size, port) }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_net_accept_connection(listener: c_int, timeout: u32) -> c_int {
    let mut timed_out = false;
    let stream = unsafe {
        witchy_broker_accept_stream(listener, timeout.min(5000) as c_int, &mut timed_out)
    };
    if stream < 0 {
        if timed_out { -3 } else { -1 }
    } else {
        witchy_broker_private_descriptor(stream)
    }
}

/// Consumes an accepted stream. On success returns the private channel and
/// writes the broker PID. Only the single-threaded supervisor calls this.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_connection_broker(
    stream: c_int,
    slot: u32,
    relay: *mut c_int,
) -> c_int {
    unsafe extern "C" {
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
        fn fork() -> c_int;
        safe fn dup2(source: c_int, target: c_int) -> c_int;
        fn signal(signal: c_int, handler: usize) -> usize;
        fn sigemptyset(set: *mut super::deadline::SignalSet) -> c_int;
        fn sigaddset(set: *mut super::deadline::SignalSet, signal: c_int) -> c_int;
        fn sigprocmask(
            how: c_int,
            set: *const super::deadline::SignalSet,
            old: *mut super::deadline::SignalSet,
        ) -> c_int;
        fn witchy_exec_descriptors(out: *mut c_int, capacity: u32) -> u32;
        safe fn _exit(code: c_int) -> !;
    }
    const AF_UNIX: c_int = 1;
    const SOCK_STREAM: c_int = 1;
    const SIGPIPE: c_int = 13;
    const SIG_IGN: usize = 1;
    const SIG_ERR: usize = usize::MAX;

    let mut channel = [-1; 2];
    if unsafe { socketpair(AF_UNIX, SOCK_STREAM, 0, channel.as_mut_ptr()) } != 0 {
        close(stream);
        return -1;
    }
    let [parent, worker] = channel.map(|fd| witchy_broker_private_descriptor(fd));
    if parent < 0 || worker < 0 || !suppress_sigpipe(parent) || !witchy_broker_set_timeout(worker) {
        close(parent);
        close(worker);
        close(stream);
        return -1;
    }
    let child = unsafe { fork() };
    if child == 0 {
        unsafe {
            close(parent);
            let roots = &*(&raw const SERVER_ROOTS);
            let mut keep = [-1; 19];
            keep[0] = worker;
            keep[1] = roots.root;
            keep[2] = roots.write_root;
            let count = witchy_exec_descriptors(keep.as_mut_ptr().add(3), 16);
            if dup2(stream, 0) < 0 || !close_inherited(false, true, keep.as_ptr(), count + 3) {
                _exit(2);
            }
            close(2);
            let mut stops = super::deadline::empty_signal_set();
            #[cfg(target_os = "macos")]
            const SIG_UNBLOCK: c_int = 2;
            #[cfg(target_os = "linux")]
            const SIG_UNBLOCK: c_int = 1;
            if sigemptyset(&mut stops) != 0
                || sigaddset(&mut stops, 15) != 0
                || sigaddset(&mut stops, 1) != 0
                || sigaddset(&mut stops, 2) != 0
                || signal(SIGPIPE, SIG_IGN) == SIG_ERR
                || signal(15, 0) == SIG_ERR
                || signal(1, 0) == SIG_ERR
                || signal(2, 0) == SIG_ERR
                || sigprocmask(SIG_UNBLOCK, &stops, ptr::null_mut()) != 0
            {
                _exit(2);
            }
            let staging = if roots.data_only && roots.write_root >= 0 {
                let fd = super::broker_directory::witchy_broker_open_staging(roots.write_root);
                if fd < 0 {
                    _exit(2);
                }
                fd
            } else {
                -1
            };
            #[cfg(target_os = "linux")]
            if !confine_linux(
                roots.root,
                roots.write_root,
                -1,
                roots.outbound.as_ptr().cast(),
                roots.outbound_count,
            ) {
                _exit(2);
            }
            super::entry_budget::set_connection(roots.connection_entries);
            super::broker_loop::relay_connection(
                worker,
                slot,
                roots.outbound.as_ptr().cast(),
                roots.outbound_count,
                roots.root,
                roots.write_root,
                roots.combined,
                roots.data_only,
                roots.entries,
                staging,
            );
            #[cfg(target_os = "linux")]
            {
                unsafe extern "C" {
                    safe fn witchy_exec_finish() -> bool;
                }
                _exit(if witchy_exec_finish() { 0 } else { 4 });
            }
            #[cfg(target_os = "macos")]
            _exit(0);
        }
    }
    close(stream);
    close(worker);
    if child < 0 {
        close(parent);
        return -1;
    }
    unsafe { relay.write(child) };
    parent
}

/// Starts the shared broker before guest entry. All pointer arguments must be
/// valid launcher-owned storage, with optional C strings represented by null.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_net_start(
    endpoints: *const *const core::ffi::c_char,
    endpoint_count: u32,
    listen_endpoint: *const core::ffi::c_char,
    dir_path: *const core::ffi::c_char,
    write_dir: *const core::ffi::c_char,
    root_fd: c_int,
    write_root_fd: c_int,
    exec_fd: c_int,
    programs: *const *const core::ffi::c_char,
    program_count: u32,
    output_path: *const core::ffi::c_char,
    directory_id: *const super::file_identity::Identity,
    entries: u64,
    data_only: bool,
    bound_port: *mut u16,
    relay: *mut c_int,
) -> c_int {
    use core::mem::MaybeUninit;
    unsafe extern "C" {
        fn socketpair(domain: c_int, kind: c_int, protocol: c_int, fds: *mut c_int) -> c_int;
        fn fork() -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
        fn signal(signal: c_int, handler: usize) -> usize;
        safe fn _exit(code: c_int) -> !;
        fn witchy_exec_setup(
            directory: c_int,
            names: *const *const core::ffi::c_char,
            count: u32,
            budget: u64,
        ) -> bool;
    }

    let mut outbound = [const { MaybeUninit::<Endpoint>::uninit() }; 16];
    if endpoint_count as usize > outbound.len() {
        return -1;
    }
    for (index, endpoint) in outbound
        .iter_mut()
        .take(endpoint_count as usize)
        .enumerate()
    {
        if !unsafe {
            witchy_broker_resolve_endpoint(endpoints.add(index).read(), endpoint.as_mut_ptr())
        } {
            return -1;
        }
    }
    let mut bind_address = Address([0; 128]);
    let bind_length = if listen_endpoint.is_null() {
        0
    } else {
        let size = unsafe {
            witchy_broker_endpoint_address(listen_endpoint, bind_address.0.as_mut_ptr(), true)
        };
        if size == 0 {
            return -1;
        }
        size
    };

    let mut channel = [-1; 2];
    if unsafe { socketpair(1, 1, 0, channel.as_mut_ptr()) } != 0 {
        return -1;
    }
    let [parent, worker] = channel;
    if unsafe {
        fcntl(parent, 2, 1 as c_int) < 0
            || fcntl(worker, 2, 1 as c_int) < 0
            || !suppress_sigpipe(parent)
    } {
        close(parent);
        close(worker);
        return -1;
    }
    let child = unsafe { fork() };
    if child == 0 {
        close(parent);
        let worker = if worker <= 2 {
            witchy_broker_private_descriptor(worker)
        } else {
            worker
        };
        let output_parent = OUTPUT_PARENT.get();
        let keep = [worker, root_fd, write_root_fd, exec_fd, output_parent];
        if worker < 0
            || [root_fd, write_root_fd, exec_fd, output_parent]
                .iter()
                .any(|&fd| (0..=2).contains(&fd))
            || !unsafe { close_inherited(false, false, keep.as_ptr(), keep.len() as u32) }
            || (exec_fd >= 0
                && !unsafe { witchy_exec_setup(exec_fd, programs, program_count, EXEC_BUDGET.get()) })
        {
            _exit(2)
        }
        close(2);
        if unsafe { signal(13, 1) } == usize::MAX || !witchy_broker_set_timeout(worker) {
            _exit(2)
        }
        let mut port = 0;
        let listener = if bind_length == 0 {
            -1
        } else {
            let fd = unsafe {
                witchy_broker_bind_listener(bind_address.0.as_mut_ptr(), bind_length, &mut port)
            };
            if fd < 0 {
                _exit(2)
            }
            fd
        };
        let mut roots = MaybeUninit::uninit();
        if !unsafe {
            super::broker_directory::open_roots(
                dir_path,
                write_dir,
                root_fd,
                write_root_fd,
                output_path,
                output_parent,
                directory_id,
                data_only,
                roots.as_mut_ptr(),
            )
        } {
            _exit(2)
        }
        let roots = unsafe { roots.assume_init() };
        #[cfg(target_os = "linux")]
        if !unsafe {
            confine_linux(
                roots.read,
                roots.write,
                roots.output,
                outbound.as_ptr().cast(),
                endpoint_count,
            )
        } {
            _exit(2)
        }
        let [hi, lo] = port.to_be_bytes();
        let ready = [0, hi, lo];
        if !unsafe {
            super::broker_transport::witchy_broker_transfer(
                worker,
                ptr::null_mut(),
                ready.as_ptr(),
                ready.len(),
            )
        } {
            _exit(2)
        }
        let mode = if !data_only {
            0
        } else if roots.write >= 0 {
            2
        } else {
            1
        };
        unsafe {
            super::broker_loop::relay_shared(
                worker,
                outbound.as_ptr().cast(),
                endpoint_count,
                listener,
                roots.read,
                roots.write,
                !dir_path.is_null() && dir_path == write_dir,
                roots.output,
                roots.name.as_ptr(),
                OUTPUT_EXCLUSIVE.get(),
                entries,
                mode,
                roots.staging,
            );
            #[cfg(target_os = "linux")]
            {
                unsafe extern "C" {
                    safe fn witchy_exec_finish() -> bool;
                }
                _exit(if witchy_exec_finish() { 0 } else { 4 });
            }
            #[cfg(target_os = "macos")]
            _exit(0);
        }
    }
    close(worker);
    if child < 0 {
        close(parent);
        return -1;
    }
    let parent = if parent <= 2 {
        witchy_broker_private_descriptor(parent)
    } else {
        parent
    };
    let mut ready = [0; 3];
    let mut startup = PollFd {
        fd: parent,
        events: 1,
        returned: 0,
    };
    if parent < 0
        || unsafe { poll(&mut startup, 1, 5000) } != 1
        || !unsafe {
            super::broker_transport::witchy_broker_transfer(
                parent,
                ready.as_mut_ptr(),
                ptr::null(),
                ready.len(),
            )
        }
        || ready[0] != 0
    {
        if parent >= 0 {
            close(parent);
        }
        let mut status = 0;
        while unsafe { waitpid(child, &mut status, 0) < 0 && super::errno::get() == 4 } {}
        return -1;
    }
    unsafe {
        bound_port.write(u16::from_be_bytes([ready[1], ready[2]]));
        relay.write(child);
    }
    parent
}

/// Dispatches a stream frame after the C relay verifies the slot index.
/// The descriptor slot is exclusive, and scratch permits 16 KiB of I/O.
pub(super) unsafe fn witchy_broker_stream_request(
    channel: c_int,
    endpoints: *const Endpoint,
    endpoint_count: u32,
    listener: c_int,
    slot: *mut c_int,
    operation: u8,
    count: usize,
    bytes: *mut u8,
) -> bool {
    use super::broker_transport::{witchy_broker_reply, witchy_broker_transfer};
    unsafe extern "C" {
        fn send(fd: c_int, bytes: *const c_void, len: usize, flags: c_int) -> isize;
        fn recv(fd: c_int, bytes: *mut u8, len: usize, flags: c_int) -> isize;
        fn read(fd: c_int, bytes: *mut c_void, len: usize) -> isize;
    }
    const EINTR: c_int = 4;
    #[cfg(target_os = "macos")]
    const EAGAIN: c_int = 35;
    #[cfg(target_os = "linux")]
    const EAGAIN: c_int = 11;
    #[cfg(target_os = "macos")]
    const DONTWAIT: c_int = 0x80;
    #[cfg(target_os = "linux")]
    const DONTWAIT: c_int = 0x40;
    let mut stream = unsafe { slot.read() };
    let accepting = matches!(operation, 5 | 6);
    let opening = operation == 1 || accepting;
    if count > 16384
        || ((accepting || operation == 4) && count != 0)
        || !(1..=8).contains(&operation)
        || (if opening {
            stream >= 0
        } else {
            stream < 0 && operation != 4
        })
        || (operation == 1 && count >= endpoint_count as usize)
        || (accepting && listener < 0)
    {
        witchy_broker_reply(channel, 2, 0);
        return false;
    }

    let mut payload = 0;
    let (status, size) = match operation {
        1 => {
            stream = unsafe { witchy_broker_dial_endpoint(endpoints.add(count)) };
            (u8::from(stream < 0), 0)
        }
        5 | 6 => {
            let mut timed_out = false;
            stream = unsafe {
                witchy_broker_accept_stream(
                    listener,
                    if operation == 5 { 5000 } else { 0 },
                    &mut timed_out,
                )
            };
            (
                if stream >= 0 {
                    0
                } else if timed_out {
                    3
                } else {
                    1
                },
                0,
            )
        }
        4 => {
            let closed = stream < 0 || close(stream) == 0;
            stream = -1;
            (u8::from(!closed), 0)
        }
        2 | 3 | 7 | 8 => {
            let sending = matches!(operation, 2 | 7);
            if sending && !unsafe { witchy_broker_transfer(channel, bytes, ptr::null(), count) } {
                return false;
            }
            if count == 0 && operation != 3 {
                (0, 0)
            } else {
                loop {
                    let result = unsafe {
                        match operation {
                            2 => send(stream, bytes.cast(), count, 0),
                            7 => send(stream, bytes.cast(), count, DONTWAIT),
                            8 => recv(stream, bytes, count, DONTWAIT),
                            _ => read(stream, bytes.cast(), count),
                        }
                    };
                    if result >= 0 && result as usize <= count {
                        let size = result as usize;
                        if !sending {
                            payload = size;
                        }
                        break (0, size);
                    }
                    let error = super::errno::get();
                    if result < 0 && error == EINTR {
                        continue;
                    }
                    break (
                        if result < 0 && matches!(operation, 7 | 8) && error == EAGAIN {
                            4
                        } else {
                            1
                        },
                        0,
                    );
                }
            }
        }
        _ => return false,
    };
    unsafe { slot.write(stream) };
    witchy_broker_reply(channel, status, size)
        && (payload == 0
            || unsafe { witchy_broker_transfer(channel, ptr::null_mut(), bytes, payload) })
}

/// Idle guests can compute indefinitely. Once the header begins, the socket
/// receive timeout bounds pauses inside the frame.
pub(super) unsafe fn witchy_broker_read_request(
    channel: c_int,
    operation: *mut u8,
    count: *mut usize,
    slot: *mut u32,
) -> bool {
    let mut ready = PollFd {
        fd: channel,
        events: 1,
        returned: 0,
    };
    loop {
        let result = unsafe { poll(&mut ready, 1, -1) };
        if result == 1 {
            break;
        }
        if result >= 0 || super::errno::get() != 4 {
            return false;
        }
    }
    let mut header = [0; 4];
    if !unsafe {
        super::broker_transport::witchy_broker_transfer(
            channel,
            header.as_mut_ptr(),
            ptr::null(),
            header.len(),
        )
    } {
        return false;
    }
    let [opcode, hi, lo, source] = header;
    unsafe {
        operation.write(opcode);
        count.write(usize::from(u16::from_be_bytes([hi, lo])));
        slot.write(u32::from(source));
    }
    true
}

/// The launcher owns this channel and is the only process that reaps relay.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_stop(channel: *mut c_int, relay: c_int, interrupted: bool) {
    unsafe { witchy_broker_stop_checked(channel, relay, interrupted) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_stop_checked(
    channel: *mut c_int,
    relay: c_int,
    interrupted: bool,
) -> bool {
    unsafe extern "C" {
        safe fn kill(pid: c_int, signal: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn waitid(
            kind: c_int,
            id: u32,
            info: *mut super::exec_capture::SignalInfo,
            options: c_int,
        ) -> c_int;
        safe fn usleep(microseconds: u32) -> c_int;
        safe fn witchy_exec_sweep(pid: c_int);
    }
    let channel = unsafe { &mut *channel };
    if *channel < 0 {
        return true;
    }
    if interrupted && relay > 0 {
        kill(relay, 15);
    }
    close(core::mem::replace(channel, -1));
    if relay > 0 {
        let mut status = 0;
        if interrupted {
            for _ in 0..200 {
                let done = unsafe { waitpid(relay, &mut status, 1) };
                if done == relay || (done < 0 && super::errno::get() != 4) {
                    return !(done == relay && status & 0x7f == 0 && (status >> 8) & 0xff == 4);
                }
                usleep(5000);
            }
            kill(relay, 9);
            let mut info: super::exec_capture::SignalInfo = unsafe { core::mem::zeroed() };
            #[cfg(target_os = "macos")]
            const WNOWAIT: c_int = 0x20;
            #[cfg(target_os = "linux")]
            const WNOWAIT: c_int = 0x01000000;
            while unsafe { waitid(1, relay as u32, &mut info, 4 | WNOWAIT) } < 0
                && super::errno::get() == 4
            {}
            witchy_exec_sweep(relay);
        }
        let done = loop {
            let done = unsafe { waitpid(relay, &mut status, 0) };
            if done >= 0 || super::errno::get() != 4 {
                break done;
            }
        };
        return !(done == relay && status & 0x7f == 0 && (status >> 8) & 0xff == 4);
    }
    true
}

static mut LAUNCH_CHANNEL: c_int = -1;
static mut LAUNCH_RELAY: c_int = 0;

extern "C" fn stop_launched_broker() {
    if !unsafe { witchy_broker_stop_checked(&raw mut LAUNCH_CHANNEL, LAUNCH_RELAY, false) } {
        unsafe extern "C" {
            fn fflush(stream: *mut core::ffi::c_void) -> c_int;
            safe fn _exit(status: c_int) -> !;
        }
        unsafe {
            fflush(core::ptr::null_mut());
            _exit(2)
        }
    }
}

const ENDPOINT_CAPACITY: usize = 16;
static mut LAUNCH_ENDPOINTS: [*const core::ffi::c_char; ENDPOINT_CAPACITY] =
    [core::ptr::null(); ENDPOINT_CAPACITY];
static mut LAUNCH_ENDPOINT_COUNT: u32 = 0;

pub(super) fn launch_endpoints() -> (*const *const core::ffi::c_char, u32) {
    ((&raw const LAUNCH_ENDPOINTS).cast(), unsafe {
        LAUNCH_ENDPOINT_COUNT
    })
}

pub(super) unsafe fn append_launch_endpoint(text: *const core::ffi::c_char) -> bool {
    if text.is_null()
        || unsafe { core::ffi::CStr::from_ptr(text) }
            .to_bytes()
            .is_empty()
    {
        return false;
    }
    let count = unsafe { LAUNCH_ENDPOINT_COUNT } as usize;
    if count >= ENDPOINT_CAPACITY {
        return false;
    }
    unsafe {
        (*(&raw mut LAUNCH_ENDPOINTS))[count] = text;
        LAUNCH_ENDPOINT_COUNT += 1;
    }
    true
}

/// The launcher supplies writable, NUL-terminated argv storage. On success
/// each separator is replaced with a NUL and the table borrows that storage.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_endpoints(text: *mut core::ffi::c_char) -> bool {
    let mut starts = [0usize; ENDPOINT_CAPACITY];
    let mut count = 0;
    let mut offset = 0;
    let bytes = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
    for part in bytes.split(|&byte| byte == b',') {
        if part.is_empty() {
            return false;
        }
        let Some(start) = starts.get_mut(count) else {
            return false;
        };
        *start = offset;
        offset += part.len() + 1;
        count += 1;
    }
    let endpoints = unsafe { &mut *(&raw mut LAUNCH_ENDPOINTS) };
    for (destination, start) in endpoints.iter_mut().zip(starts.into_iter().take(count)) {
        unsafe {
            if start != 0 {
                text.add(start - 1).write(0);
            }
            *destination = text.add(start);
        }
    }
    unsafe { LAUNCH_ENDPOINT_COUNT = count as u32 };
    true
}

/// Endpoint and directory arguments are live launcher strings, with null
/// representing an absent grant. The launcher invokes this once before entry.
pub(super) unsafe fn launch_broker(
    listener: *const core::ffi::c_char,
    read_dir: *const core::ffi::c_char,
    write_dir: *const core::ffi::c_char,
    root_fd: c_int,
    write_root_fd: c_int,
    exec_fd: c_int,
    programs: *const *const core::ffi::c_char,
    program_count: u32,
    output: *const core::ffi::c_char,
    identity: *const super::file_identity::Identity,
    entries: u64,
    data_only: bool,
    stderr: *mut core::ffi::c_void,
) -> c_int {
    unsafe extern "C" {
        fn atexit(callback: extern "C" fn()) -> c_int;
        fn fputs(text: *const core::ffi::c_char, stream: *mut core::ffi::c_void) -> c_int;
    }
    let endpoints = (&raw const LAUNCH_ENDPOINTS).cast();
    let endpoint_count = unsafe { LAUNCH_ENDPOINT_COUNT };
    let mut port = 0;
    let channel = unsafe {
        witchy_net_start(
            endpoints,
            endpoint_count,
            listener,
            read_dir,
            write_dir,
            root_fd,
            write_root_fd,
            exec_fd,
            programs,
            program_count,
            output,
            identity,
            entries,
            data_only,
            &mut port,
            &raw mut LAUNCH_RELAY,
        )
    };
    if exec_fd >= 0 {
        close(exec_fd);
    }
    if root_fd >= 0 {
        close(root_fd);
    }
    if write_root_fd >= 0 && write_root_fd != root_fd {
        close(write_root_fd);
    }
    let output_parent = OUTPUT_PARENT.get();
    if output_parent >= 0 {
        close(output_parent);
    }
    if channel < 0 {
        unsafe {
            fputs(c"native I/O requires valid resolvable endpoint or directory grants and a working broker\n".as_ptr(), stderr)
        };
        return -1;
    }
    unsafe { LAUNCH_CHANNEL = channel };
    if unsafe { atexit(stop_launched_broker) } != 0 {
        stop_launched_broker();
        return -1;
    }
    super::file_reader::bind_channel(channel);
    super::connection_queue::configure_network(channel, endpoints, endpoint_count, listener, port);
    channel
}
