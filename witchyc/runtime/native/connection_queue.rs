//! FIFO for accepted sockets waiting for the active worker.

use super::global::Global;
use super::sys;
use core::ffi::{CStr, c_int};
use core::sync::atomic::{AtomicI32, Ordering};

const SOCKET_SLOTS: usize = 16;
const SOCKET_CHUNK: usize = 16384;
const ENTRY_SOCKET: u64 = 131072;

static SERVING_WORKER: AtomicI32 = AtomicI32::new(0);
static SERVING_BROKER: AtomicI32 = AtomicI32::new(0);
const STOP_SIGNALS: [c_int; 3] = [15, 1, 2];

fn uptime_ns() -> u64 {
    super::deadline::monotonic_ms()
        .unwrap_or(u64::MAX)
        .saturating_mul(1_000_000)
}

fn root_identity(fd: c_int) -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        super::seatbelt::stat(fd).map(|v| (v.device as u64, v.inode))
    }
    #[cfg(target_os = "linux")]
    {
        super::linux_stat::stat(fd).map(|v| (v.device, v.inode))
    }
}

/// The signals that stop the supervisor: 15, 1 and 2.
pub(super) fn stop_signal_set() -> Option<super::deadline::SignalSet> {
    unsafe extern "C" {
        fn sigemptyset(set: *mut super::deadline::SignalSet) -> c_int;
        fn sigaddset(set: *mut super::deadline::SignalSet, signal: c_int) -> c_int;
    }
    let mut set = super::deadline::empty_signal_set();
    let valid = unsafe {
        sigemptyset(&mut set) == 0
            && STOP_SIGNALS.into_iter().all(|signal| sigaddset(&mut set, signal) == 0)
    };
    valid.then_some(set)
}

/// Blocks (`how` 1) or unblocks (`how` 2) the signals of `signals`.
fn mask_signals(how: c_int, signals: &super::deadline::SignalSet) -> bool {
    unsafe extern "C" {
        fn sigprocmask(
            how: c_int,
            set: *const super::deadline::SignalSet,
            old: *mut super::deadline::SignalSet,
        ) -> c_int;
    }
    #[cfg(target_os = "linux")]
    let how = if how == 1 { 0 } else { 1 };
    // SAFETY: the call reads `signals`, and a null old set asks for none to be written.
    unsafe { sigprocmask(how, signals, core::ptr::null_mut()) == 0 }
}

pub(super) fn unblock_signals(signals: &super::deadline::SignalSet) -> bool {
    mask_signals(2, signals)
}

fn stop_signals(how: c_int) -> bool {
    let Some(signals) = stop_signal_set() else { return false };
    mask_signals(how, &signals)
}

extern "C" fn stop_serving(signal: c_int) {
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanoseconds: i64,
    }
    unsafe extern "C" {
        safe fn kill(pid: c_int, signal: c_int) -> c_int;
        fn nanosleep(duration: *const Timespec, remaining: *mut Timespec) -> c_int;
        safe fn _exit(code: c_int) -> !;
    }
    let worker = SERVING_WORKER.load(Ordering::Relaxed);
    let broker = SERVING_BROKER.load(Ordering::Relaxed);
    if worker > 0 {
        kill(worker, 9);
    }
    if broker > 0 {
        kill(broker, 15);
        let mut status = 0;
        let delay = Timespec {
            seconds: 0,
            nanoseconds: 5_000_000,
        };
        let mut done = 0;
        for _ in 0..200 {
            done = sys::waitpid(broker, Some(&mut status), 1);
            if done > 0 || (done < 0 && super::errno::get() != 4) {
                break;
            }
            // SAFETY: the call reads `delay`, and a null remainder asks for none to be written.
            unsafe {
                nanosleep(&delay, core::ptr::null_mut());
            }
        }
        // A failed wait does not establish that the PID is still our child.
        if done == 0 {
            kill(broker, 9);
        }
    }
    _exit(128 + signal);
}

fn install_stop_handler() -> bool {
    unsafe extern "C" {
        fn sigaction(
            signal: c_int,
            action: *const super::deadline::SignalAction,
            old: *mut super::deadline::SignalAction,
        ) -> c_int;
    }
    let Some(mask) = stop_signal_set() else { return false };
    let mut action = super::deadline::SignalAction::new(stop_serving as *const () as usize);
    // Keep other stop signals out until cleanup has exited the supervisor.
    action.mask = mask;
    unsafe {
        STOP_SIGNALS.into_iter().all(|signal| sigaction(signal, &action, core::ptr::null_mut()) == 0)
    }
}

// The launcher and each forked worker access their own table on a single thread.
#[derive(Clone, Copy)]
struct NativeSocket {
    handle: u64,
    queued_length: usize,
    accepted: bool,
    grouped: bool,
    queued: [u8; SOCKET_CHUNK],
}

unsafe extern "C" {
    safe fn witchy_net_close(channel: c_int, slot: u32) -> c_int;
}

const SOCKET_IO_ERROR: c_int = 5;

impl NativeSocket {
    const EMPTY: Self = Self {
        handle: 0,
        queued_length: 0,
        accepted: false,
        grouped: false,
        queued: [0; SOCKET_CHUNK],
    };

    fn flush(&mut self, channel: c_int, slot: u32) -> c_int {
        if self.queued_length > self.queued.len() {
            return SOCKET_IO_ERROR;
        }
        super::broker_transport::witchy_net_flush_queue(
            channel,
            slot,
            &self.queued,
            &mut self.queued_length,
        )
    }
}

static mut SOCKETS: [NativeSocket; SOCKET_SLOTS] = [NativeSocket::EMPTY; SOCKET_SLOTS];

#[repr(C)]
struct Queue {
    slots: [c_int; 4096],
    head: usize,
    len: usize,
    capacity: usize,
}

impl Queue {
    fn room(&self, remaining: u64) -> bool {
        self.len < self.capacity && (self.len as u64) < remaining
    }

    fn push(&mut self, fd: c_int) -> bool {
        if !self.room(u64::MAX) || fd < 0 {
            return false;
        }
        let tail = self.head + self.len;
        let index = if tail >= self.capacity {
            tail - self.capacity
        } else {
            tail
        };
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        *slot = fd;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<c_int> {
        if self.len == 0 {
            return None;
        }
        let fd = *self.slots.get(self.head)?;
        let next = self.head + 1;
        self.head = if next >= self.capacity { 0 } else { next };
        self.len -= 1;
        Some(fd)
    }
}

/// Wait for the active worker, accepting pending sockets while queue space
/// and the remaining connection budget permit it.
fn witchy_wait_worker(
    worker: c_int,
    listener: c_int,
    queue: &mut Queue,
    remaining: u64,
    setup_failed: c_int,
) -> c_int {
    use super::broker_socket::{wait_process, witchy_net_accept_connection};
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
    }
    const EINTR: c_int = 4;
    const WNOHANG: c_int = 1;
    const TIMED_OUT: c_int = -3;
    #[cfg(target_os = "macos")]
    const WNOWAIT: c_int = 0x20;
    #[cfg(target_os = "linux")]
    const WNOWAIT: c_int = 0x01000000;
    if worker <= 0 {
        return -1;
    }
    let mut status = 0;
    loop {
        let mode = if queue.room(remaining) { WNOHANG } else { 0 };
        let (result, seen) = wait_process(worker, 4 | WNOWAIT | mode);
        let waited = if result == 0 && seen == worker {
            stop_signals(1);
            let mut reaped;
            loop {
                reaped = sys::waitpid(worker, Some(&mut status), 0);
                if reaped >= 0 || super::errno::get() != EINTR {
                    break;
                }
            }
            SERVING_WORKER.store(0, Ordering::Relaxed);
            stop_signals(2);
            reaped
        } else if result == 0 {
            0
        } else {
            -1
        };
        if waited < 0 && super::errno::get() == EINTR {
            continue;
        }
        if waited < 0 {
            return -1;
        }
        if waited != 0 {
            return match status & 127 {
                0 if (status >> 8) & 255 == setup_failed => 3,
                0 if (status >> 8) & 255 == super::deadline::SETUP_TIMED_OUT => 4,
                0 => 0,
                14 => 2, // SIGALRM
                _ => 1,
            };
        }
        let fd = witchy_net_accept_connection(listener, 10);
        if fd >= 0 {
            if !queue.push(fd) {
                close(fd);
            }
        } else if fd != TIMED_OUT {
            // Reap the active worker before the caller tears down its broker.
            while wait_process(worker, 4 | WNOWAIT).0 < 0 && super::errno::get() == EINTR {}
            stop_signals(1);
            while sys::waitpid(worker, Some(&mut status), 0) < 0 && super::errno::get() == EINTR {
            }
            SERVING_WORKER.store(0, Ordering::Relaxed);
            stop_signals(2);
            return -1;
        }
    }
}

/// The most a socket-entry worker may spend installing its sandbox before its connection's
/// deadline starts (macOS). Always clipped to what is left of the run's deadline.
#[cfg(target_os = "macos")]
const SETUP_LIMIT_MS: u64 = 10_000;

/// Forks the guest worker after its private broker is ready. The launcher
/// supplies the live stdout stream, and `exec_shared` is null or a mapping of
/// one `u64`.
unsafe fn start_worker(
    listener: c_int,
    channel: c_int,
    grants: u64,
    timeout_ms: u64,
    run_deadline: u64,
    setup_failed: c_int,
    stdout: *mut core::ffi::c_void,
    slot: usize,
    queue: &Queue,
    root: c_int,
    write_root: c_int,
    exec_shared: *mut u64,
) -> c_int {
    unsafe extern "C" {
        fn fork() -> c_int;
        safe fn close(fd: c_int) -> c_int;
        fn munmap(address: *mut core::ffi::c_void, length: usize) -> c_int;
        fn fflush(stream: *mut core::ffi::c_void) -> c_int;
        safe fn _exit(code: c_int) -> !;
        fn witchy_entry(grants: u64);
        safe fn witchy_allow_allocator();
        safe fn witchy_arm_deadline(milliseconds: u64) -> bool;
        safe fn witchy_run_deadline_left_ms(end: u64, now: u64) -> u64;
        fn witchy_install_sandbox_with_relay(
            output: bool,
            allocator: bool,
            input: bool,
            relay: c_int,
        ) -> bool;
    }
    let worker = unsafe { fork() };
    if worker != 0 {
        return worker;
    }
    let output = grants & 1 != 0;
    let allocator = grants & 2 != 0;
    close(listener);
    sys::default_signal(15);
    sys::default_signal(1);
    sys::default_signal(2);
    if !stop_signals(2) {
        _exit(setup_failed);
    }
    NETWORK.set(NetworkConfig {
        channel,
        ..NETWORK.get()
    });
    super::file_reader::bind_channel(channel);
    for i in 0..queue.len {
        let at = (queue.head + i) % queue.capacity;
        close(queue.slots[at]);
    }
    if root >= 0 {
        close(root);
    }
    if write_root >= 0 && write_root != root {
        close(write_root);
    }
    let mut programs = [-1; 16];
    let count = super::exec_programs::witchy_exec_descriptors(&mut programs);
    for fd in programs.into_iter().take(count.min(16)) {
        close(fd);
    }
    if !exec_shared.is_null() && unsafe { munmap(exec_shared.cast(), size_of::<u64>()) } != 0 {
        _exit(setup_failed);
    }
    unsafe {
        core::ptr::addr_of_mut!(SOCKETS)
            .cast::<NativeSocket>()
            .add(slot)
            .write(NativeSocket {
                handle: ENTRY_SOCKET,
                accepted: true,
                ..NativeSocket::EMPTY
            });
    }
    unsafe {
        let remaining = || {
            if run_deadline == 0 {
                u64::MAX
            } else {
                witchy_run_deadline_left_ms(run_deadline, uptime_ns()).max(1)
            }
        };
        #[cfg(target_os = "linux")]
        if grants & (32768 | 65536 | super::tls_grant::TLS_SIGNER) != 0 {
            unsafe extern "C" {
                safe fn witchy_sandbox_allow_signing();
            }
            witchy_sandbox_allow_signing();
        }
        // macOS: the connection's deadline measures the guest's time with the connection, so
        // it starts once the worker is sandboxed. Seatbelt's sandbox_init can take most of a
        // second on a loaded host (measured: 450-950 ms with three CLI suites running, against
        // 4-12 ms idle); counted against a 200 ms connection deadline it killed workers before
        // any guest code ran and reset their clients. Setup has its own bound, never past the
        // run's deadline; if it expires the worker exits SETUP_TIMED_OUT, not by SIGALRM, so the
        // supervisor reports it as a setup failure of that connection alone.
        #[cfg(target_os = "macos")]
        if !super::deadline::arm_setup_deadline(SETUP_LIMIT_MS.min(remaining()))
            || !witchy_install_sandbox_with_relay(output, allocator, false, channel)
            || !witchy_arm_deadline(timeout_ms.min(remaining()))
        {
            _exit(setup_failed);
        }
        // Linux: Landlock and seccomp install in microseconds, and the worker's seccomp filter
        // allows no timer call after them, so the connection deadline is armed first.
        #[cfg(target_os = "linux")]
        if !witchy_arm_deadline(timeout_ms.min(remaining()))
            || !witchy_install_sandbox_with_relay(output, allocator, false, channel)
        {
            _exit(setup_failed);
        }
        if allocator {
            witchy_allow_allocator();
        }
        witchy_entry(grants);
        _exit(if output && fflush(stdout) < 0 { 2 } else { 0 })
    }
}

/// The launcher owns the channel and relay fields and starts one worker at a time.
unsafe fn serve_connections(
    listener: c_int,
    grants: u64,
    limit: u64,
    worker_ms: u64,
    run_ms: u64,
    waiting: u32,
    slot: u32,
    channel: &mut c_int,
    relay: &mut c_int,
    stdout: *mut core::ffi::c_void,
    stderr: *mut core::ffi::c_void,
    root: c_int,
    write_root: c_int,
    combined: bool,
    data_only: bool,
    entries: u64,
    exec_shared: *mut u64,
) -> c_int {
    use super::broker_socket::{
        witchy_broker_server_roots, witchy_broker_stop_checked, witchy_net_accept_connection,
        witchy_net_connection_broker,
    };
    unsafe extern "C" {
        safe fn close(fd: c_int) -> c_int;
        fn fprintf(stream: *mut core::ffi::c_void, format: *const core::ffi::c_char, ...) -> c_int;
        fn fputs(text: *const core::ffi::c_char, stream: *mut core::ffi::c_void) -> c_int;
        safe fn witchy_budget_share() -> bool;
        safe fn witchy_run_deadline_ns(now: u64, ms: u64) -> u64;
        safe fn witchy_run_deadline_left_ms(end: u64, now: u64) -> u64;
        #[cfg(target_os = "linux")]
        safe fn witchy_exec_sweep_own() -> bool;
        fn mmap(
            address: *mut core::ffi::c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut core::ffi::c_void;
    }
    const SETUP_FAILED: c_int = 125;
    let say = |message: &CStr| {
        // SAFETY: `message` is a C string, and the launcher supplies the live stream.
        unsafe { fputs(message.as_ptr(), stderr) };
    };
    let fail = |message: &CStr| {
        say(message);
        2
    };
    if slot as usize >= SOCKET_SLOTS
        || waiting > 4096
        || !witchy_budget_share()
        || !super::io_budget::share_filesystem()
    {
        return fail(c"witchy: could not initialize connection budgets\n");
    }
    #[cfg(target_os = "macos")]
    const SHARED_ANON: c_int = 0x1001;
    #[cfg(target_os = "linux")]
    const SHARED_ANON: c_int = 0x21;
    let shared_entries = unsafe {
        mmap(
            core::ptr::null_mut(),
            size_of::<u64>(),
            3,
            SHARED_ANON,
            -1,
            0,
        )
    }
    .cast::<u64>();
    if shared_entries.addr() == usize::MAX {
        return fail(c"witchy: could not allocate the shared directory budget\n");
    }
    // SAFETY: a fresh mapping of one writable `u64`.
    unsafe { shared_entries.write(entries) };
    // SAFETY: `shared_entries` is the mapping made above, which this process never unmaps.
    unsafe { witchy_broker_server_roots(root, write_root, combined, data_only, shared_entries) };
    if !install_stop_handler() {
        return fail(c"witchy: could not install connection supervisor signal handlers\n");
    }
    let mut queue = Queue {
        slots: [0; 4096],
        head: 0,
        len: 0,
        capacity: waiting as usize,
    };
    let uptime = uptime_ns;
    let report = |flag: &'static core::ffi::CStr| unsafe {
        fprintf(
            stderr,
            c"witchy: limit reached: %s\n".as_ptr(),
            super::limit_labels::witchy_limit_label(flag).as_ptr(),
        );
    };
    let end = witchy_run_deadline_ns(if run_ms == 0 { 0 } else { uptime() }, run_ms);
    let remaining_time =
        || witchy_run_deadline_left_ms(end, if end == 0 { 0 } else { uptime() });
    let expired = || {
        report(c"--deadline-ms");
        2
    };
    // A setup that fails (rather than runs slow) usually fails the same way each time: after
    // this many in a row the service stops instead of resetting every client.
    const SETUP_FAILURES_IN_A_ROW: u32 = 3;
    let mut setup_failures = 0;
    let mut accepted = 0;
    while accepted < limit {
        let left = remaining_time();
        if left == 0 {
            return expired();
        }
        let connection = queue.pop().unwrap_or_else(|| {
            witchy_net_accept_connection(listener, left.min(u64::from(u32::MAX)) as u32)
        });
        if connection == -3 {
            continue;
        }
        if connection < 0 {
            return fail(c"witchy: could not accept a connection\n");
        }
        let timeout = worker_ms.min(remaining_time());
        if timeout == 0 && end != 0 {
            close(connection);
            return expired();
        }
        if !stop_signals(1) {
            close(connection);
            return fail(c"witchy: could not block supervisor stop signals\n");
        }
        let broker = witchy_net_connection_broker(connection, slot, relay);
        *channel = broker;
        if broker < 0 {
            stop_signals(2);
            return fail(c"witchy: could not start a connection broker\n");
        }
        SERVING_BROKER.store(*relay, Ordering::Relaxed);
        accepted += 1;
        let worker = unsafe {
            start_worker(
                listener,
                broker,
                grants,
                timeout,
                end,
                SETUP_FAILED,
                stdout,
                slot as usize,
                &queue,
                root,
                write_root,
                exec_shared,
            )
        };
        SERVING_WORKER.store(worker.max(0), Ordering::Relaxed);
        stop_signals(2);
        let result =
            witchy_wait_worker(worker, listener, &mut queue, limit - accepted, SETUP_FAILED);
        stop_signals(1);
        let cleaned =
            witchy_broker_stop_checked(channel, *relay, matches!(result, -1 | 1 | 2 | 3 | 4));
        #[cfg(target_os = "linux")]
        let cleaned = {
            let swept = grants & 262144 == 0 || witchy_exec_sweep_own();
            cleaned && swept
        };
        SERVING_BROKER.store(0, Ordering::Relaxed);
        stop_signals(2);
        if !cleaned {
            return fail(c"witchy: Exec: what a connection's program left could not be stopped: the service stops\n");
        }
        match result {
            -1 => return fail(c"witchy: could not supervise a connection worker\n"),
            2 => {
                if remaining_time() == 0 {
                    return expired();
                }
                report(c"--connection-deadline-ms");
            }
            // A worker that never reached guest code fails its own connection only: the
            // connection is closed and reported, and queued clients are still served.
            3 => {
                say(c"witchy: a connection's worker could not install its deadline or sandbox; that connection is closed\n");
                setup_failures += 1;
                if setup_failures == SETUP_FAILURES_IN_A_ROW {
                    return fail(c"witchy: workers failed to install their sandbox three times in a row: the service stops\n");
                }
                continue;
            }
            4 => {
                if remaining_time() == 0 {
                    return expired();
                }
                say(c"witchy: a connection's worker did not install its sandbox in time; that connection is closed\n");
            }
            _ => {}
        }
        setup_failures = 0;
    }
    if remaining_time() == 0 { expired() } else { 0 }
}

/// # Safety
/// The caller uses no other reference into the socket table after this call
/// starts: the guest dispatcher owns the slot for the call.
unsafe fn socket_close(channel: c_int, slot: u32) -> c_int {
    // SAFETY: calls are serialized on the guest thread, and the caller holds
    // no other reference to the table.
    let Some(socket) = (unsafe { (&mut *core::ptr::addr_of_mut!(SOCKETS)).get_mut(slot as usize) })
    else {
        return SOCKET_IO_ERROR;
    };
    let flushed = socket.flush(channel, slot);
    let closed = witchy_net_close(channel, slot);
    socket.handle = 0;
    socket.accepted = false;
    socket.grouped = false;
    if flushed != 0 || closed != 0 {
        SOCKET_IO_ERROR
    } else {
        0
    }
}

// The guest dispatcher validates the live socket and supplies a guest
// pointer for slice operations. Only the bounded transfer reaches the ABI.
unsafe fn socket_transfer(channel: c_int, slot: u32, op: u64, operand: u64, count: u64) -> u64 {
    unsafe extern "C" {
        safe fn witchy_network_remaining() -> u64;
        safe fn witchy_network_charge(bytes: usize);
    }
    use super::broker_transport::{
        witchy_net_receive, witchy_net_send, witchy_net_try_receive, witchy_net_try_send,
    };
    const TOO_LARGE: u64 = 3;
    const IO: u64 = SOCKET_IO_ERROR as u64;
    const WOULD_BLOCK: u64 = 7;

    let Some(socket) = (unsafe { (&mut *core::ptr::addr_of_mut!(SOCKETS)).get_mut(slot as usize) })
    else {
        socket_trap()
    };
    let remaining = || witchy_network_remaining();
    let charge = |size| witchy_network_charge(size);
    let transfer_error = |status| (if status == -4 { WOULD_BLOCK } else { IO }) << 32;
    if op == 4 {
        let Ok(byte) = u8::try_from(operand) else {
            socket_trap()
        };
        if remaining() == 0 {
            return TOO_LARGE;
        }
        if socket.queued_length == SOCKET_CHUNK && socket.flush(channel, slot) != 0 {
            return IO;
        }
        let Some(out) = socket.queued.get_mut(socket.queued_length) else {
            return IO;
        };
        *out = byte;
        socket.queued_length += 1;
        charge(1);
        return 0;
    }
    if op == 5 {
        if remaining() == 0 || socket.flush(channel, slot) != 0 {
            return 257;
        }
        let mut byte = 0;
        let size = witchy_net_receive(channel, slot, core::slice::from_mut(&mut byte));
        return match size {
            ..0 => 257,
            0 => 256,
            _ => {
                charge(size as usize);
                u64::from(byte)
            }
        };
    }

    let nonblocking = matches!(op, 12 | 13);
    if nonblocking {
        if socket.queued_length != 0 {
            let Some(queued) = socket.queued.get(..socket.queued_length) else {
                return IO << 32;
            };
            let sent = witchy_net_try_send(channel, slot, queued);
            if sent < 0 {
                return transfer_error(sent);
            }
            let sent = sent as usize;
            if sent > socket.queued_length || socket.queued_length > SOCKET_CHUNK {
                return IO << 32;
            }
            socket.queued.copy_within(sent..socket.queued_length, 0);
            socket.queued_length -= sent;
            if socket.queued_length != 0 {
                return WOULD_BLOCK << 32;
            }
        }
    } else if socket.flush(channel, slot) != 0 {
        return IO << 32;
    }

    let budget = remaining();
    // Blocking sends can produce a partial write followed by budget exhaustion.
    if op != 7 && budget == 0 {
        return TOO_LARGE << 32;
    }
    let len = count.min(budget).min(SOCKET_CHUNK as u64) as usize;
    let bytes = operand as usize as *mut u8;
    let missing = bytes.is_null() && len != 0;
    // An empty span needs no guest memory, and its address may be null.
    let bytes = if len == 0 {
        core::ptr::NonNull::dangling().as_ptr()
    } else {
        bytes
    };
    // SAFETY: the guest dispatcher supplies a span of `count` bytes, readable
    // for a send and writable for a receive, and `len` is at most `count`.
    // No span is formed from a null address.
    let size = unsafe {
        match op {
            7 | 8 | 12 | 13 if missing => -1,
            7 => witchy_net_send(channel, slot, core::slice::from_raw_parts(bytes, len)),
            8 => witchy_net_receive(channel, slot, core::slice::from_raw_parts_mut(bytes, len)),
            12 => witchy_net_try_send(channel, slot, core::slice::from_raw_parts(bytes, len)),
            13 => {
                witchy_net_try_receive(channel, slot, core::slice::from_raw_parts_mut(bytes, len))
            }
            _ => socket_trap(),
        }
    };
    if size < 0 {
        return if nonblocking {
            transfer_error(size)
        } else {
            IO << 32
        };
    }
    if size > 0 || op == 7 {
        charge(size as usize);
    }
    let status = if op == 7 && count > budget && remaining() == 0 {
        TOO_LARGE << 32
    } else {
        0
    };
    size as u64 | status
}

#[derive(Clone, Copy)]
struct NetworkGroup {
    handle: u64,
    members: [u64; SOCKET_SLOTS],
    listener: bool,
}

const GROUP_SLOTS: usize = 64;
const GROUP_BASE: u64 = 1 << 47;
const TIMER_BIT: u64 = 1 << 48;
const LISTENER: u64 = 512;

static mut GROUPS: [NetworkGroup; GROUP_SLOTS] = [NetworkGroup {
    handle: 0,
    members: [0; SOCKET_SLOTS],
    listener: false,
}; GROUP_SLOTS];
static NEXT_GROUP: Global<u64> = Global::new(GROUP_BASE);

fn socket_trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn live_group(groups: &mut [NetworkGroup; GROUP_SLOTS], handle: u64) -> Option<&mut NetworkGroup> {
    let group = &mut groups[handle as usize % GROUP_SLOTS];
    (handle != 0 && group.handle == handle).then_some(group)
}

fn socket_group(op: u64, handle: u64, operand: u64) -> u64 {
    // Calls are serialized on the guest thread.
    let groups = unsafe { &mut *core::ptr::addr_of_mut!(GROUPS) };
    if op == 15 {
        for _ in 0..GROUP_SLOTS {
            let token = NEXT_GROUP.get();
            if token >= TIMER_BIT {
                break;
            }
            NEXT_GROUP.set(token + 1);
            let group = &mut groups[token as usize % GROUP_SLOTS];
            if group.handle == 0 {
                *group = NetworkGroup {
                    handle: token,
                    members: [0; SOCKET_SLOTS],
                    listener: handle == LISTENER,
                };
                return token;
            }
        }
        socket_trap();
    }
    let group = live_group(groups, handle).unwrap_or_else(|| socket_trap());
    let sockets = unsafe { &mut *core::ptr::addr_of_mut!(SOCKETS) };
    if op == 17 {
        let member = group
            .members
            .get_mut(operand as usize)
            .unwrap_or_else(|| socket_trap());
        let token = *member;
        let socket = &mut sockets[token as usize % SOCKET_SLOTS];
        if token == 0 || socket.handle != token {
            socket_trap();
        }
        *member = 0;
        socket.grouped = false;
        return token;
    }
    let socket = &mut sockets[operand as usize % SOCKET_SLOTS];
    if op != 16 || operand == 0 || socket.handle != operand || socket.grouped {
        socket_trap();
    }
    let Some(index) = group.members.iter().position(|member| *member == 0) else {
        socket_trap();
    };
    group.members[index] = operand;
    socket.grouped = true;
    index as u64
}

fn socket_close_group(channel: c_int, handle: u64) -> bool {
    let groups = unsafe { &mut *core::ptr::addr_of_mut!(GROUPS) };
    let Some(group) = live_group(groups, handle) else {
        return false;
    };
    for token in group.members {
        if token != 0 {
            let slot = token as usize % SOCKET_SLOTS;
            if unsafe { (*core::ptr::addr_of!(SOCKETS))[slot].handle } != token {
                socket_trap();
            }
            // SAFETY: no reference into the socket table is held here.
            unsafe { socket_close(channel, slot as u32) };
        }
    }
    group.handle = 0;
    true
}

enum PollTarget {
    Socket(usize),
    Timer(u64),
    Closed,
}

// The dispatcher supplies the group (if any) and the writable guest entries.
unsafe fn socket_poll(
    channel: c_int,
    listener: bool,
    entries: *mut u64,
    count: u64,
    group: u64,
) -> u64 {
    unsafe extern "C" {
        fn witchy_timer_due(handle: u64, due: *mut u64) -> bool;
        safe fn witchy_clock_now_ms(grant: u64) -> u64;
    }
    const LISTENER_SLOT: u64 = 16_777_215;
    const CLOCK: u64 = 16;

    if count > 4097 {
        socket_trap();
    }
    let group = if group == 0 {
        None
    } else {
        let groups = unsafe { &mut *core::ptr::addr_of_mut!(GROUPS) };
        Some(&*live_group(groups, group).unwrap_or_else(|| socket_trap()))
    };
    if count == 0 {
        return 0;
    }
    let entries = unsafe { core::slice::from_raw_parts_mut(entries, count as usize) };
    let sockets = unsafe { &*core::ptr::addr_of!(SOCKETS) };
    let target = |slot: u64| {
        let handle = if let Some(group) = group {
            if slot == LISTENER_SLOT {
                if !group.listener {
                    socket_trap();
                }
                LISTENER
            } else {
                group.members.get(slot as usize).copied().unwrap_or(0)
            }
        } else {
            slot
        };
        if handle == LISTENER && listener {
            return PollTarget::Socket(0);
        }
        let index = handle as usize % SOCKET_SLOTS;
        let socket = &sockets[index];
        if handle != 0 && socket.handle == handle && (group.is_some() || !socket.grouped) {
            return PollTarget::Socket(index + 1);
        }
        let mut due = 0;
        if group.is_none() && unsafe { witchy_timer_due(handle, &mut due) } {
            return PollTarget::Timer(due);
        }
        PollTarget::Closed
    };

    let mut request = [0; SOCKET_SLOTS + 1];
    let mut timeout = 5000;
    let mut has_socket = false;
    let mut soonest: Option<u64> = None;
    for &entry in entries.iter() {
        if entry == 0 && group.is_none() {
            continue;
        }
        match target(entry >> 8) {
            PollTarget::Socket(index) => {
                request[index] |= (entry as u8 & 3) | 4;
                has_socket = true;
            }
            PollTarget::Timer(due) => soonest = Some(soonest.map_or(due, |time| time.min(due))),
            PollTarget::Closed => timeout = 0,
        }
    }
    if let Some(due) = soonest.filter(|_| timeout != 0) {
        timeout = due
            .saturating_sub(witchy_clock_now_ms(CLOCK))
            .min(timeout.into()) as u32;
    }
    if (has_socket || soonest.is_some())
        && super::broker_transport::witchy_net_poll(channel, &mut request, timeout) < 0
    {
        socket_trap();
    }
    let now = soonest.map_or(0, |_| witchy_clock_now_ms(CLOCK));
    let mut ready = 0;
    for entry in entries {
        if *entry == 0 && group.is_none() {
            continue;
        }
        let flags = match target(*entry >> 8) {
            PollTarget::Socket(index) => u64::from(request[index]) & (16 | ((*entry & 3) << 2)),
            PollTarget::Timer(due) => {
                if now >= due {
                    4
                } else {
                    0
                }
            }
            PollTarget::Closed => 16,
        };
        *entry = (*entry & !28) | flags;
        ready += u64::from(flags != 0);
    }
    ready
}

static NEXT_SOCKET: Global<u64> = Global::new(ENTRY_SOCKET);

// The dispatcher validates the grant and resolves the endpoint before opening.
fn socket_open(channel: c_int, op: u64, endpoint: u32) -> u64 {
    unsafe extern "C" {
        safe fn witchy_net_connect(channel: c_int, slot: u32, endpoint: u32) -> c_int;
        safe fn witchy_net_accept(channel: c_int, slot: u32, wait: bool) -> c_int;
    }
    const FAILED_HANDLE: u64 = 8;
    const TOO_LARGE: u64 = 3;
    const REFUSED: u64 = 4;
    const TIMED_OUT: u64 = 6;
    const WOULD_BLOCK: u64 = 7;
    const BROKER_TIMED_OUT: c_int = -3;

    let sockets = unsafe { &mut *core::ptr::addr_of_mut!(SOCKETS) };
    for _ in 0..SOCKET_SLOTS {
        let token = NEXT_SOCKET.get();
        if token >= GROUP_BASE {
            break;
        }
        NEXT_SOCKET.set(token + 1);
        let slot = token as usize % SOCKET_SLOTS;
        let socket = &mut sockets[slot];
        if socket.handle != 0 {
            continue;
        }
        let result = {
            match op {
                2 => witchy_net_connect(channel, slot as u32, endpoint),
                9 | 11 => witchy_net_accept(channel, slot as u32, op == 9),
                _ => socket_trap(),
            }
        };
        let error = match result {
            0 => {
                socket.handle = token;
                socket.accepted = op != 2;
                return token;
            }
            BROKER_TIMED_OUT if op == 9 => TIMED_OUT,
            BROKER_TIMED_OUT => WOULD_BLOCK,
            _ if op == 2 => REFUSED,
            _ => SOCKET_IO_ERROR as u64,
        };
        return FAILED_HANDLE + error;
    }
    FAILED_HANDLE + TOO_LARGE
}

const NET: u64 = 4;
const FIRST_NARROWED: u64 = 65_536;
const ENDPOINT_LIMIT: usize = 16;
const FAILED_HANDLE: u64 = 8;

#[derive(Clone, Copy)]
struct PendingAddress {
    bytes: [u8; 256],
    len: usize,
}

static mut ADDRESSES: [PendingAddress; ENDPOINT_LIMIT + 1] = [PendingAddress {
    bytes: [0; 256],
    len: 0,
}; ENDPOINT_LIMIT + 1];

fn listener_address(host_port: &core::ffi::CStr, port: u16, output: &mut [u8]) -> usize {
    let bytes = host_port.to_bytes();
    let Some(separator) = bytes.iter().rposition(|&byte| byte == b':') else {
        return 0;
    };
    let Some(host) = bytes.get(..separator) else {
        return 0;
    };
    let bracket = !host.starts_with(b"[") && host.iter().any(|&byte| byte == b':');
    let mut digits = [0; 5];
    let mut remaining = port;
    let mut digit_count = 0;
    for digit in digits.iter_mut().rev() {
        *digit = b'0' + (remaining % 10) as u8;
        digit_count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    let length = host.len() + 1 + digit_count + if bracket { 2 } else { 0 };
    if output.len() < length {
        return 0;
    }
    let prefix = bracket.then_some(b'[');
    let suffix = bracket.then_some(b']');
    let address = prefix
        .into_iter()
        .chain(host.iter().copied())
        .chain(suffix)
        .chain(core::iter::once(b':'))
        .chain(digits.into_iter().skip(5 - digit_count));
    for (out, byte) in output.iter_mut().zip(address) {
        *out = byte;
    }
    length
}

#[derive(Clone, Copy)]
struct NetworkConfig {
    channel: c_int,
    endpoint_count: usize,
    listener: Option<&'static CStr>,
    port: u16,
}

static NETWORK: Global<NetworkConfig> = Global::new(NetworkConfig {
    channel: -1,
    endpoint_count: 0,
    listener: None,
    port: 0,
});

// The launcher binds once before entering the guest or forking workers.
// The endpoints are the first `endpoint_count` of the launcher's table.
pub(super) fn configure_network(
    channel: c_int,
    endpoint_count: usize,
    listener: Option<&'static CStr>,
    port: u16,
) {
    NETWORK.set(NetworkConfig {
        channel,
        endpoint_count,
        listener,
        port,
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_native_net(op: u64, handle: u64, operand: u64, count: u64) -> u64 {
    use super::broker_endpoint::witchy_net_same_endpoint;
    use super::broker_socket::launch_endpoint;
    let NetworkConfig {
        channel,
        endpoint_count,
        listener,
        port,
    } = NETWORK.get();
    if channel < 0 || endpoint_count > ENDPOINT_LIMIT {
        socket_trap();
    }
    let narrowed = handle
        .checked_sub(FIRST_NARROWED)
        .filter(|index| *index < endpoint_count as u64);
    let net = (handle == NET && endpoint_count != 0) || narrowed.is_some();
    let listening = listener.is_some();
    let listener_grant = handle == LISTENER && listening;
    let failed = handle > FAILED_HANDLE && handle <= FAILED_HANDLE + 7;
    match op {
        0..=2 => {
            if !net {
                socket_trap();
            }
            let index = narrowed.map_or(0, |index| index as usize + 1);
            let address = unsafe { &mut (*core::ptr::addr_of_mut!(ADDRESSES))[index] };
            if op == 0 {
                let byte = u8::try_from(operand).unwrap_or_else(|_| socket_trap());
                if let Some(out) = address.bytes.get_mut(address.len) {
                    *out = byte;
                    address.len += 1;
                }
                return 0;
            }
            let len = core::mem::take(&mut address.len);
            if len >= address.bytes.len() || address.bytes.iter().take(len).any(|&byte| byte == 0) {
                return FAILED_HANDLE + 1;
            }
            address.bytes[len] = 0;
            // The checks above leave one NUL, at `len`.
            let Ok(candidate) = CStr::from_bytes_with_nul(&address.bytes[..=len]) else {
                return FAILED_HANDLE + 1;
            };
            let span = narrowed.map_or(0..endpoint_count, |index| {
                let index = index as usize;
                index..index + 1
            });
            let endpoint = span.into_iter().find(|&index| {
                launch_endpoint(index)
                    .is_some_and(|configured| witchy_net_same_endpoint(candidate, configured))
            });
            let Some(endpoint) = endpoint else {
                return FAILED_HANDLE + 1;
            };
            return if op == 1 {
                FIRST_NARROWED + endpoint as u64
            } else {
                socket_open(channel, op, endpoint as u32)
            };
        }
        9 | 11 if listener_grant => return socket_open(channel, op, 0),
        9 | 11 => socket_trap(),
        14 | 18 => {
            if (op == 14 && !(net || listener_grant)) || (op == 18 && handle == 0) {
                socket_trap();
            }
            let group = if op == 18 { handle } else { 0 };
            return unsafe { socket_poll(channel, listening, operand as *mut u64, count, group) };
        }
        15 if !(net || listener_grant) => socket_trap(),
        15..=17 => return socket_group(op, handle, operand),
        6 if socket_close_group(channel, handle) || net || failed => return 0,
        _ => {}
    }
    let slot = handle as usize % SOCKET_SLOTS;
    let socket = unsafe { &(*core::ptr::addr_of!(SOCKETS))[slot] };
    let live = handle != 0 && socket.handle == handle;
    if op == 3 {
        if net || live {
            return 0;
        }
        if failed {
            return handle - FAILED_HANDLE;
        }
    }
    if !live || socket.grouped {
        socket_trap();
    }
    match op {
        4 | 5 | 7 | 8 | 12 | 13 => unsafe {
            socket_transfer(channel, slot as u32, op, operand, count)
        },
        // SAFETY: `socket` is not used again on this arm.
        6 => (unsafe { socket_close(channel, slot as u32) }) as u64,
        10 if socket.accepted && count != 0 => {
            let Some(endpoint) = listener else { return 0 };
            // The guest ABI supplies a writable span. Limit it to the maximum
            // bytes this operation could write before constructing a slice.
            let capacity = count.min(endpoint.to_bytes().len() as u64 + 8) as usize;
            let output = unsafe { core::slice::from_raw_parts_mut(operand as *mut u8, capacity) };
            listener_address(endpoint, port, output) as u64
        }
        10 => 0,
        _ => socket_trap(),
    }
}

/// The launcher passes the validated endpoint and its live standard streams.
/// Each other pointer is null, for a grant that is absent, or launcher-owned
/// storage that lives as long as the process.
pub(super) unsafe fn launch_connections(
    endpoint: *const core::ffi::c_char,
    grants: u64,
    limit: u64,
    worker_ms: u64,
    run_ms: u64,
    waiting: u64,
    read_dir: *const core::ffi::c_char,
    write_dir: *const core::ffi::c_char,
    root_fd: c_int,
    write_fd: c_int,
    identity: *const super::file_identity::Identity,
    entries: u64,
    connection_entries: u64,
    data_only: bool,
    exec_fd: c_int,
    programs: *const *const core::ffi::c_char,
    program_count: u32,
    exec_budget: u64,
    input_budget: u64,
    input_budget_set: bool,
    stdout: *mut core::ffi::c_void,
    stderr: *mut core::ffi::c_void,
) -> c_int {
    use core::ffi::{c_char, c_void};

    #[repr(C)]
    struct DescriptorLimit {
        current: u64,
        maximum: u64,
    }

    use super::broker_socket::{
        launch_endpoint_count, witchy_broker_server_connection_entries,
        witchy_broker_server_endpoints, witchy_net_listen,
    };
    unsafe extern "C" {
        fn getrlimit(resource: c_int, limit: *mut DescriptorLimit) -> c_int;
        safe fn close(fd: c_int) -> c_int;
        fn fprintf(stream: *mut c_void, format: *const c_char, ...) -> c_int;
        fn fputs(text: *const c_char, stream: *mut c_void) -> c_int;
        fn mmap(
            address: *mut c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
    }

    let fail = |message: &CStr| {
        // SAFETY: `message` is a C string, and the launcher supplies the live stream.
        unsafe { fputs(message.as_ptr(), stderr) };
        2
    };
    if grants & 8 != 0
        && (!input_budget_set
            || input_budget > 16 * 1024 * 1024
            || !super::input_stream::snapshot_input(input_budget))
    {
        return fail(c"native Socket entry reads its input once, before the first connection: it requires --input-bytes of at most 16 MiB covering all of it\n");
    }
    let keep = [root_fd, write_fd, exec_fd];
    #[cfg(target_os = "linux")]
    use super::linux_sandbox::witchy_close_inherited_keeping;
    #[cfg(target_os = "macos")]
    use super::seatbelt_fds::witchy_close_inherited_keeping;
    if !witchy_close_inherited_keeping(grants & 1 != 0, false, &keep) {
        return fail(c"native socket server could not close inherited descriptors\n");
    }
    let mut port = 0;
    // SAFETY: the launcher passes the endpoint as a C string, and it lives as
    // long as the process.
    let endpoint_text: &'static CStr = unsafe { CStr::from_ptr(endpoint) };
    let listener = witchy_net_listen(endpoint_text, &mut port);
    if listener < 0 {
        return fail(c"native socket server could not bind the granted listener\n");
    }

    #[cfg(target_os = "macos")]
    const RLIMIT_NOFILE: c_int = 8;
    #[cfg(target_os = "linux")]
    const RLIMIT_NOFILE: c_int = 7;
    // Reserve standard streams, listener, next accepted socket, and broker FDs.
    const RESERVED_DESCRIPTORS: u64 = 8;
    let mut descriptors = DescriptorLimit {
        current: 0,
        maximum: 0,
    };
    let mut root = root_fd;
    let mut write_root = if write_dir == read_dir && !read_dir.is_null() {
        -1
    } else {
        write_fd
    };
    #[cfg(target_os = "macos")]
    const ROOT_FLAGS: c_int = 0x2110_0000;
    #[cfg(target_os = "linux")]
    const ROOT_FLAGS: c_int = 0o40000 | 0o2000000 | 0o100000;
    // SAFETY: called only with a directory argument that is not null, which
    // the launcher passes as a C string.
    let open_root = |path: *const c_char| {
        #[cfg(target_os = "macos")]
        {
            sys::open(unsafe { CStr::from_ptr(path) }, ROOT_FLAGS)
        }
        #[cfg(target_os = "linux")]
        {
            super::linux_stat::openat_no_symlinks(-100, unsafe { CStr::from_ptr(path) }, ROOT_FLAGS)
        }
    };
    if !read_dir.is_null() && root < 0 {
        root = open_root(read_dir);
    }
    if !write_dir.is_null() && write_dir != read_dir && write_root < 0 {
        write_root = open_root(write_dir);
    }
    if !write_dir.is_null() && write_dir == read_dir && root >= 0 {
        write_root = sys::duplicate_close_on_exec(root, 3);
    }
    for fd in [&mut root, &mut write_root] {
        if (0..=2).contains(fd) {
            let moved = sys::duplicate_close_on_exec(*fd, 3);
            close(*fd);
            *fd = moved;
        }
    }
    if (!read_dir.is_null() && root < 0)
        || (!write_dir.is_null() && write_root < 0)
        || (!identity.is_null()
            && !root_identity(if root >= 0 { root } else { write_root }).is_some_and(
                |(device, inode)| unsafe {
                    device == (*identity).device && inode == (*identity).inode
                },
            ))
    {
        close(listener);
        return 2;
    }
    if (grants & 4 != 0 && !witchy_broker_server_endpoints())
        || (grants & 262144 != 0
            && (exec_fd < 0
                // SAFETY: the launcher passes exec_guest's table of `program_count` live names.
                || !unsafe {
                    super::exec_programs::setup_from_table(
                        exec_fd,
                        programs,
                        program_count,
                        exec_budget,
                    )
                }))
    {
        close(listener);
        return 2;
    }
    witchy_broker_server_connection_entries(connection_entries);
    let exec_shared = if grants & 262144 != 0 {
        #[cfg(target_os = "macos")]
        const SHARED_ANON: c_int = 0x1001;
        #[cfg(target_os = "linux")]
        const SHARED_ANON: c_int = 0x21;
        let address = unsafe {
            mmap(
                core::ptr::null_mut(),
                size_of::<u64>(),
                3,
                SHARED_ANON,
                -1,
                0,
            )
        }
        .cast::<u64>();
        if address.addr() == usize::MAX {
            close(listener);
            return 2;
        }
        // SAFETY: a fresh shared, writable mapping that the supervisor never unmaps.
        unsafe {
            super::exec_programs::witchy_exec_share_budget(address);
        }
        address
    } else {
        core::ptr::null_mut()
    };

    #[cfg(target_os = "linux")]
    if grants & 262144 != 0 {
        unsafe extern "C" {
            safe fn witchy_exec_supervise() -> bool;
        }
        if !witchy_exec_supervise() {
            close(listener);
            return 2;
        }
    }

    let status = if unsafe { getrlimit(RLIMIT_NOFILE, &mut descriptors) } != 0 {
        2
    } else {
        let room = descriptors.current.saturating_sub(RESERVED_DESCRIPTORS);
        let waiting = waiting.min(room);
        if endpoint_text.to_bytes().rsplit(|&byte| byte == b':').next()
            .is_some_and(|number| number.iter().all(|&digit| digit == b'0'))
        {
            let mut address = [0; 256];
            let len = listener_address(endpoint_text, port, &mut address[..255]);
            if len != 0 {
                unsafe { fprintf(stderr, c"witchy: listening on %s\n".as_ptr(), address.as_ptr()) };
            }
        }
        configure_network(-1, launch_endpoint_count(), Some(endpoint_text), port);
        let mut channel = -1;
        let mut relay = 0;
        unsafe {
            serve_connections(
                listener,
                grants,
                limit,
                worker_ms,
                run_ms,
                waiting as u32,
                (ENTRY_SOCKET % SOCKET_SLOTS as u64) as u32,
                &mut channel,
                &mut relay,
                stdout,
                stderr,
                root,
                write_root,
                !read_dir.is_null() && read_dir == write_dir,
                data_only,
                entries,
                exec_shared,
            )
        }
    };
    close(listener);
    status
}

fn connection_launch_allowed(
    required: u64,
    supplied: u64,
    per_connection: bool,
    listener: bool,
    connection_options: bool,
) -> bool {
    const WORKER_GRANTS: u64 = ENTRY_SOCKET
        | 1
        | 2
        | 4
        | 8
        | 16
        | 32
        | 64
        | 128
        | 256
        | 1024
        | 4096
        | 8192
        | 16384
        | 262144
        | super::tls_grant::TLS_SIGNER;
    if per_connection != (required & ENTRY_SOCKET != 0) {
        return false;
    }
    if per_connection {
        listener && supplied & !WORKER_GRANTS == 0
    } else {
        !connection_options
    }
}

/// Check worker grant compatibility before binding the listener. The launcher
/// supplies its live error stream.
pub(super) unsafe fn witchy_validate_connection_launch(
    required: u64,
    supplied: u64,
    per_connection: bool,
    listener: bool,
    connection_options: bool,
    stderr: *mut core::ffi::c_void,
) -> bool {
    unsafe extern "C" {
        fn fputs(text: *const core::ffi::c_char, stream: *mut core::ffi::c_void) -> c_int;
    }
    let allowed = connection_launch_allowed(
        required,
        supplied,
        per_connection,
        listener,
        connection_options,
    );
    if !allowed {
        unsafe {
            fputs(
                c"native Socket entry requires --listen and --instance-per-connection with compatible grants\n".as_ptr(),
                stderr,
            )
        };
    }
    allowed
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any queue the supervisor can hold: it starts empty at place zero with
    /// room for at most 4096, and only `push` and `pop` change it.
    fn any_queue() -> Queue {
        let queue = Queue {
            slots: kani::any(),
            head: kani::any(),
            len: kani::any(),
            capacity: kani::any(),
        };
        kani::assume(holds(&queue));
        queue
    }

    fn holds(queue: &Queue) -> bool {
        queue.capacity <= queue.slots.len()
            && queue.len <= queue.capacity
            && (queue.head < queue.capacity || queue.head == 0)
    }

    /// A push is taken exactly when there is room and the descriptor is one,
    /// and it lands in the place after the last one waiting, inside the ring.
    #[kani::proof]
    fn a_push_lands_after_the_last_waiting() {
        let mut queue = any_queue();
        let (head, len, capacity) = (queue.head, queue.len, queue.capacity);
        let fd: c_int = kani::any();
        let pushed = queue.push(fd);
        assert_eq!(pushed, len < capacity && fd >= 0);
        assert!(holds(&queue) && queue.head == head && queue.capacity == capacity);
        if pushed {
            let place = (head + len) % capacity;
            kani::cover!(place < head, "the ring wraps");
            assert!(queue.len == len + 1 && queue.slots[place] == fd);
        } else {
            assert_eq!(queue.len, len);
        }
    }

    /// A pop gives the descriptor at the head, when one waits, and moves the
    /// head one place round the ring.
    #[kani::proof]
    fn a_pop_takes_the_head() {
        let mut queue = any_queue();
        let (head, len, capacity) = (queue.head, queue.len, queue.capacity);
        let first = queue.slots[head];
        let popped = queue.pop();
        assert!(holds(&queue) && queue.capacity == capacity);
        if len == 0 {
            assert!(popped.is_none() && queue.head == head && queue.len == 0);
        } else {
            kani::cover!(queue.head == 0 && head != 0, "the head wraps");
            assert!(popped == Some(first) && queue.len == len - 1);
            assert_eq!(queue.head, (head + 1) % capacity);
        }
    }

    /// Two descriptors pushed onto an empty queue come back in that order,
    /// wherever the head is, and `room` counts them against the budget.
    #[kani::proof]
    fn descriptors_leave_in_the_order_they_came() {
        let mut queue = any_queue();
        kani::assume(queue.len == 0 && queue.capacity >= 2);
        let (first, second): (c_int, c_int) = (kani::any(), kani::any());
        kani::assume(first >= 0 && second >= 0);
        kani::cover!(queue.head == queue.capacity - 1, "the second wraps");
        assert!(!queue.room(0) && queue.room(1));
        assert!(queue.push(first) && queue.push(second));
        assert!(!queue.room(2) && queue.room(3) == (queue.capacity > 2));
        assert!(queue.pop() == Some(first) && queue.pop() == Some(second));
        assert!(queue.pop().is_none());
    }

    /// The listener's address is written whole or not at all: the host up to
    /// its last colon, a colon and the port in decimal, with brackets round a
    /// host that holds a colon and does not open with one. It is written
    /// exactly when the text has a colon and the buffer has room. This holds
    /// for every port, host text of up to five bytes and buffer of up to
    /// twelve, the longest such address.
    #[kani::proof]
    #[kani::unwind(14)]
    fn a_listener_address_fits_its_buffer() {
        let mut text: [u8; 6] = kani::any();
        text[5] = 0;
        let Ok(host_port) = core::ffi::CStr::from_bytes_until_nul(&text) else {
            return;
        };
        let port: u16 = kani::any();
        let mut buffer = [0u8; 12];
        let room: usize = kani::any();
        kani::assume(room <= buffer.len());
        let length = listener_address(host_port, port, &mut buffer[..room]);
        let bytes = host_port.to_bytes();
        let digits = match port {
            0..=9 => 1,
            10..=99 => 2,
            100..=999 => 3,
            1000..=9999 => 4,
            _ => 5,
        };
        // What the address should be, worked out from the text alone.
        let mut last = None;
        let mut earlier = false;
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b':' {
                earlier |= last.is_some();
                last = Some(at);
            }
            at += 1;
        }
        let Some(host) = last else {
            assert_eq!(length, 0);
            return;
        };
        let bracketed = earlier && bytes[0] != b'[';
        let whole = host + 1 + digits + 2 * usize::from(bracketed);
        kani::cover!(
            length == 0 && room == 5 && bytes.len() == 5 && bytes[3] == b':',
            "no room"
        );
        if room < whole {
            assert_eq!(length, 0);
            let index: usize = kani::any();
            kani::assume(index < buffer.len());
            assert_eq!(buffer[index], 0);
            return;
        }
        assert_eq!(length, whole);
        kani::cover!(bracketed, "a bare IPv6 host gains brackets");
        kani::cover!(port == u16::MAX);
        let colon = length - digits - 1;
        assert_eq!(buffer[colon], b':');
        let mut value = 0u32;
        let mut at = colon + 1;
        while at < length {
            assert!(buffer[at].is_ascii_digit());
            value = value * 10 + u32::from(buffer[at] - b'0');
            at += 1;
        }
        assert_eq!(value, u32::from(port));
        if bracketed {
            assert_eq!(buffer[0], b'[');
            assert_eq!(buffer[colon - 1], b']');
        }
        let index: usize = kani::any();
        kani::assume(index < host);
        assert_eq!(buffer[index + usize::from(bracketed)], bytes[index]);
    }
}
