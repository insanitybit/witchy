//! Socket operations used by the broker.

use super::global::{Global, State, Table};
use super::sys::{self, PollFd};
use core::ffi::{CStr, c_char, c_int};

unsafe extern "C" {
    safe fn close(fd: c_int) -> c_int;
}

fn suppress_sigpipe(fd: c_int) -> bool {
    #[cfg(target_os = "macos")]
    {
        let on: c_int = 1;
        sys::setsockopt(fd, 0xffff, 0x1022, &on) == 0
    }
    #[cfg(target_os = "linux")]
    {
        let _ = fd;
        true
    } // Linux transport sends with MSG_NOSIGNAL.
}

fn close_inherited(output: bool, input: bool, keep: &[c_int]) -> bool {
    #[cfg(target_os = "linux")]
    use super::linux_sandbox::witchy_close_inherited_keeping;
    #[cfg(target_os = "macos")]
    use super::seatbelt_fds::witchy_close_inherited_keeping;
    witchy_close_inherited_keeping(output, input, keep)
}

#[cfg(target_os = "linux")]
fn confine_linux(root: c_int, write_root: c_int, output: c_int, endpoints: &[Endpoint]) -> bool {
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
    if endpoints.len() > 16 {
        return false;
    }
    let mut ports = [0u16; 128];
    let mut used = 0usize;
    for endpoint in endpoints {
        if endpoint.count as usize > endpoint.addresses.len() {
            return false;
        }
        for address in endpoint.addresses.iter().take(endpoint.count as usize) {
            ports[used] = u16::from_be_bytes([address.0[2], address.0[3]]);
            used += 1;
        }
    }
    witchy_exec_start_runner()
        // SAFETY: the first `used` places of `ports` are filled.
        && unsafe {
            witchy_confine_broker(
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

#[derive(Clone, Copy)]
struct ServerRoots {
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    connection_entries: u64,
}

static SERVER_ROOTS: Global<ServerRoots> = Global::new(ServerRoots {
    root: -1,
    write_root: -1,
    combined: false,
    data_only: false,
    connection_entries: u64::MAX,
});
/// The directory budget that connection brokers share, when there is one. The
/// supervisor never reads it: each forked broker takes its own copy of the handle.
static SERVER_ENTRIES: State<Option<sys::SharedWord>> = State::new(None);
static SERVER_OUTBOUND: Table<Endpoint, ENDPOINT_CAPACITY> = Table::new(Endpoint::EMPTY);

/// The resolved endpoints of connection brokers, copied to `out`, and their count.
fn server_outbound(out: &mut [Endpoint; ENDPOINT_CAPACITY]) -> usize {
    let mut count = 0;
    for (slot, endpoint) in out.iter_mut().zip(SERVER_OUTBOUND.iter()) {
        *slot = endpoint;
        count += 1;
    }
    count
}

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

/// Resolves the launch endpoints for the brokers of connections. False, with
/// the endpoints in use left as they were, when one does not resolve.
pub(super) fn witchy_broker_server_endpoints() -> bool {
    let mut resolved = [Endpoint::EMPTY; ENDPOINT_CAPACITY];
    let mut count = 0;
    for (slot, name) in resolved.iter_mut().zip(LAUNCH_ENDPOINTS.iter()) {
        let Some(endpoint) = witchy_broker_resolve_endpoint(name) else {
            return false;
        };
        *slot = endpoint;
        count += 1;
    }
    SERVER_OUTBOUND.clear();
    for endpoint in resolved.into_iter().take(count) {
        SERVER_OUTBOUND.push(endpoint);
    }
    true
}

pub(super) fn witchy_broker_server_connection_entries(entries: u64) {
    SERVER_ROOTS.set(ServerRoots {
        connection_entries: entries,
        ..SERVER_ROOTS.get()
    });
}

/// `entries` is the directory budget that connection brokers share and write
/// through, or `None` for no limit.
pub(super) fn witchy_broker_server_roots(
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: Option<sys::SharedWord>,
) {
    SERVER_ROOTS.set(ServerRoots {
        root,
        write_root,
        combined,
        data_only,
        ..SERVER_ROOTS.get()
    });
    SERVER_ENTRIES.with(|shared| *shared = entries);
}

/// The endpoint was initialized by the Rust resolver before broker startup.
fn witchy_broker_dial_endpoint(endpoint: &Endpoint) -> c_int {
    for (address, &len) in endpoint
        .addresses
        .iter()
        .zip(&endpoint.lengths)
        .take(endpoint.count as usize)
    {
        // The resolver stores no length past the end of an address.
        let Some(address) = address.0.get(..len as usize) else {
            continue;
        };
        let stream = witchy_broker_dial(address);
        if stream >= 0 {
            return stream;
        }
    }
    -1
}

/// Binds the listener `address` names and writes the bound port on success.
pub(super) fn witchy_net_listen(address: &CStr, port: &mut u16) -> c_int {
    let mut bound = Address([0; 128]);
    let size = witchy_broker_endpoint_address(address, &mut bound.0, true);
    if size == 0 {
        return -1;
    }
    witchy_broker_bind_listener(&mut bound.0, size, port)
}

pub(super) fn witchy_net_accept_connection(listener: c_int, timeout: u32) -> c_int {
    let mut timed_out = false;
    let stream = witchy_broker_accept_stream(listener, timeout.min(5000) as c_int, &mut timed_out);
    if stream < 0 {
        if timed_out { -3 } else { -1 }
    } else {
        witchy_broker_private_descriptor(stream)
    }
}

/// Consumes an accepted stream. On success returns the private channel and
/// writes the broker PID. Only the single-threaded supervisor calls this.
pub(super) fn witchy_net_connection_broker(stream: c_int, slot: u32, relay: &mut c_int) -> c_int {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn dup2(source: c_int, target: c_int) -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    const AF_UNIX: c_int = 1;
    const SOCK_STREAM: c_int = 1;
    const SIGPIPE: c_int = 13;

    let mut channel = [-1; 2];
    if sys::socketpair(AF_UNIX, SOCK_STREAM, 0, &mut channel) != 0 {
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
    // SAFETY: the supervisor has one thread, so the child may run any code.
    let child = unsafe { fork() };
    if child == 0 {
        close(parent);
        let roots = SERVER_ROOTS.get();
        let mut keep = [-1; 19];
        keep[0] = worker;
        keep[1] = roots.root;
        keep[2] = roots.write_root;
        let count = super::exec_programs::witchy_exec_descriptors(&mut keep[3..]);
        if dup2(stream, 0) < 0 || !close_inherited(false, true, &keep[..count + 3]) {
            _exit(2);
        }
        close(2);
        // The stop signals the supervisor blocked around the fork: 15, 1 and 2.
        let Some(stops) = super::connection_queue::stop_signal_set() else {
            _exit(2);
        };
        if !sys::ignore_signal(SIGPIPE)
            || !sys::default_signal(15)
            || !sys::default_signal(1)
            || !sys::default_signal(2)
            || !super::connection_queue::unblock_signals(&stops)
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
        let mut outbound = [Endpoint::EMPTY; ENDPOINT_CAPACITY];
        let outbound_count = server_outbound(&mut outbound);
        let outbound = &outbound[..outbound_count];
        #[cfg(target_os = "linux")]
        if !confine_linux(roots.root, roots.write_root, -1, outbound) {
            _exit(2);
        }
        super::entry_budget::set_connection(roots.connection_entries);
        // This child's copy of the handle. The supervisor is not using the
        // budget when it forks, and the relay below is this process's one user.
        let mut entries = SERVER_ENTRIES.with(Option::take);
        super::broker_loop::relay_connection(
            worker,
            slot,
            outbound,
            roots.root,
            roots.write_root,
            roots.combined,
            roots.data_only,
            // SAFETY: the supervisor runs one connection broker at a time and
            // reaps it before starting the next, and reads the word in none
            // of that time, so this process is its one user.
            entries.as_mut().map(|word| unsafe { word.as_mut() }),
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
    close(stream);
    close(worker);
    if child < 0 {
        close(parent);
        return -1;
    }
    *relay = child;
    parent
}

/// Starts the shared broker before guest entry and writes its bound port and
/// PID on success. Each pointer argument is null, for a grant that is absent,
/// or valid launcher-owned storage: C strings, `program_count` program names,
/// and one directory identity.
pub(super) unsafe fn witchy_net_start(
    endpoints: &[&CStr],
    listen_endpoint: Option<&CStr>,
    dir_path: *const c_char,
    write_dir: *const c_char,
    root_fd: c_int,
    write_root_fd: c_int,
    exec_fd: c_int,
    programs: *const *const c_char,
    program_count: u32,
    output_path: *const c_char,
    directory_id: *const super::file_identity::Identity,
    entries: u64,
    data_only: bool,
    bound_port: &mut u16,
    relay: &mut c_int,
) -> c_int {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn _exit(code: c_int) -> !;
    }

    let mut outbound = [Endpoint::EMPTY; ENDPOINT_CAPACITY];
    if endpoints.len() > outbound.len() {
        return -1;
    }
    for (endpoint, name) in outbound.iter_mut().zip(endpoints) {
        let Some(resolved) = witchy_broker_resolve_endpoint(name) else {
            return -1;
        };
        *endpoint = resolved;
    }
    // The length check above keeps this within the array.
    let outbound = &outbound[..endpoints.len()];
    let mut bind_address = Address([0; 128]);
    let bind_length = match listen_endpoint {
        None => 0,
        Some(listen_endpoint) => {
            let size = witchy_broker_endpoint_address(listen_endpoint, &mut bind_address.0, true);
            if size == 0 {
                return -1;
            }
            size
        }
    };

    let mut channel = [-1; 2];
    if sys::socketpair(1, 1, 0, &mut channel) != 0 {
        return -1;
    }
    let [parent, worker] = channel;
    if sys::set_descriptor_flags(parent, 1) < 0
        || sys::set_descriptor_flags(worker, 1) < 0
        || !suppress_sigpipe(parent)
    {
        close(parent);
        close(worker);
        return -1;
    }
    // SAFETY: the launcher has one thread, so the child may run any code.
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
            || !close_inherited(false, false, &keep)
            || (exec_fd >= 0
                // SAFETY: the launcher passes exec_guest's table of `program_count` live names.
                && !unsafe {
                    super::exec_programs::setup_from_table(
                        exec_fd,
                        programs,
                        program_count,
                        EXEC_BUDGET.get(),
                    )
                })
        {
            _exit(2)
        }
        close(2);
        if !sys::ignore_signal(13) || !witchy_broker_set_timeout(worker) {
            _exit(2)
        }
        let mut port = 0;
        let listener = if bind_length == 0 {
            -1
        } else {
            let fd = witchy_broker_bind_listener(&mut bind_address.0, bind_length, &mut port);
            if fd < 0 {
                _exit(2)
            }
            fd
        };
        // SAFETY: the caller passes each path as null or a live C string.
        let [read_path, write_path, output_name] = [dir_path, write_dir, output_path]
            .map(|path| (!path.is_null()).then(|| unsafe { CStr::from_ptr(path) }));
        let Some(roots) = super::broker_directory::open_roots(
            read_path,
            write_path,
            root_fd,
            write_root_fd,
            output_name,
            output_parent,
            // SAFETY: the caller passes null or one readable identity.
            unsafe { directory_id.as_ref() },
            data_only,
        ) else {
            _exit(2)
        };
        #[cfg(target_os = "linux")]
        if !confine_linux(roots.read, roots.write, roots.output, outbound) {
            _exit(2)
        }
        let [hi, lo] = port.to_be_bytes();
        let ready = [0, hi, lo];
        if !super::broker_transport::witchy_broker_send(worker, &ready) {
            _exit(2)
        }
        let mode = if !data_only {
            0
        } else if roots.write >= 0 {
            2
        } else {
            1
        };
        super::broker_loop::relay_shared(
            worker,
            outbound,
            listener,
            roots.read,
            roots.write,
            !dir_path.is_null() && dir_path == write_dir,
            roots.output,
            // The name `open_roots` wrote, which ends before the last byte.
            CStr::from_bytes_until_nul(&roots.name).ok(),
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
        || sys::poll(core::slice::from_mut(&mut startup), 5000) != 1
        || !super::broker_transport::witchy_broker_receive(parent, &mut ready)
        || ready[0] != 0
    {
        if parent >= 0 {
            close(parent);
        }
        let mut status = 0;
        while sys::waitpid(child, Some(&mut status), 0) < 0 && super::errno::get() == 4 {}
        return -1;
    }
    *bound_port = u16::from_be_bytes([ready[1], ready[2]]);
    *relay = child;
    parent
}

/// Dispatches a stream frame after the relay verifies the slot index.
/// `slot` is that stream's descriptor, and `bytes` the relay's scratch.
pub(super) fn witchy_broker_stream_request(
    channel: c_int,
    endpoints: &[Endpoint],
    listener: c_int,
    slot: &mut c_int,
    operation: u8,
    count: usize,
    bytes: &mut [u8; 16384],
) -> bool {
    use super::broker_transport::{witchy_broker_receive, witchy_broker_reply, witchy_broker_send};
    const EINTR: c_int = 4;
    #[cfg(target_os = "macos")]
    const EAGAIN: c_int = 35;
    #[cfg(target_os = "linux")]
    const EAGAIN: c_int = 11;
    #[cfg(target_os = "macos")]
    const DONTWAIT: c_int = 0x80;
    #[cfg(target_os = "linux")]
    const DONTWAIT: c_int = 0x40;
    let mut stream = *slot;
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
        || (operation == 1 && count >= endpoints.len())
        || (accepting && listener < 0)
    {
        witchy_broker_reply(channel, 2, 0);
        return false;
    }

    let mut payload = 0;
    let (status, size) = match operation {
        1 => {
            // The check above keeps `count` within `endpoints`.
            stream = endpoints.get(count).map_or(-1, witchy_broker_dial_endpoint);
            (u8::from(stream < 0), 0)
        }
        5 | 6 => {
            let mut timed_out = false;
            stream = witchy_broker_accept_stream(
                listener,
                if operation == 5 { 5000 } else { 0 },
                &mut timed_out,
            );
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
            if sending && !witchy_broker_receive(channel, &mut bytes[..count]) {
                return false;
            }
            if count == 0 && operation != 3 {
                (0, 0)
            } else {
                loop {
                    // The first check keeps `count` within the scratch.
                    let result = match operation {
                        2 => sys::send(stream, &bytes[..count], 0),
                        7 => sys::send(stream, &bytes[..count], DONTWAIT),
                        8 => sys::recv(stream, &mut bytes[..count], DONTWAIT),
                        _ => sys::read(stream, &mut bytes[..count]),
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
    *slot = stream;
    witchy_broker_reply(channel, status, size)
        && (payload == 0 || witchy_broker_send(channel, &bytes[..payload]))
}

/// The next frame's operation, count and slot, or `None` when the channel ends.
/// Idle guests can compute indefinitely. Once the header begins, the socket
/// receive timeout bounds pauses inside the frame.
pub(super) fn witchy_broker_read_request(channel: c_int) -> Option<(u8, usize, u32)> {
    let mut ready = PollFd {
        fd: channel,
        events: 1,
        returned: 0,
    };
    loop {
        let result = sys::poll(core::slice::from_mut(&mut ready), -1);
        if result == 1 {
            break;
        }
        if result >= 0 || super::errno::get() != 4 {
            return None;
        }
    }
    let mut header = [0; 4];
    if !super::broker_transport::witchy_broker_receive(channel, &mut header) {
        return None;
    }
    let [opcode, hi, lo, source] = header;
    Some((
        opcode,
        usize::from(u16::from_be_bytes([hi, lo])),
        u32::from(source),
    ))
}

/// `waitid` for the process `pid`: what the call returned, and the PID it reported.
pub(super) fn wait_process(pid: c_int, options: c_int) -> (c_int, c_int) {
    let mut info = sys::SignalInfo::zeroed();
    let result = sys::waitid(1, pid as u32, &mut info, options);
    (result, info.pid)
}

/// The launcher owns this channel and is the only process that reaps relay.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_broker_stop(channel: *mut c_int, relay: c_int, interrupted: bool) {
    // SAFETY: the caller passes its own channel, which nothing else refers to.
    witchy_broker_stop_checked(unsafe { &mut *channel }, relay, interrupted);
}

/// Closes the channel and reaps `relay`. False when the relay reported that
/// what it started could not be stopped.
pub(super) fn witchy_broker_stop_checked(
    channel: &mut c_int,
    relay: c_int,
    interrupted: bool,
) -> bool {
    unsafe extern "C" {
        safe fn kill(pid: c_int, signal: c_int) -> c_int;
        safe fn usleep(microseconds: u32) -> c_int;
        safe fn witchy_exec_sweep(pid: c_int);
    }
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
                let done = sys::waitpid(relay, Some(&mut status), 1);
                if done == relay || (done < 0 && super::errno::get() != 4) {
                    return !(done == relay && status & 0x7f == 0 && (status >> 8) & 0xff == 4);
                }
                usleep(5000);
            }
            kill(relay, 9);
            #[cfg(target_os = "macos")]
            const WNOWAIT: c_int = 0x20;
            #[cfg(target_os = "linux")]
            const WNOWAIT: c_int = 0x01000000;
            while wait_process(relay, 4 | WNOWAIT).0 < 0 && super::errno::get() == 4 {}
            witchy_exec_sweep(relay);
        }
        let done = loop {
            let done = sys::waitpid(relay, Some(&mut status), 0);
            if done >= 0 || super::errno::get() != 4 {
                break done;
            }
        };
        return !(done == relay && status & 0x7f == 0 && (status >> 8) & 0xff == 4);
    }
    true
}

static LAUNCH_CHANNEL: Global<c_int> = Global::new(-1);
static LAUNCH_RELAY: Global<c_int> = Global::new(0);

extern "C" fn stop_launched_broker() {
    let mut channel = LAUNCH_CHANNEL.get();
    let stopped = witchy_broker_stop_checked(&mut channel, LAUNCH_RELAY.get(), false);
    LAUNCH_CHANNEL.set(channel);
    if !stopped {
        unsafe extern "C" {
            fn fflush(stream: *mut core::ffi::c_void) -> c_int;
            safe fn _exit(status: c_int) -> !;
        }
        // SAFETY: a null stream asks for every open output stream to be flushed.
        unsafe { fflush(core::ptr::null_mut()) };
        _exit(2)
    }
}

const ENDPOINT_CAPACITY: usize = 16;
// Each is launcher text that lives as long as the process.
static LAUNCH_ENDPOINTS: Table<&'static CStr, ENDPOINT_CAPACITY> = Table::new(c"");

pub(super) fn launch_endpoint_count() -> usize {
    LAUNCH_ENDPOINTS.len()
}

pub(super) fn launch_endpoint(index: usize) -> Option<&'static CStr> {
    LAUNCH_ENDPOINTS.iter().nth(index)
}

/// `text` is null or a C string that lives as long as the process.
pub(super) unsafe fn append_launch_endpoint(text: *const c_char) -> bool {
    if text.is_null() {
        return false;
    }
    // SAFETY: the caller's promise.
    let text: &'static CStr = unsafe { CStr::from_ptr(text) };
    if text.to_bytes().is_empty() {
        return false;
    }
    LAUNCH_ENDPOINTS.push(text)
}

/// The launcher supplies writable, NUL-terminated argv storage. On success
/// each separator is replaced with a NUL and the table borrows that storage.
pub(super) unsafe fn witchy_bind_endpoints(text: *mut c_char) -> bool {
    let mut starts = [0usize; ENDPOINT_CAPACITY];
    let mut count = 0;
    let mut offset = 0;
    // SAFETY: the launcher's promise. Nothing is written while `bytes` is in use.
    let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
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
    LAUNCH_ENDPOINTS.clear();
    // Every separator is replaced before any part is read as a C string.
    for start in starts.into_iter().take(count) {
        if start != 0 {
            // SAFETY: `start - 1` is the place of a separator found above.
            unsafe { text.add(start - 1).write(0) };
        }
    }
    for start in starts.into_iter().take(count) {
        // SAFETY: `start` is where a part begins, each part now ends in a NUL, and the
        // launcher's storage lives as long as the process.
        LAUNCH_ENDPOINTS.push(unsafe { CStr::from_ptr(text.add(start)) });
    }
    true
}

/// Endpoint and directory arguments are live launcher strings, with null
/// representing an absent grant. The launcher invokes this once before entry.
pub(super) unsafe fn launch_broker(
    listener: *const c_char,
    read_dir: *const c_char,
    write_dir: *const c_char,
    root_fd: c_int,
    write_root_fd: c_int,
    exec_fd: c_int,
    programs: *const *const c_char,
    program_count: u32,
    output: *const c_char,
    identity: *const super::file_identity::Identity,
    entries: u64,
    data_only: bool,
    stderr: *mut core::ffi::c_void,
) -> c_int {
    unsafe extern "C" {
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> c_int;
    }
    // SAFETY: the launcher passes null or a C string that lives as long as the process.
    let listener: Option<&'static CStr> =
        (!listener.is_null()).then(|| unsafe { CStr::from_ptr(listener) });
    let mut endpoints = [c""; ENDPOINT_CAPACITY];
    let mut endpoint_count = 0;
    for (slot, name) in endpoints.iter_mut().zip(LAUNCH_ENDPOINTS.iter()) {
        *slot = name;
        endpoint_count += 1;
    }
    let mut port = 0;
    let mut relay = 0;
    // SAFETY: the launcher's promise about each pointer is the one `witchy_net_start` asks for.
    let channel = unsafe {
        witchy_net_start(
            &endpoints[..endpoint_count],
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
            &mut relay,
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
    LAUNCH_RELAY.set(relay);
    LAUNCH_CHANNEL.set(channel);
    if sys::atexit(stop_launched_broker) != 0 {
        stop_launched_broker();
        return -1;
    }
    super::file_reader::bind_channel(channel);
    super::connection_queue::configure_network(channel, endpoint_count, listener, port);
    channel
}
