//! Test suites only (`--features resident`; a default build has none of this): one resident
//! process that starts many runs quickly, for suites that make hundreds of them.
//!
//!   witchy-wasmtime-host --resident=fd:N
//!
//! N is a listening Unix stream socket the caller made and handed over. Each connection asks
//! for one run: its command line, environment, umask, working directory and descriptors (the
//! standard three and any others, at the numbers the run is to have them), exactly what a
//! process started with that command line would have. The resident process forks, and the
//! forked process becomes that run: it closes every descriptor but the request's, puts each at
//! its number, takes the request's environment, umask and directory, and then does what `main`
//! does (`host_main`), from `close_inherited_on_exec` to its exit status. The resident process
//! reports that status on the connection when the run's process has ended. Losing the
//! connection cancels the run with SIGTERM. Closing the resident's control pipe cancels
//! and reaps all active runs before it exits.
//!
//! Isolation. Every run is still a process of its own, so nothing reaches run N+1 from run N
//! through this process's memory or descriptors: N+1 is forked from the resident process, never
//! from N, and the resident process itself
//!   - never parses a grant, runs a guest, reads a secret's descriptor or a module's input: it
//!     passes a request's descriptors on unread and closes them as soon as it has forked;
//!   - zeroes a request's bytes (arguments and environment, where a test's secret value can be)
//!     as soon as it has forked, and keeps none of them;
//!   - is one thread (it compiles on its own thread only, `engine_config`), so a fork copies no
//!     lock another thread held, and no other thread's state;
//!   - calls into AWS-LC once, at its start, for one ML-DSA verification of an all-zero key, so
//!     the module's ML-DSA self-test (fixed vectors) has passed before any fork; it never draws
//!     randomness, so each run's first draw seeds its own generator from the system, as a new
//!     process's does (and AWS-LC reseeds on every draw where it cannot detect a fork, macOS).
//! What runs share is what the resident process holds: Wasmtime engines and modules compiled
//! from exact module bytes. A run looks one up only by its own bytes, compared whole, after its
//! own admission of those bytes under its own grants, and with the engine settings its own flags
//! decide; a miss compiles in the run, which sends the bytes back after it has finished, to be
//! compiled here for the next run. Compiling is a pure function of bytes and settings: a
//! compiled module carries no store, no grant, no fuel and no deadline. Each run makes its own
//! `Store`, `Linker`, grants, fuel, deadline and memory limits, as a process of the CLI does.
//!
//! What differs from a process the caller starts: the run's parent and process group are the
//! resident process's; the deadline starts at the fork, not at `exec` (the engine is already
//! loaded); limits (`setrlimit`) are the resident process's; and traps use signals, not Mach
//! ports. Tests of startup, descriptor inheritance, environment erasure and process isolation
//! run the real executable.
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use wasmtime::{Engine, Module};

/// A request: `WRS1`, the descriptor count and each one's number (CWD: the working directory),
/// the umask, then the body's length and the body (the arguments, then the environment, each a
/// count and then length-prefixed strings). Everything little-endian `u32`.
const MAGIC: &[u8; 4] = b"WRS1";
const MAX_FDS: usize = 64;
const MAX_BODY: usize = 4 << 20;
const CWD: u32 = u32::MAX;
/// The most modules kept, and the most bytes a run may send back to be compiled.
const MAX_COMPILED: usize = 64;
const MAX_FILL: usize = (16 << 20) + 32;

#[cfg(target_os = "macos")]
type Mode = u16;
#[cfg(not(target_os = "macos"))]
type Mode = u32;
unsafe extern "C" {
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn dup2(from: i32, to: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
    fn umask(mask: Mode) -> Mode;
    fn fchdir(fd: i32) -> i32;
    fn pipe(fds: *mut i32) -> i32;
    fn geteuid() -> u32;
    fn _exit(code: i32) -> !;
    #[cfg(target_os = "macos")]
    fn getpeereid(fd: i32, uid: *mut u32, gid: *mut u32) -> i32;
}
const F_DUPFD: i32 = 0;
const F_SETFD: i32 = 2;
const FD_CLOEXEC: i32 = 1;

/// The resident runner if the command line asks for it (`--resident=fd:N`, alone): its exit
/// status once its standard input closes, which is how its caller ends it.
pub fn serve_if_asked() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next()?;
    let number = first.to_str()?.strip_prefix("--resident=")?;
    let listener = match (number.strip_prefix("fd:").and_then(|n| n.parse::<RawFd>().ok()).filter(|n| *n >= 3), args.next()) {
        (Some(fd), None) => fd,
        _ => { say!("usage: witchy-wasmtime-host --resident=fd:N (a listening Unix socket)"); return Some(2); }
    };
    Some(match serve(listener) { Ok(()) => 0, Err(error) => { say!("resident: {error}"); 1 } })
}

struct Compiled { unmetered: bool, reservation: u64, bytes: Vec<u8>, engine: Engine, module: Module }
#[derive(Default)]
struct Cache { engines: Vec<(bool, u64, Engine)>, modules: Vec<Compiled> }
impl Cache {
    /// The engine for these settings: one per settings, made with the resident settings.
    fn engine(&mut self, unmetered: bool, reservation: u64) -> wasmtime::Result<Engine> {
        if let Some((_, _, engine)) = self.engines.iter().find(|(u, r, _)| *u == unmetered && *r == reservation) { return Ok(engine.clone()); }
        let engine = Engine::new(&crate::engine_config(unmetered, reservation, true))?;
        self.engines.push((unmetered, reservation, engine.clone()));
        Ok(engine)
    }
    /// A module compiled from exactly `bytes`, with exactly these settings.
    fn find(&self, unmetered: bool, reservation: u64, bytes: &[u8]) -> Option<(Engine, Module)> {
        self.modules.iter().find(|c| c.unmetered == unmetered && c.reservation == reservation && c.bytes == bytes).map(|c| (c.engine.clone(), c.module.clone()))
    }
    /// What a run sent back: `[1][unmetered][reservation u64][length u64]` and the bytes, whole,
    /// or nothing is kept. Wasmtime validates them as it compiles them.
    fn fill(&mut self, sent: &[u8]) {
        if sent.len() < 18 || sent[0] != 1 || sent[1] > 1 || self.modules.len() >= MAX_COMPILED { return; }
        let unmetered = sent[1] == 1;
        let reservation = u64::from_le_bytes(sent[2..10].try_into().expect("eight bytes"));
        let length = u64::from_le_bytes(sent[10..18].try_into().expect("eight bytes"));
        let bytes = &sent[18..];
        if bytes.len() as u64 != length || self.find(unmetered, reservation, bytes).is_some() { return; }
        let Ok(engine) = self.engine(unmetered, reservation) else { return };
        let Ok(module) = Module::new(&engine, bytes) else { return };
        self.modules.push(Compiled { unmetered, reservation, bytes: bytes.to_vec(), engine, module });
    }
}

/// A run in progress: its process, the connection that asked for it (until the caller goes),
/// and the pipe its process holds until it ends (what it sends back arrives on it).
struct Run { pid: i32, connection: Option<std::os::unix::net::UnixStream>, done: std::fs::File, sent: Vec<u8> }

fn serve(listener: RawFd) -> std::io::Result<()> {
    // SAFETY: the descriptor was given for this on the command line, and nothing else here owns it.
    let listener = std::os::unix::net::UnixListener::from(unsafe { OwnedFd::from_raw_fd(listener) });
    listener.set_nonblocking(true)?;
    let mut cache = Cache::default();
    // Every engine is made now, before any run: the first one decides how traps are handled,
    // process-wide (signals, which a forked process inherits).
    cache.engine(false, 131_072).map_err(std::io::Error::other)?;
    // The crypto module's ML-DSA self-test (fixed test vectors, about 20 ms), which it runs at
    // its first ML-DSA operation in a process: run once here, through a verification of an
    // all-zero key and signature, so no run repeats it. A verification draws no randomness
    // (a key generation would: its pairwise test signs), so this process's generator is never
    // set up, and touches no secret.
    if crate::sha::provider().is_err() { return Err(std::io::Error::other("crypto provider is not in FIPS mode")); }
    let _ = crate::sha::mldsa65_verify(&[0; crate::sha::MLDSA65_PUBLIC_KEY], b"", &[0; crate::sha::MLDSA65_SIGNATURE]);
    let mut runs: Vec<Run> = Vec::new();
    let result = serve_requests(&listener, &mut cache, &mut runs);
    drop(listener);
    // The PIDs are still our unreaped children. Notify every run before waiting on any.
    for run in &runs {
        unsafe { kill(run.pid, 15) };
    }
    for Run { pid, done, .. } in runs {
        // A finishing child may be writing its module to the cache pipe.
        drop(done);
        reap(pid);
    }
    result
}

fn reap(pid: i32) -> i32 {
    let mut status = 0i32;
    while unsafe { waitpid(pid, &mut status, 0) } < 0 {
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return 127;
        }
    }
    // As a caller's `subprocess` reports it: the exit code, or minus the signal.
    if status & 0x7f == 0 { (status >> 8) & 0xff } else { -(status & 0x7f) }
}

fn serve_requests(listener: &std::os::unix::net::UnixListener, cache: &mut Cache, runs: &mut Vec<Run>) -> std::io::Result<()> {
    let mut pending: Vec<Pending> = Vec::new();
    // Room for far more descriptors than any request may have, so the system never truncates
    // the list (a truncated one is refused, never parsed: review, 66 descriptors crashed it).
    let mut control = vec![std::mem::MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(4096))];
    // Ready: the caller may connect.
    std::io::stdout().write_all(b"ready\n")?;
    std::io::stdout().flush()?;
    loop {
        use crate::sys::{poll, PollFd, POLLIN, POLLHUP, POLLERR};
        let mut fds = vec![PollFd { fd: 0, events: POLLIN, revents: 0 }, PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 }];
        for run in runs.iter() {
            fds.push(PollFd { fd: run.done.as_raw_fd(), events: POLLIN, revents: 0 });
            fds.push(PollFd { fd: run.connection.as_ref().map_or(-1, |c| c.as_raw_fd()), events: POLLIN, revents: 0 });
        }
        for request in &pending { fds.push(PollFd { fd: request.connection.as_raw_fd(), events: POLLIN, revents: 0 }); }
        // Until the oldest request still arriving runs out of time, or indefinitely.
        let wait = pending.iter().map(|request| REQUEST_TIME.saturating_sub(request.since.elapsed()).as_millis() as i32 + 1).min().unwrap_or(-1);
        poll(&mut fds, wait)?;
        // Standard input closed (or anything arrived on it): the caller is done with this process.
        if fds[0].revents & (POLLIN | POLLHUP | POLLERR) != 0 { return Ok(()); }
        let mut ended = Vec::new();
        for (index, run) in runs.iter_mut().enumerate() {
            let (done, asked) = (fds[2 + 2 * index].revents, fds[3 + 2 * index].revents);
            if done & (POLLIN | POLLHUP | POLLERR) != 0 {
                let mut chunk = [0u8; 65_536];
                match run.done.read(&mut chunk) {
                    Ok(0) | Err(_) => ended.push(run.pid),
                    Ok(count) => { if run.sent.len() + count <= MAX_FILL { run.sent.extend_from_slice(&chunk[..count]); } else { run.sent = vec![0]; } }
                }
            }
            // The caller stops the run (`T`: SIGTERM, `K`: SIGKILL), or has gone.
            if asked & (POLLIN | POLLHUP | POLLERR) != 0 && let Some(connection) = run.connection.as_mut() {
                let mut byte = [0u8; 1];
                match connection.read(&mut byte) {
                    Ok(1) if byte[0] == b'T' => { unsafe { kill(run.pid, 15) }; }
                    Ok(1) if byte[0] == b'K' => { unsafe { kill(run.pid, 9) }; }
                    Ok(1) => {}
                    Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                    _ => {
                        unsafe { kill(run.pid, 15) };
                        run.connection = None;
                    }
                }
            }
        }
        // Requests still arriving: read what has come; start each one that is whole.
        let base = 2 + 2 * runs.len();
        let mut index = 0;
        pending.retain_mut(|request| {
            let ready = fds[base + index].revents;
            index += 1;
            if ready & (POLLIN | POLLHUP | POLLERR) == 0 { return request.since.elapsed() < REQUEST_TIME; }
            match request.read(&mut control) {
                Ok(false) => true,
                Ok(true) => {
                    match request.start(cache) { Ok(run) => runs.push(run), Err(error) => say!("resident: request refused: {error}") }
                    false
                }
                Err(error) => { say!("resident: request refused: {error}"); false }
            }
        });
        for pid in ended {
            let at = runs.iter().position(|run| run.pid == pid).expect("a run in progress");
            let mut run = runs.swap_remove(at);
            let code = reap(run.pid);
            if let Some(mut connection) = run.connection.take() {
                let _ = connection.set_nonblocking(false);
                let _ = connection.write_all(&[&MAGIC[..], &code.to_le_bytes()].concat());
            }
            cache.fill(&run.sent);
        }
        if fds[1].revents & POLLIN != 0 {
            loop {
                match listener.accept() {
                    Ok((connection, _)) => match Pending::new(connection) { Ok(request) => pending.push(request), Err(error) => say!("resident: request refused: {error}") },
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

/// The caller's user, or the request is refused.
fn same_user(connection: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = { let (mut uid, mut gid) = (0u32, 0u32); if unsafe { getpeereid(connection.as_raw_fd(), &mut uid, &mut gid) } < 0 { return Err(std::io::Error::last_os_error()); } uid };
    #[cfg(not(target_os = "macos"))]
    let uid = rustix::net::sockopt::socket_peercred(connection)?.uid.as_raw();
    if uid != unsafe { geteuid() } { return Err(std::io::Error::other("another user's request")); }
    Ok(())
}

fn word(bytes: &[u8], at: &mut usize) -> std::io::Result<u32> {
    let value = bytes.get(*at..*at + 4).ok_or_else(|| std::io::Error::other("truncated request"))?;
    *at += 4;
    Ok(u32::from_le_bytes(value.try_into().expect("four bytes")))
}

/// How long a request may take to arrive whole.
const REQUEST_TIME: std::time::Duration = std::time::Duration::from_secs(5);

/// A request still arriving, read as it comes and never waited for (review: one caller that
/// sent nothing held up every run for the timeout). Its descriptors are the ones that arrive
/// with its first byte. An invalid descriptor list is refused as soon as it is read.
/// Its bytes are zeroed when it is dropped, whatever happened.
struct Pending { connection: std::os::unix::net::UnixStream, got: Vec<u8>, need: usize, head: bool, fds: Vec<OwnedFd>, since: std::time::Instant }
impl Drop for Pending {
    fn drop(&mut self) { self.got.fill(0); std::hint::black_box(&self.got); }
}
impl Pending {
    fn new(connection: std::os::unix::net::UnixStream) -> std::io::Result<Self> {
        connection.set_nonblocking(true)?;
        same_user(&connection)?;
        Ok(Pending { connection, got: Vec::new(), need: 8, head: false, fds: Vec::new(), since: std::time::Instant::now() })
    }
    /// Reads what has arrived: whether the request is whole. Everything is read with
    /// `recvmsg`, so no descriptor can arrive unseen (review: with plain reads, a descriptor
    /// sent with a later segment was installed here and kept).
    fn read(&mut self, space: &mut [std::mem::MaybeUninit<u8>]) -> std::io::Result<bool> {
        loop {
            if self.got.len() == self.need {
                if !self.head {
                    // The head: then the numbers, the umask and the body's length.
                    if &self.got[..4] != MAGIC { return Err(std::io::Error::other("not a request")); }
                    let count = u32::from_le_bytes(self.got[4..8].try_into().expect("four bytes")) as usize;
                    if count > MAX_FDS || count != self.fds.len() { return Err(std::io::Error::other("descriptor count")); }
                    self.head = true;
                    self.need = 8 + 4 * count + 8;
                    continue;
                }
                if self.need == 8 + 4 * self.fds.len() + 8 {
                    let length = u32::from_le_bytes(self.got[self.need - 4..self.need].try_into().expect("four bytes")) as usize;
                    if length > MAX_BODY { return Err(std::io::Error::other("request too large")); }
                    // Its final size: the body is never moved once any of it has arrived.
                    self.need += length;
                    self.got.reserve_exact(length);
                    if length > 0 { continue; }
                }
                return Ok(true);
            }
            let start = self.got.len();
            self.got.resize(self.need, 0);
            let mut control = rustix::net::RecvAncillaryBuffer::new(space);
            let received = rustix::net::recvmsg(&self.connection, &mut [std::io::IoSliceMut::new(&mut self.got[start..])], &mut control, rustix::net::RecvFlags::empty());
            let received = match received {
                Ok(received) => received,
                Err(error) => {
                    self.got.truncate(start);
                    let error = std::io::Error::from(error);
                    if error.kind() == std::io::ErrorKind::WouldBlock { return Ok(false); }
                    return Err(error);
                }
            };
            let mut arrived: Vec<OwnedFd> = Vec::new();
            let truncated = received.flags.contains(rustix::net::ReturnFlags::CTRUNC);
            if !truncated {
                for message in control.drain() {
                    if let rustix::net::RecvAncillaryMessage::ScmRights(rights) = message {
                        arrived.extend(rights);
                    }
                }
            }
            drop(control);
            self.got.truncate(start + received.bytes);
            if truncated || (start != 0 && !arrived.is_empty()) {
                return Err(std::io::Error::other("descriptors after the request's first byte"));
            }
            self.fds.extend(arrived);
            if received.bytes == 0 { return Err(std::io::Error::other("closed before the request was whole")); }
        }
    }
    /// Forks the whole request's run. Its descriptors are gone from this process when this
    /// returns, and its bytes when it is dropped.
    fn start(&mut self, cache: &Cache) -> std::io::Result<Run> {
        let count = self.fds.len();
        let mut at = 8;
        let mut targets = Vec::with_capacity(count);
        for _ in 0..count { targets.push(word(&self.got, &mut at)?); }
        let mask = word(&self.got, &mut at)?;
        let _length = word(&self.got, &mut at)?;
        // Each number once; the standard three and the working directory always; no number past
        // what a secret's `fd:N` may name.
        let mut sorted = targets.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != count || !sorted.iter().all(|t| *t <= 1023 || *t == CWD) || ![0, 1, 2, CWD].iter().all(|t| sorted.contains(t)) || mask > 0o777 {
            return Err(std::io::Error::other("malformed request"));
        }
        let connection = self.connection.try_clone()?;
        let fds = std::mem::take(&mut self.fds);
        let (pid, done) = fork_run(fds, &targets, mask, &mut self.got[at..], cache)?;
        // A test's secret value can be in the arguments or the environment: none of it stays here.
        self.got.fill(0);
        Ok(Run { pid, connection: Some(connection), done, sent: Vec::new() })
    }
}

fn fork_run(fds: Vec<OwnedFd>, targets: &[u32], mask: u32, body: &mut [u8], cache: &Cache) -> std::io::Result<(i32, std::fs::File)> {
    let mut ends = [0i32; 2];
    if unsafe { pipe(ends.as_mut_ptr()) } < 0 { return Err(std::io::Error::last_os_error()); }
    // SAFETY: `pipe` just made both; nothing else owns them.
    let (done, sends) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    for end in ends { unsafe { fcntl(end, F_SETFD, FD_CLOEXEC) }; }
    let pid = unsafe { fork() };
    if pid < 0 { return Err(std::io::Error::last_os_error()); }
    if pid == 0 { become_run(fds, targets, mask, body, sends, cache) }
    // The run's descriptors are its process's alone now.
    drop(fds);
    drop(sends);
    Ok((pid, std::fs::File::from(done)))
}

/// A forked process that cannot become its run: before its standard streams are its own.
fn fail(_: &str) -> ! { unsafe { _exit(127) } }

/// The forked process: the request's process, then the run. Never returns.
fn become_run(fds: Vec<OwnedFd>, targets: &[u32], mask: u32, body: &mut [u8], sends: OwnedFd, cache: &Cache) -> ! {
    let given: Vec<RawFd> = fds.into_iter().map(|fd| { let raw = fd.as_raw_fd(); std::mem::forget(fd); raw }).collect();
    let sends = { let raw = sends.as_raw_fd(); std::mem::forget(sends); raw };
    // Every descriptor of the resident process's goes: its listener, other runs' connections
    // and pipes, this run's connection. Listed whole before any is closed.
    let listed: Vec<RawFd> = {
        let Ok(dir) = cap_std::fs::Dir::open_ambient_dir("/dev/fd", cap_std::ambient_authority()) else { fail("/dev/fd") };
        let Ok(entries) = dir.entries() else { fail("/dev/fd") };
        entries.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<RawFd>().ok()).collect()
    };
    for number in listed { if !given.contains(&number) && number != sends { unsafe { close(number) }; } }
    // Above every number in use or wanted, then each into place (`dup2` leaves it inherited:
    // not close-on-exec, as a descriptor a process is started with).
    let above = targets.iter().filter(|t| **t != CWD).map(|t| *t as RawFd).chain(given.iter().copied()).chain([sends]).max().unwrap_or(2) + 1;
    let raise = |fd: RawFd| -> RawFd { let moved = unsafe { fcntl(fd, F_DUPFD, above) }; if moved < 0 { fail("dup") } unsafe { close(fd) }; moved };
    let moved: Vec<RawFd> = given.iter().map(|fd| raise(*fd)).collect();
    let sends = raise(sends);
    unsafe { fcntl(sends, F_SETFD, FD_CLOEXEC) };
    for (fd, target) in moved.iter().zip(targets) {
        if *target == CWD { if unsafe { fchdir(*fd) } < 0 { fail("cwd") } }
        else if unsafe { dup2(*fd, *target as RawFd) } < 0 { fail("dup2") }
        unsafe { close(*fd) };
    }
    unsafe { umask(mask as Mode) };
    // The request's arguments and environment, and nothing of the resident process's.
    let mut at = 0;
    let strings = |body: &[u8], at: &mut usize| -> Option<Vec<Vec<u8>>> {
        let count = word(body, at).ok()? as usize;
        let mut out = Vec::new();
        for _ in 0..count { let length = word(body, at).ok()? as usize; out.push(body.get(*at..*at + length)?.to_vec()); *at += length; }
        Some(out)
    };
    let (args, mut environment) = (strings(body, &mut at), strings(body, &mut at));
    // This process's copy of the request (the resident process clears its own once it has
    // forked): cleared as the executable clears the environment it was started with.
    body.fill(0);
    std::hint::black_box(&body);
    let (Some(args), Some(environment)) = (args, environment.as_mut()) else { say!("resident: malformed request"); unsafe { _exit(1) } };
    let Ok(args) = args.into_iter().map(String::from_utf8).collect::<Result<Vec<String>, _>>() else { say!("resident: an argument is not UTF-8"); unsafe { _exit(1) } };
    // SAFETY: one thread (the forked one), and nothing reads the environment meanwhile.
    unsafe {
        for (name, _) in std::env::vars_os() { std::env::remove_var(name); }
        use std::os::unix::ffi::OsStrExt;
        for entry in environment.iter() {
            let Some(split) = entry.iter().position(|b| *b == b'=').filter(|at| *at > 0) else { say!("resident: malformed environment"); _exit(1) };
            std::env::set_var(std::ffi::OsStr::from_bytes(&entry[..split]), std::ffi::OsStr::from_bytes(&entry[split + 1..]));
        }
    }
    // Only the environment itself holds a value now (where `take_secret_variables` clears it).
    for entry in environment.iter_mut() { entry.fill(0); }
    std::hint::black_box(&environment);
    let mut sent = None;
    let mut compile = |unmetered: bool, reservation: u64, bytes: &[u8]| -> wasmtime::Result<(Engine, Module)> {
        if let Some(found) = cache.find(unmetered, reservation, bytes) { return Ok(found); }
        let engine = match cache.engines.iter().find(|(u, r, _)| *u == unmetered && *r == reservation) {
            Some((_, _, engine)) => engine.clone(),
            None => Engine::new(&crate::engine_config(unmetered, reservation, true))?,
        };
        let module = Module::new(&engine, bytes)?;
        sent = Some([&[1u8, u8::from(unmetered)][..], &reservation.to_le_bytes(), &(bytes.len() as u64).to_le_bytes(), bytes].concat());
        Ok((engine, module))
    };
    let code = crate::host_main(&args, &[sends as u32], &mut compile);
    let _ = std::io::stdout().flush();
    // The module's bytes, for the next run, once this one is over (never anything else).
    if let Some(bytes) = sent {
        // SAFETY: `sends` is this process's own pipe end, moved above.
        let mut pipe = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(sends) });
        let _ = pipe.write_all(&bytes);
        std::mem::forget(pipe);
    }
    std::process::exit(code)
}
