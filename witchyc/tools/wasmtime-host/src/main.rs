//! The Witchy runtime: runs one module under the grants given on the command line.
//!
//!   witchy-wasmtime-host (MODULE.wasm | --module-fd=N) [--output] [--allocator] [--fuel=N] [--memory-pages=N]
//!       [--module-bytes=N] [--module-pin=sha384:HEX] [--deadline-ms=N] [--input=FILE | --input-stdin | --sqlite-request-stdin] [--dir=DIR] ...
//!       (docs/wasm-memory-sizes.md, docs/wasm-capabilities.md)
//!
//! The module is checked against the closed profile (profile.rs) before Wasmtime compiles
//! it. No WASI, no native fallback, and nothing else: this process is the whole runtime.
/// Every line this engine writes about a run goes through `said::line`, and so begins
/// `witchy: `: the one way to standard error in this crate (a test reads the sources for any
/// other). A program's own output that begins so, or as the launcher's lines do, is marked
/// (`stream::plain`), so nothing a program prints reads as said by the engine or the launcher.
pub mod said {
    /// What every line of the engine's begins with.
    pub const PREFIX: &str = "witchy: ";
    /// What every line of the launcher's begins with (Grimoire's `say`).
    pub const LAUNCHER: &str = "grimoire: ";
    /// `text`, each of its lines with the prefix and a newline.
    pub fn prefixed(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        for line in text.strip_suffix('\n').unwrap_or(text).split('\n') { out.push_str(PREFIX); out.push_str(line); out.push('\n'); }
        out
    }
    /// Said, in one write. A failed write is ignored and never a panic: standard error may be
    /// closed, and the thread that ends a run at its deadline says its line through here.
    pub fn line(text: &str) { use std::io::Write; let _ = std::io::stderr().lock().write_all(prefixed(text).as_bytes()); }
}
macro_rules! say { ($($arg:tt)*) => { crate::said::line(&format!($($arg)*)) } }
mod profile;
mod test_batch;
#[cfg(feature = "resident")]
mod resident;
mod sha;
mod mldsa_signer;
mod local_service;
mod aead;
mod key_agreement;
mod x509;
mod tls_signer;
mod sqlite;
mod exec_child;
use std::io::{Read, Write};
// Files and directories: every path a program uses is resolved through a granted `Dir` (`Files`).
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::Dir;
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

struct State { edge_limit: usize, hits: std::collections::BTreeSet<u16>, test_control: bool, limits: Counted,
    /// What the instruction count was last set to (the start's `--fuel`, then each refill).
    interval: u64, interval_started: bool, output: Vec<u8>, output_limit: usize, streamed: Option<usize>, refill: Option<u64>, active: bool, input: Vec<u8>, taken: usize,
    /// Whose the output budget is (`Share`). `lines`: streamed output is put together here and
    /// passed on a whole line at a time (an instance for each connection: no line of one is
    /// mixed with another's). `source`: standard input as a stream (`InputSource`).
    output_share: Share, lines: Option<Vec<u8>>, source: Option<InputSource>, files: Files, network: Network, programs: Programs, secrets: Secrets,
    sqlite: Option<sqlite::Grant>,
    local_service: Option<local_service::Grant>,
    agreements: key_agreement::Pending,
    env_variables: std::collections::BTreeMap<Vec<u8>, Vec<u8>>, env_name: Vec<u8>, env_value: Option<Vec<u8>>,
    grants: std::collections::BTreeMap<Vec<u8>, i64>, grant_name: Vec<u8>,
    /// `main`'s `Args`: the words the launch gave (`--arg=`), and the one last chosen.
    words: Vec<Vec<u8>>, word: Option<usize>,
    #[cfg(feature = "guest-profile")] samples: profile_guest::Samples }

/// The store's limits, and how many bytes of memory the instance has been given so far (for
/// the `limit: used:` line): the limits decide, this only counts what they allowed.
struct Counted { inner: StoreLimits, bytes: usize }
impl Counted { fn new(reservation: u64) -> Self { Self { inner: StoreLimitsBuilder::new().memory_size(reservation as usize).memories(2).tables(0).instances(1).build(), bytes: 0 } } }
impl wasmtime::ResourceLimiter for Counted {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let allowed = self.inner.memory_growing(current, desired, maximum)?;
        if allowed { self.bytes += desired.saturating_sub(current); }
        Ok(allowed)
    }
    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> { self.inner.memory_grow_failed(error) }
    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> { self.inner.table_growing(current, desired, maximum) }
    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> { self.inner.table_grow_failed(error) }
    fn instances(&self) -> usize { self.inner.instances() }
    fn tables(&self) -> usize { self.inner.tables() }
    fn memories(&self) -> usize { self.inner.memories() }
}
/// Whose a budget is, where the program runs once for each connection: said when it is
/// reached. `Run`: the whole run's (always, for a program run once). `Connection`: this
/// instance's own (`--…-per-connection`); reaching it ends nothing but what this instance
/// may still do. `Rest(ceiling)`: what was left of the whole run's `ceiling` when this
/// instance began, which was less than a connection's own.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Share { #[default] Run, Connection, Rest(u64) }
impl Share { fn of_run(self, budget: u64) -> u64 { match self { Share::Rest(ceiling) => ceiling, _ => budget } } }
/// `--usage`: what the run used of each of its limits, said in one `limit: used:` line when
/// it ends, so an operator can size them. The counts are the store's; they are copied here
/// when the program accepts a connection and when it ends (a few stores of a number: nothing
/// on any path that moves bytes), so that the threads that end a run from outside (the
/// deadline, the lifeline) can say them too, as of the last copy.
mod usage {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    pub static ASKED: AtomicBool = AtomicBool::new(false);
    static COPIED: AtomicBool = AtomicBool::new(false);
    static ENDED: AtomicBool = AtomicBool::new(false);
    static START: AtomicU64 = AtomicU64::new(0); static PEAK: AtomicU64 = AtomicU64::new(0);
    static NET: AtomicU64 = AtomicU64::new(0); static FILES: AtomicU64 = AtomicU64::new(0); static MADE: AtomicU64 = AtomicU64::new(0);
    static OUTPUT: AtomicU64 = AtomicU64::new(0); static MEMORY: AtomicU64 = AtomicU64::new(0);
    static CONNECTIONS: AtomicU64 = AtomicU64::new(0); static FAILED: AtomicU64 = AtomicU64::new(0);
    /// The most one connection's instance used of each budget, and the input taken so far.
    static NET_MOST: AtomicU64 = AtomicU64::new(0); static FILES_MOST: AtomicU64 = AtomicU64::new(0); static MADE_MOST: AtomicU64 = AtomicU64::new(0); static OUTPUT_MOST: AtomicU64 = AtomicU64::new(0);
    static INPUT: AtomicU64 = AtomicU64::new(0);
    /// The limits themselves, set once before the program starts: what each count is out of.
    /// `fuel`: the start's count and each connection's, if counted. A `None` is not granted.
    pub struct Limits { pub fuel: Option<(u64, Option<u64>)>, pub each: bool, pub net: Option<u64>, pub files: Option<(u64, u64)>, pub output: Option<u64>, pub memory: u64,
        /// A streamed input's budget, and each connection's own budgets: network bytes, file
        /// bytes, names made, output bytes.
        pub input: Option<u64>, pub own: [Option<u64>; 4] }
    static LIMITS: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();
    pub fn limits(limits: Limits) { let _ = LIMITS.set(limits); }
    /// The instructions of one interval: the start (`first`), or the work after one accepted
    /// connection, of which the most is kept.
    pub fn interval(first: bool, used: u64) { if first { START.store(used, Relaxed); } else { PEAK.fetch_max(used, Relaxed); } }
    pub fn connection(failed: bool) { CONNECTIONS.fetch_add(1, Relaxed); if failed { FAILED.fetch_add(1, Relaxed); } }
    pub fn copy(state: &super::State, ended: bool) {
        NET.store(state.network.used, Relaxed); FILES.store(state.files.used, Relaxed); MADE.store(state.files.made, Relaxed);
        OUTPUT.store(state.streamed.unwrap_or(state.output.len()) as u64, Relaxed); MEMORY.fetch_max(state.limits.bytes as u64, Relaxed);
        COPIED.store(true, Relaxed); if ended { ENDED.store(true, Relaxed); }
    }
    /// One connection's instance is over: the run's totals so far (the ledger's, which only
    /// grow), what this instance used of each, and the memory it had.
    pub fn settled(totals: [u64; 4], own: [u64; 4], memory: u64) {
        for (count, value) in [&NET, &FILES, &MADE, &OUTPUT].into_iter().zip(totals) { count.fetch_max(value, Relaxed); }
        for (count, value) in [&NET_MOST, &FILES_MOST, &MADE_MOST, &OUTPUT_MOST].into_iter().zip(own) { count.fetch_max(value, Relaxed); }
        MEMORY.fetch_max(memory, Relaxed);
        COPIED.store(true, Relaxed);
    }
    /// The run is over: the counts are final.
    pub fn ended() { COPIED.store(true, Relaxed); ENDED.store(true, Relaxed); }
    /// Bytes of a streamed input handed to the program so far.
    pub fn input(taken: u64) { INPUT.store(taken, Relaxed); }
    /// A budget's size as said: a number, or `unlimited`.
    pub fn amount(budget: u64) -> String { if budget == u64::MAX { "unlimited".to_owned() } else { budget.to_string() } }
    /// The line, or nothing where it was not asked for.
    pub fn line() -> Option<String> {
        if !ASKED.load(Relaxed) { return None; }
        Some(line_for(LIMITS.get()?))
    }
    /// The line for these limits (the run's, set once; a test's own).
    pub fn line_for(limits: &Limits) -> String {
        let name = super::limit::name;
        if !COPIED.load(Relaxed) {
            // A pipe tool stopped part-way has still read what it read.
            return match limits.input {
                Some(input) => format!("limit: used: input {} of {} bytes ({}); nothing else was counted: the run was stopped before it ended", INPUT.load(Relaxed), amount(input), name("--input-bytes")),
                None => "limit: used: not counted: the run was stopped before it ended or accepted a connection".to_owned(),
            };
        }
        // A connection's own budget beside the run's: the most one instance used of it.
        let own = |at: usize, most: &AtomicU64, flag: &'static str, unit: &str| limits.own[at].map_or_else(String::new, |each| format!(", at most {} of {each}{unit} in one connection ({})", most.load(Relaxed), name(flag)));
        let mut parts = Vec::new();
        match limits.fuel {
            None => parts.push("instructions not counted".to_owned()),
            Some((count, _)) if limits.each => parts.push(format!("instructions at most {} of {count} in one connection ({})", PEAK.load(Relaxed), name("--fuel"))),
            Some((start, Some(each))) => parts.push(format!("instructions {} of {start} at the start ({}), at most {} of {each} after one accepted connection ({})", START.load(Relaxed), name("--fuel"), PEAK.load(Relaxed), name("--fuel-per-accept"))),
            Some((count, None)) => parts.push(format!("instructions {} of {count} ({})", START.load(Relaxed), name("--fuel"))),
        }
        if limits.each { parts.push(format!("connections {}, of which {} ended early", CONNECTIONS.load(Relaxed), FAILED.load(Relaxed))); }
        if let Some(net) = limits.net { parts.push(format!("network {} of {} bytes ({}){}", NET.load(Relaxed), amount(net), name("--net-bytes"), own(0, &NET_MOST, "--net-bytes-per-connection", ""))); }
        if let Some((bytes, entries)) = limits.files { parts.push(format!("files {} of {} bytes ({}){}, {} of {} made ({}){}", FILES.load(Relaxed), amount(bytes), name("--fs-bytes"), own(1, &FILES_MOST, "--fs-bytes-per-connection", ""),
            MADE.load(Relaxed), amount(entries), name("--fs-entries"), own(2, &MADE_MOST, "--fs-entries-per-connection", " made"))); }
        if let Some(output) = limits.output { parts.push(format!("output {} of {} bytes ({}){}", OUTPUT.load(Relaxed), amount(output), name("--output-bytes"), own(3, &OUTPUT_MOST, "--output-bytes-per-connection", ""))); }
        if let Some(input) = limits.input { parts.push(format!("input {} of {} bytes ({})", INPUT.load(Relaxed), amount(input), name("--input-bytes"))); }
        parts.push(format!("memory {} of {} pages", MEMORY.load(Relaxed).div_ceil(65536), limits.memory));
        format!("limit: used: {}{}", parts.join("; "), if ENDED.load(Relaxed) { "" } else if limits.each { " (as of the last connection that ended)" } else { " (as of the last accepted connection)" })
    }
    pub fn say() { if let Some(line) = line() { crate::said::line(&line); } }
    #[cfg(test)]
    pub fn forget() { for flag in [&ASKED, &COPIED, &ENDED] { flag.store(false, Relaxed); } for count in [&START, &PEAK, &NET, &FILES, &MADE, &OUTPUT, &MEMORY, &CONNECTIONS, &FAILED, &NET_MOST, &FILES_MOST, &MADE_MOST, &OUTPUT_MOST, &INPUT] { count.store(0, Relaxed); } }
}
/// The directory a run may reach and what it has opened there. Handles are indices into
/// `entries` plus `FIRST_HANDLE`; the guest only ever holds numbers, and every operation
/// checks the entry's kind and right again. The host reads and writes no guest memory: a path
/// arrives one byte at a time, and contents leave and arrive one byte at a time.
///
/// A directory is held as a cap-std `Dir`, opened once when the run starts: nothing after that
/// is looked up by an ambient path. Every open, create, remove and rename goes through a `Dir`,
/// one plain name at a time, never following a symbolic link (`Files::resolve`), so moving or
/// replacing a directory on the way while the run works cannot redirect it outside the grant.
/// A file being written keeps its directory's `Dir` and its name.
#[derive(Default)]
struct Files { named_dirs: Vec<(Dir, u8)>, read_aliases: Vec<(Vec<u8>, Dir)>, alias_root: Option<i64>,
    root: Option<Dir>, write_root: Option<Dir>,
    file_read: Option<(Dir, std::ffi::OsString)>, file_write: Option<(Dir, std::ffi::OsString)>, read: bool, write: bool, budget: u64, used: u64,
    /// Names this run has made (files and directories), and the most it may make (`--fs-entries`).
    made: u64, entry_limit: u64, entries: Vec<Entry>,
    /// Whose the byte budget and the count of names are, for the line said when one is reached.
    share: Share, entry_share: Share,
    /// Each `fd:` secret source's device and inode (`Secrets::objects`, with any
    /// `--secret-object` a launcher gave): a file that is one of them is never read, replaced,
    /// removed or renamed through a grant, whatever its name or link (one source, one route).
    secret_objects: Vec<(u64, u64)>,
    /// `--data-only`, a directory the run may write: that directory, and the engine's staging
    /// directory in it once it is open (`Files::staging_in`).
    data_root: Option<Dir>, staging: Option<Dir> }
enum Entry {
    /// `from`: the path kept by `fs_rename_from` for the next `fs_rename`.
    Dir { dir: Dir, read: bool, write: bool, pending: Vec<u8>, from: Vec<u8> },
    Reading { data: Vec<u8>, at: usize },
    /// Contents reach the disk at close, whole, by renaming a finished sibling into place.
    /// `exclusive`: created only if nothing is at the name when it is finished.
    /// `append`: the file is there and `data` is added at its end at close (`Files::append`).
    Writing { parent: Dir, name: std::ffi::OsString, data: Vec<u8>, exclusive: bool, append: bool },
    Failed(i64),
    Closed,
}
const FIRST_HANDLE: i64 = 16;
const MAX_HANDLES: usize = 4096;
const MAX_PATH: usize = 1024;
/// The most entries a rename looks through below a directory it moves, while the run has a
/// secret source to keep in place (`Files::guard`).
const MAX_GUARDED: usize = 65_536;
// Error codes, as library/wasm-fs.wy reads them.
const NOT_FOUND: i64 = 1; const INVALID_PATH: i64 = 2; const TOO_LARGE: i64 = 3; const WRONG_KIND: i64 = 4; const IO: i64 = 5; const EXISTS: i64 = 6;

/// The few POSIX calls std does not offer: descriptors, signals, sockets. Files and
/// directories go through cap-std (`Files`).
mod sys {
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::os::fd::{FromRawFd, OwnedFd};
    #[cfg(target_os = "macos")]
    mod c { pub const DONTWAIT: i32 = 0x80; pub const ELOOP: i32 = 62; pub const SOL_SOCKET: i32 = 0xffff; pub const SO_REUSEPORT: i32 = 0x0200; }
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    mod c { pub const DONTWAIT: i32 = 0x40; pub const ELOOP: i32 = 40; pub const SOL_SOCKET: i32 = 1; pub const SO_REUSEPORT: i32 = 15; }
    pub use c::ELOOP;
    unsafe extern "C" {
        pub fn fchdir(fd: c_int) -> c_int;
        fn geteuid() -> u32;
        fn signal(signal: c_int, handler: Option<extern "C" fn(c_int)>) -> usize;
        fn _exit(code: c_int) -> !;
    }
    unsafe extern "C" { fn fcntl(fd: c_int, command: c_int, ...) -> c_int; }
    /// End the process now: no exit handler, no flush, no lock.
    pub fn exit_now(code: i32) -> ! { unsafe { _exit(code) } }
    /// Unclaimed inherited descriptors numbered 3 and up. A host-owned or
    /// previously claimed descriptor number cannot be taken again.
    static INHERITED: std::sync::OnceLock<std::sync::Mutex<Vec<u32>>> = std::sync::OnceLock::new();
    /// Marks every descriptor from 3 up that this process was started with close-on-exec, so no
    /// program a module starts through `Exec` inherits one, and remembers which they were. Called
    /// first in `main`, while the process is one thread and has opened nothing of its own. The
    /// list is the system's own (`/dev/fd`); a process that cannot list it does not run. The
    /// listing is read whole and closed before any number is looked at, so the descriptors the
    /// listing itself used are closed by then and are not taken for inherited ones.
    ///
    /// `own`: descriptors the host itself holds for its own use before this is called (only
    /// the resident runner's, `resident.rs`): marked close-on-exec like the rest, but never
    /// taken for ones the process was started with, so no secret flag can name them.
    pub fn close_inherited_on_exec(own: &[u32]) -> std::io::Result<()> {
        let mut listed = Vec::new();
        for entry in std::fs::read_dir("/dev/fd")? {
            let Some(number) = entry?.file_name().to_str().and_then(|n| n.parse::<c_int>().ok()) else { return Err(std::io::Error::other("unexpected /dev/fd entry")) };
            listed.push(number);
        }
        let mut inherited = Vec::new();
        for number in listed {
            if number < 3 { continue; }
            // SAFETY: F_GETFD and F_SETFD change only this descriptor's close-on-exec flag. A
            // descriptor that is closed now (the listing's own) is not an error.
            if unsafe { fcntl(number, 1) } < 0 { continue; }
            check(unsafe { fcntl(number, 2, 1 as c_int) })?;
            if own.contains(&(number as u32)) { continue; }
            inherited.push(number as u32);
        }
        let _ = INHERITED.set(std::sync::Mutex::new(inherited));
        Ok(())
    }
    /// The descriptor `number` this process was started with, now owned here (closed when
    /// dropped) and closed on exec, so no program the run starts inherits it. F_GETFD and
    /// F_SETFD are 1 and 2, FD_CLOEXEC 1, on both systems.
    pub fn take_descriptor(number: u32) -> std::io::Result<OwnedFd> {
        let fd = c_int::try_from(number).map_err(std::io::Error::other)?;
        // Only one the process was started with: a number the host itself opened since (a
        // directory, the module) is not a source.
        let unavailable = || std::io::Error::other("descriptor was not inherited or was already selected");
        let mut inherited = INHERITED.get().ok_or_else(unavailable)?.lock().map_err(|_| unavailable())?;
        let index = inherited.iter().position(|known| *known == number).ok_or_else(unavailable)?;
        check(unsafe { fcntl(fd, 1) })?;
        check(unsafe { fcntl(fd, 2, 1 as c_int) })?;
        inherited.swap_remove(index);
        // SAFETY: the descriptor was inherited and open, and is transferred to an owner once.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
    /// Why a secret's descriptor gave no value.
    pub enum ReadFailure { Unreadable, TooLarge, NotClosed }
    /// Everything `fd` holds up to its end, read before `until`: more than `limit` bytes, or an
    /// end not reached by then (a writer that never closes), is a failure. The end is a read that
    /// returns 0 bytes, and nothing else: on macOS `poll` never reports the end of a named FIFO
    /// whose writer has closed (it waited out the bound with the whole value read). So a thread
    /// of its own reads with ordinary blocking reads, and this waits for it no later than
    /// `until`. The descriptor is never switched to non-blocking (it may be shared with another
    /// process). A reader still waiting at `until` keeps the descriptor until the process exits,
    /// which follows: a slot that fails stops the run.
    pub fn read_until(fd: OwnedFd, limit: usize, until: std::time::Instant) -> Result<Vec<u8>, ReadFailure> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::Builder::new().name("secret-read".into()).spawn(move || {
            let mut file = std::fs::File::from(fd);
            // Room for the whole bound from the start, so growing never leaves a freed partial
            // copy of the value behind (review of the signer: a seed arriving in pieces).
            let mut bytes = Vec::with_capacity(limit.saturating_add(4096));
            let mut buffer = [0u8; 4096];
            let result = loop {
                match std::io::Read::read(&mut file, &mut buffer) {
                    Ok(0) => break Ok(std::mem::take(&mut bytes)),
                    Ok(count) => {
                        if bytes.len() + count > bytes.capacity() { break Err(ReadFailure::TooLarge); }
                        bytes.extend_from_slice(&buffer[..count]);
                        if bytes.len() > limit { break Err(ReadFailure::TooLarge); }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break Err(ReadFailure::Unreadable),
                }
            };
            // What was read of a value that failed is not kept, nor the last read's buffer.
            bytes.fill(0);
            buffer.fill(0);
            std::hint::black_box(&buffer);
            drop(file);
            // A value the waiter gave up on is not kept either.
            if let Err(std::sync::mpsc::SendError(Ok(mut late))) = send.send(result) { late.fill(0); }
        });
        if reader.is_err() { return Err(ReadFailure::Unreadable); }
        match receive.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(result) => result,
            Err(_) => Err(ReadFailure::NotClosed),
        }
    }
    #[cfg(target_os = "linux")]
    unsafe extern "C" { static mut environ: *mut *mut c_char; fn prctl(option: c_int, ...) -> c_int; }
    /// PR_SET_DUMPABLE (4) to 0: same-user processes can no longer read this one's memory.
    /// An error if the call fails: the caller stops rather than run readable.
    #[cfg(target_os = "linux")]
    pub fn not_dumpable() -> std::io::Result<()> { check(unsafe { prctl(4, 0 as std::ffi::c_ulong) }).map(drop) }
    #[cfg(target_os = "macos")]
    unsafe extern "C" { fn _NSGetEnviron() -> *mut *mut *mut c_char; }
    /// The variable `name`'s value, taken out of this process: its bytes are overwritten where
    /// the process was started with them (the copy `/proc/PID/environ` on Linux and `ps -E` on
    /// macOS read), then it is unset. Called first in `main`, while the process is one thread.
    pub fn take_variable(name: &str) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStringExt;
        let value = std::env::var_os(name)?.into_vec();
        // SAFETY: one thread (nothing else reads the environment meanwhile); each entry is a
        // NUL-terminated string of the process's initial environment, and only its bytes after
        // `NAME=` are written, within the string.
        unsafe {
            #[cfg(target_os = "linux")] let mut entry = environ;
            #[cfg(target_os = "macos")] let mut entry = *_NSGetEnviron();
            while !entry.is_null() && !(*entry).is_null() {
                let text = *entry;
                let length = CStr::from_ptr(text).to_bytes().len();
                let bytes = std::slice::from_raw_parts(text.cast::<u8>(), length);
                if bytes.len() > name.len() && bytes.starts_with(name.as_bytes()) && bytes[name.len()] == b'=' {
                    for at in name.len() + 1..length { std::ptr::write_volatile(text.add(at), 0); }
                }
                entry = entry.add(1);
            }
            std::env::remove_var(name);
        }
        Some(value)
    }
    fn check(result: c_int) -> std::io::Result<c_int> { if result < 0 { Err(std::io::Error::last_os_error()) } else { Ok(result) } }
    /// Where the handler below writes the signal's number: a pipe a thread of the run reads
    /// (`stopped_by_signal`); `NO_ONE` before there is one; `TOLD` once a signal was handed on.
    static STOPPED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(NO_ONE);
    const NO_ONE: c_int = -1;
    const TOLD: c_int = -2;
    /// On SIGTERM or SIGINT, tell the run's shutdown thread, which stops the process group,
    /// reports usage, and exits. Only async-signal-safe calls here. If
    /// there is no one to tell, or the write fails, the process exits here. A signal after
    /// one was handed on (Ctrl-C reaches both Grimoire and the engine, and Grimoire then sends
    /// SIGTERM) changes nothing: the thread told is already ending the run, and its own timer
    /// ends the process a quarter of a second later whatever it waits on (`ending`).
    pub fn install_shutdown_signal_handlers() -> bool {
        use rustix::process::Signal;
        extern "C" fn terminated(number: c_int) {
            // Preserve the interrupted thread's errno.
            let errno = unsafe { errno_location() };
            let saved = unsafe { *errno };
            let pipe = STOPPED.swap(TOLD, std::sync::atomic::Ordering::SeqCst);
            if pipe != TOLD {
                let byte = number as u8;
                if pipe < 0 || unsafe { write(pipe, (&raw const byte).cast(), 1) } != 1 { unsafe { _exit(if number == 2 { 130 } else { 124 }) } }
            }
            unsafe { *errno = saved };
        }
        unsafe extern "C" {
            #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
            #[cfg_attr(not(target_os = "linux"), link_name = "__error")]
            fn errno_location() -> *mut c_int;
        }
        // Ignore inherited SIGCHLD dispositions that would reap our process-group leader
        // before the host can safely signal the group.
        unsafe {
            signal(Signal::CHILD.as_raw(), None) != usize::MAX
                && signal(Signal::TERM.as_raw(), Some(terminated)) != usize::MAX
                && signal(Signal::INT.as_raw(), Some(terminated)) != usize::MAX
        }
    }
    /// The signal that stopped the run, when one does: waits for the handler above. `None` if
    /// no pipe could be made (a signal then ends the process in the handler, as before).
    pub fn stopped_by_signal() -> Option<impl FnOnce() -> u8> {
        use std::os::fd::IntoRawFd;
        let (mut reader, writer) = std::io::pipe().ok()?;
        // Only where no signal has come yet: one that came first has ended the process already.
        let _ = STOPPED.compare_exchange(NO_ONE, writer.into_raw_fd(), std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst);
        Some(move || { use std::io::Read; let mut byte = [0u8]; loop { match reader.read(&mut byte) { Ok(1) => return byte[0], Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}, _ => loop { std::thread::park() } } } })
    }
    unsafe extern "C" { fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize; fn listen(fd: c_int, backlog: c_int) -> c_int; fn getrlimit(resource: c_int, limit: *mut [u64; 2]) -> c_int; fn setrlimit(resource: c_int, limit: *const [u64; 2]) -> c_int; }
    /// Asks the system to queue up to `waiting` connections not yet accepted on a socket that
    /// already listens (std asks for 128). The system's own most still applies
    /// (`kern.ipc.somaxconn`, 128 by default on macOS; `net.core.somaxconn` on Linux), which
    /// is why the engine also accepts ahead of its instances. A refusal changes nothing.
    pub fn listen_again(fd: c_int, waiting: usize) { let _ = unsafe { listen(fd, c_int::try_from(waiting).unwrap_or(c_int::MAX)) }; }
    /// How many of `wanted` descriptors this process can hold beside `others`: its soft limit
    /// on open files is raised toward its hard limit for them (a shell's default of 256 would
    /// hold fewer waiting connections than a service is asked for), never above 10240, and
    /// what still does not fit is not promised. At least one.
    pub fn room_for_descriptors(wanted: usize, others: usize) -> usize {
        const NOFILE: c_int = if cfg!(target_os = "linux") { 7 } else { 8 };
        let mut limit = [0u64; 2];
        if unsafe { getrlimit(NOFILE, &raw mut limit) } != 0 { return wanted.min(64); }
        let needed = (wanted + others) as u64;
        if limit[0] < needed {
            let raised = [needed.min(limit[1]).min(10_240).max(limit[0]), limit[1]];
            if unsafe { setrlimit(NOFILE, &raw const raised) } == 0 { limit = raised; }
        }
        wanted.min((limit[0] as usize).saturating_sub(others)).max(1)
    }
    /// What `accept` may fail with for a while and then succeed: the connection was gone before
    /// it was accepted, or the process or system has no descriptor or memory for it now.
    pub const TRANSIENT_ACCEPT: [i32; 6] = if cfg!(target_os = "linux") { [103, 24, 23, 105, 12, 71] } else { [53, 24, 23, 55, 12, 100] };
    /// Stop every process in the group `group` leads (SIGKILL).
    /// One descriptor to wait on, as `poll(2)` takes it (the same layout on macOS and Linux).
    #[repr(C)]
    pub struct PollFd { pub fd: c_int, pub events: i16, pub revents: i16 }
    pub const POLLIN: i16 = 0x1; pub const POLLOUT: i16 = 0x4; pub const POLLERR: i16 = 0x8; pub const POLLHUP: i16 = 0x10;
    unsafe extern "C" {
        #[link_name = "poll"]
        fn poll_raw(fds: *mut PollFd, count: std::ffi::c_uint, timeout: c_int) -> c_int;
        #[link_name = "recv"]
        fn recv_raw(fd: c_int, buffer: *mut c_void, length: usize, flags: c_int) -> isize;
        #[link_name = "send"]
        fn send_raw(fd: c_int, buffer: *const c_void, length: usize, flags: c_int) -> isize;
    }
    /// Wait until one of `fds` is ready or `timeout` milliseconds pass; how many are ready.
    pub fn poll(fds: &mut [PollFd], timeout: i32) -> std::io::Result<usize> {
        loop {
            let ready = unsafe { poll_raw(fds.as_mut_ptr(), fds.len() as std::ffi::c_uint, timeout) };
            if ready >= 0 { return Ok(ready as usize); }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted { return Err(error); }
        }
    }
    /// What a socket has received, without waiting: `WouldBlock` when nothing has.
    pub fn try_recv(fd: c_int, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = unsafe { recv_raw(fd, buffer.as_mut_ptr().cast(), buffer.len(), c::DONTWAIT) };
        if count < 0 { Err(std::io::Error::last_os_error()) } else { Ok(count as usize) }
    }
    /// As much of `buffer` as the socket takes now: `WouldBlock` when it takes nothing.
    pub fn try_send(fd: c_int, buffer: &[u8]) -> std::io::Result<usize> {
        let count = unsafe { send_raw(fd, buffer.as_ptr().cast(), buffer.len(), c::DONTWAIT) };
        if count < 0 { Err(std::io::Error::last_os_error()) } else { Ok(count as usize) }
    }
    unsafe extern "C" {
        fn setsockopt(fd: c_int, level: c_int, name: c_int, value: *const c_void, length: u32) -> c_int;
        fn getsockopt(fd: c_int, level: c_int, name: c_int, value: *mut c_void, length: *mut u32) -> c_int;
    }
    /// Turns `SO_REUSEPORT` off on a bound socket and reads it back: with it on, another socket
    /// with it on (of this user) binds the same address and port, and the system shares the
    /// connections out between them. Off, a second bind of the address is refused on macOS and
    /// Linux whatever the second socket sets. An option that cannot be read back off is an error.
    pub fn refuse_port_sharing(fd: c_int) -> std::io::Result<()> {
        let off: c_int = 0;
        check(unsafe { setsockopt(fd, c::SOL_SOCKET, c::SO_REUSEPORT, (&raw const off).cast(), 4) })?;
        let (mut value, mut length): (c_int, u32) = (1, 4);
        check(unsafe { getsockopt(fd, c::SOL_SOCKET, c::SO_REUSEPORT, (&raw mut value).cast(), &raw mut length) })?;
        if value == 0 && length == 4 { Ok(()) } else { Err(std::io::Error::other("the listening socket's port could be shared")) }
    }
    /// Tests only: turns `SO_REUSEPORT` on, and reads it.
    #[cfg(test)]
    pub fn share_port(fd: c_int) -> std::io::Result<bool> {
        let on: c_int = 1;
        check(unsafe { setsockopt(fd, c::SOL_SOCKET, c::SO_REUSEPORT, (&raw const on).cast(), 4) })?;
        port_shared(fd)
    }
    #[cfg(test)]
    pub fn port_shared(fd: c_int) -> std::io::Result<bool> {
        let (mut value, mut length): (c_int, u32) = (0, 4);
        check(unsafe { getsockopt(fd, c::SOL_SOCKET, c::SO_REUSEPORT, (&raw mut value).cast(), &raw mut length) })?;
        Ok(value != 0)
    }
    /// Whether a descriptor is open and a pipe (a FIFO), by `fstat`.
    pub fn is_pipe(number: u32) -> bool {
        use std::os::fd::BorrowedFd;
        let Ok(number) = c_int::try_from(number) else { return false };
        // Borrowed only to look: an invalid number makes `fstat` fail, which is the answer.
        let fd = unsafe { BorrowedFd::borrow_raw(number) };
        rustix::fs::fstat(fd).is_ok_and(|stat| rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Fifo)
    }
    /// The user this engine acts as.
    pub fn effective_user() -> u32 { unsafe { geteuid() } }
    pub fn open_descriptor(fd: c_int) -> bool { unsafe { fcntl(fd, 1) >= 0 } }
}

/// What is at `name` in `dir`, opened to read without following a link or waiting on a pipe.
fn open_entry(dir: &Dir, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
    use cap_std::fs::OpenOptionsExt;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    dir.open_with(name, &options).map(cap_std::fs::File::into_std)
}
/// The directory at an absolute path of plain names (`--no-links`), opened from `/` one name at
/// a time without following a link: what was checked is what is opened, and a directory swapped
/// for a link on the way, before or during the open, is refused.
fn open_without_links(path: &std::path::Path) -> std::io::Result<Dir> {
    use std::path::Component;
    if !path.is_absolute() { return Err(std::io::Error::other("not an absolute path")); }
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => dir = dir.open_dir_nofollow(name)?,
            _ => return Err(std::io::Error::other("not a plain absolute path")),
        }
    }
    Ok(dir)
}
/// The device a directory is on.
/// `--dir-id=DEVICE:INODE`, once: two decimal numbers and nothing else.
fn parse_dir_id(text: &str, already: Option<(u64, u64)>) -> wasmtime::Result<(u64, u64)> {
    let (Some((device, inode)), None) = (text.split_once(':'), already) else { return Err(reject("invalid grant")) };
    let decimal = |text: &str| if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) { text.parse::<u64>().ok() } else { None };
    match (decimal(device), decimal(inode)) { (Some(device), Some(inode)) => Ok((device, inode)), _ => Err(reject("invalid grant")) }
}
/// The one input, opened. Never the object a secret is read from (`secrets`: every
/// `--secret-object` and every secret descriptor's identity), whatever name reached it. With
/// `examined` (`--input-id=DEVICE:INODE`, what the launcher looked at), the open follows no
/// link at the name, and what was opened must be a regular file and that very file: one put
/// at the path between the launcher's look and this open (a link, the secret's file, any
/// other) is not the grant, and nothing runs. The check is of the open descriptor, so there
/// is no second look at the path.
fn open_input(path: &str, examined: Option<(u64, u64)>, secrets: &[(u64, u64)]) -> wasmtime::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let unreadable = |_| reject("input cannot be read");
    let file = if path == STANDARD_INPUT {
        use std::os::fd::AsFd;
        if examined.is_some() { return Err(reject("invalid grant: --input-id is for an --input file")); }
        std::io::stdin().as_fd().try_clone_to_owned().map(std::fs::File::from).map_err(unreadable)?
    } else if examined.is_some() {
        // No link followed at the name, and no waiting on a FIFO put there.
        std::fs::OpenOptions::new().read(true).custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32).open(path)
            .map_err(|_| reject("the granted input cannot be opened as the file the launcher examined (--input-id): a link or something unreadable is at its path; nothing runs"))?
    } else { std::fs::File::open(path).map_err(unreadable)? };
    let meta = file.metadata().map_err(unreadable)?;
    if let Some(examined) = examined {
        if !meta.is_file() || (meta.dev(), meta.ino()) != examined {
            return Err(reject("the granted input is not the file the launcher examined (--input-id): it was replaced since; nothing runs"));
        }
    }
    if secrets.contains(&(meta.dev(), meta.ino())) { return Err(reject("input is a secret slot's source")); }
    Ok(file)
}
/// A pinned module source has to be a regular file at the supplied name. In particular a FIFO
/// must not stall an unbounded service's startup. Test the opened descriptor before reading.
fn open_module(path: &std::path::Path, pinned: bool, secrets: &[(u64, u64)]) -> wasmtime::Result<std::fs::File> {
    if !pinned { return std::fs::File::open(path).map_err(|_| reject("module cannot be read")); }
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let unreadable = |_| reject("pinned module must be a readable regular file at its path");
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(flags.bits() as i32).open(path).map_err(unreadable)?;
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.is_file() { return Err(reject("pinned module must be a readable regular file at its path")); }
    if secrets.contains(&(metadata.dev(), metadata.ino())) { return Err(reject("module is a secret slot's source")); }
    Ok(file)
}
/// A launcher may unlink its staged module before starting the host. Adopt the
/// inherited regular file directly and check its entire contents, regardless of offset.
fn open_module_descriptor(number: u32, secrets: &[(u64, u64)]) -> wasmtime::Result<std::fs::File> {
    use std::io::Seek;
    use std::os::unix::fs::MetadataExt;
    let unreadable = |_| reject("pinned module descriptor must be an inherited readable regular file");
    let mut file = std::fs::File::from(sys::take_descriptor(number).map_err(unreadable)?);
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.is_file() { return Err(reject("pinned module descriptor must be an inherited readable regular file")); }
    if secrets.contains(&(metadata.dev(), metadata.ino())) { return Err(reject("module is a secret slot's source")); }
    file.rewind().map_err(unreadable)?;
    Ok(file)
}
/// The whole input, up to its grant: one byte more is a refusal of all of it, so the program
/// never runs on a prefix. From a file or the engine's standard input alike.
fn read_input(mut source: impl Read, granted: u64, named: Option<&str>) -> wasmtime::Result<Vec<u8>> {
    let mut data = Vec::new();
    (&mut source).take(granted + 1).read_to_end(&mut data).map_err(|_| reject("input cannot be read"))?;
    if data.len() as u64 > granted { return Err(reject(&input_over(granted, named, false))); }
    Ok(data)
}
/// `--input-stdin`: the engine's standard input, handed to the program as it asks for it, 64 KiB
/// at a time: a tool in a pipe works on input of any length in the memory it has, and answers
/// before its input ends. `left`: what remains of `--input-bytes`, a running budget (not
/// counted where it is `unlimited`). One byte past it ends the run with an error: unlike an
/// input file, which is read whole or refused whole before the program starts, the program
/// has by then run on what came before, and what it wrote has been passed on.
struct InputSource { file: std::fs::File, granted: u64, left: u64, taken: u64, ended: bool }
impl InputSource {
    const CHUNK: usize = 65_536;
    /// The next bytes into `buffer` (emptied first): whether there are any, or the input ended.
    fn refill(&mut self, buffer: &mut Vec<u8>) -> wasmtime::Result<bool> {
        buffer.clear();
        if self.ended { return Ok(false); }
        buffer.resize(Self::CHUNK, 0);
        let count = loop {
            match self.file.read(buffer) {
                Ok(count) => break count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => { buffer.clear(); return Err(reject("input cannot be read")); }
            }
        };
        buffer.truncate(count);
        if count == 0 { self.ended = true; return Ok(false); }
        if self.granted != u64::MAX {
            if count as u64 > self.left {
                buffer.clear();
                let (granted, named) = (self.granted, limit::named("--input-bytes"));
                limit::reached(limit::INPUT, || format!("input bytes: standard input holds more than the {granted} bytes granted ({}); the run ends", limit::name("--input-bytes")));
                return Err(reject(&input_over(granted, named.as_deref(), true)));
            }
            self.left -= count as u64;
        }
        self.taken = self.taken.saturating_add(count as u64);
        usage::input(self.taken);
        Ok(true)
    }
}
/// The next byte of the program's input: from what is held, then from the stream.
fn next_input(state: &mut State) -> wasmtime::Result<Option<u8>> {
    loop {
        if let Some(byte) = state.input.get(state.taken).copied() { state.taken += 1; return Ok(Some(byte)); }
        let Some(source) = &mut state.source else { return Ok(None) };
        state.taken = 0;
        if !source.refill(&mut state.input)? { return Ok(None); }
    }
}
/// What an input over its limit says: the operator's name for the limit when a launcher gave
/// one (`--limit-name`), the engine's flag otherwise.
fn input_over(granted: u64, named: Option<&str>, stream: bool) -> String {
    match (named, stream) {
        (Some(name), false) => format!("input exceeds its grant: more than {granted} bytes: raise {name} (at most 16777216)"),
        (None, false) => format!("input exceeds its grant: more than {granted} bytes (`--input-bytes=N` grants up to 16777216)"),
        (Some(name), true) => format!("input exceeds its grant: more than {granted} bytes on standard input: raise {name} (a number of bytes, or \"unlimited\")"),
        (None, true) => format!("input exceeds its grant: more than {granted} bytes on standard input (`--input-bytes=N`, or `--input-bytes=unlimited`)"),
    }
}
/// The directory opened is the one the launcher examined, by device and inode, or nothing runs:
/// another directory put at the path between its look and this open is not the grant.
fn held_to(root: &Dir, examined: (u64, u64)) -> wasmtime::Result<()> {
    use cap_std::fs::MetadataExt;
    let now = root.dir_metadata().map_err(|_| reject("granted directory cannot be examined"))?;
    if (now.dev(), now.ino()) != examined { return Err(reject("the granted directory is not the one the launcher examined (--dir-id): it was replaced since; nothing runs")); }
    Ok(())
}
fn device(dir: &Dir) -> std::io::Result<u64> { use cap_std::fs::MetadataExt; Ok(dir.dir_metadata()?.dev()) }
/// The directory at `name` in `dir`, opened without following a link, if it is on `device`
/// (the granted directory's). A device number is what is compared: a bind mount of another
/// directory of the same file system (Linux) has the same one, and is not told apart here. Checked on the handle opened, not the name, so there is no moment
/// between the check and the use: a file system mounted there since the launcher looked, or
/// while the run goes on, is refused like a link (cap-std's refusals carry no OS error either).
fn open_dir_on(dir: &Dir, name: &std::ffi::OsStr, device: u64) -> std::io::Result<Dir> {
    use cap_std::fs::MetadataExt;
    let opened = dir.open_dir_nofollow(name)?;
    if opened.dir_metadata()?.dev() != device { return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the path crosses onto another device")); }
    Ok(opened)
}
/// A device and inode as std's `MetadataExt` gives them for an open file (a negative macOS
/// `dev_t` sign-extended), which is how the secrets' objects are recorded.
fn identity(meta: &cap_std::fs::Metadata) -> (u64, u64) { use cap_std::fs::MetadataExt; (meta.dev(), meta.ino()) }
/// What a symbolic link at `name` in `dir` resolves to, followed by the system wherever it
/// points, as a launcher reading through it would see it. Used only to refuse (`Files::guard`).
fn linked_identity(dir: &Dir, name: &std::ffi::OsStr) -> Option<(u64, u64)> {
    let stat = rustix::fs::statat(dir, name, rustix::fs::AtFlags::empty()).ok()?;
    #[allow(clippy::unnecessary_cast)]
    Some((stat.st_dev as u64, stat.st_ino as u64))
}

/// A `--listen=` value: the host to bind and its port, as written. The host is an IPv4 address
/// or a name with no `:` in it, or an IPv6 literal, in brackets (`[::1]:8080`) or without
/// (`::1:8080`, as `wasm-grants` writes it: brackets are glob characters to a shell). The port
/// is what follows the last `:`, digits only. The host comes back as an authority spells it:
/// an IPv6 literal in brackets, in its canonical text.
fn listen_address(text: &str) -> Option<(String, &str)> {
    let (host, port) = text.rsplit_once(':')?;
    if text.len() > 256 || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u16>().is_err() { return None; }
    let literal = |text: &str| text.parse::<std::net::Ipv6Addr>().ok().map(|address| format!("[{address}]"));
    let host = match host.strip_prefix('[').and_then(|inner| inner.strip_suffix(']')) {
        Some(inner) => literal(inner)?,
        None if host.contains(':') => literal(host)?,
        None if host.is_empty() || host.contains(['[', ']']) => return None,
        None => host.to_owned(),
    };
    Some((host, port))
}
/// The granted listening address, bound, with the authority a request to it names in its `Host`
/// header: the granted host and the port actually bound. No other socket can bind the same
/// address and port while this one is open (`refuse_port_sharing`; `SO_REUSEADDR`, which std
/// sets so that a restarted server is not kept out by connections still closing, lets no
/// second socket bind the same address and port on macOS or Linux; a wildcard bind of the
/// port, which macOS allows, receives nothing sent to this address while this socket listens).
fn bind_granted(host: &str, port: &str) -> std::io::Result<(std::net::TcpListener, String)> {
    use std::os::fd::AsRawFd;
    let bound = std::net::TcpListener::bind(format!("{host}:{port}"))?;
    sys::refuse_port_sharing(bound.as_raw_fd())?;
    bound.set_nonblocking(true)?;
    let authority = format!("{host}:{}", bound.local_addr()?.port());
    Ok((bound, authority))
}

impl Files {
    /// `--data-only`: the granted directory holds data and nothing that decides authority, and
    /// the rule says which names a program may use, not which it may not (docs/wasm-files.md,
    /// "Data names"). 0: off. 1: a directory the run only reads. 2: one it may write.
    ///
    /// Every component of every path is a *data name*: 1 to 255 bytes, each an ASCII letter, a
    /// digit, `-`, `_` or `.`, the first not `.`. So nothing hidden is read, entered or made, and
    /// no name has a character a file system may fold onto another (APFS resolves `witchy.loc`
    /// + KELVIN SIGN as `witchy.lock`: review of 2026-11-22). Where the run may write, a name a
    /// build reads as a manifest, lock, policy, package list, grant document or source, compared
    /// without ASCII case, is no data name either: it is not made, replaced, moved to or from,
    /// removed, read, entered or listed.
    fn data_only() -> &'static std::sync::atomic::AtomicU8 {
        static DATA_ONLY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        &DATA_ONLY
    }
    /// The positive set: what a name under a data directory is made of.
    fn plain(name: &str) -> bool {
        !name.is_empty() && name.len() <= 255 && !name.starts_with('.') && !name.ends_with('.') && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }
    /// A name a build reads, without ASCII case (the only case a plain name has).
    fn protected(name: &str) -> bool {
        // Trailing dots are no part of a name to some filesystems (`a.wy.` opens `a.wy`): a
        // plain name has none, and the question is asked of the name without them anyway.
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        name == "witchy.toml" || name == "witchy.lock" || [".wy", ".policy", ".packages", ".grants.toml"].iter().any(|end| name.ends_with(end))
    }
    /// Whether a path may have this component, in the mode the run is in.
    fn named(name: &str) -> bool {
        match Self::data_only().load(std::sync::atomic::Ordering::SeqCst) { 0 => true, 1 => Self::plain(name), _ => Self::plain(name) && !Self::protected(name) }
    }
    /// A name nothing is written at under `--data-only`, whichever way the entry was reached.
    fn guarded(name: &std::ffi::OsStr) -> bool {
        Self::data_only().load(std::sync::atomic::Ordering::SeqCst) != 0 && !name.to_str().is_some_and(|name| Self::plain(name) && !Self::protected(name))
    }
    /// What making or moving a name costs of the byte budget, whatever it holds: a directory
    /// entry is not free because it is empty.
    const ENTRY_COST: u64 = 256;
    /// Before a name is made (`new`: a file that was not there, a directory) or moved to: the
    /// fixed cost against the byte budget, and for a new name the count of names this run may
    /// make. Charged before the attempt and never given back: removing a name frees nothing.
    fn place(&mut self, new: bool, name: &std::ffi::OsStr) -> Result<(), i64> {
        if Self::guarded(name) { return Err(INVALID_PATH); }
        if new && self.made >= self.entry_limit {
            let granted = self.entry_limit;
            match self.entry_share {
                Share::Connection => limit::reached(limit::ENTRIES_EACH, || format!("file entries: a connection's instance has made its {granted} files and directories ({}); that instance makes no more, and the server goes on", limit::name("--fs-entries-per-connection"))),
                share => { let granted = share.of_run(granted); limit::reached(limit::ENTRIES, || format!("file entries: the run has made its {granted} granted files and directories ({}); no more are made", limit::name("--fs-entries"))) }
            }
            return Err(TOO_LARGE);
        }
        if Self::ENTRY_COST > self.budget.saturating_sub(self.used) { return Err(Self::full(self.budget, self.share)); }
        self.used += Self::ENTRY_COST;
        if new { self.made += 1; }
        Ok(())
    }
    /// The byte budget is used up: said once, and the code the program gets.
    fn full(budget: u64, share: Share) -> i64 {
        match share {
            Share::Connection => limit::reached(limit::FILES_EACH, || format!("file bytes: a connection's instance has read and written its {budget} bytes ({}); that instance reads and writes nothing more, and the server goes on", limit::name("--fs-bytes-per-connection"))),
            _ => { let budget = share.of_run(budget); limit::reached(limit::FILES, || format!("file bytes: the run has read and written its {budget} granted bytes ({}); nothing more is read or written", limit::name("--fs-bytes"))) }
        }
        TOO_LARGE
    }
    fn entry(&mut self, handle: i64) -> wasmtime::Result<&mut Entry> {
        let index = handle.checked_sub(FIRST_HANDLE).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| reject("invalid file capability"))?;
        self.entries.get_mut(index).ok_or_else(|| reject("invalid file capability"))
    }
    /// A new handle, reusing a closed one's number (as network handles do): a server that opens
    /// and closes a file per request needs no more than it holds at once (review: it trapped
    /// after 4096 opens). A number is reused only after its handle was given back, which source
    /// code cannot then use (it was moved).
    fn issue(&mut self, entry: Entry) -> wasmtime::Result<i64> {
        if let Some(free) = self.entries.iter().position(|e| matches!(e, Entry::Closed)) {
            let handle = FIRST_HANDLE + free as i64;
            if self.alias_root == Some(handle) { self.alias_root = None; }
            self.entries[free] = entry;
            return Ok(handle);
        }
        if self.entries.len() >= MAX_HANDLES { return Err(reject("file handle limit exceeded")); }
        self.entries.push(entry);
        Ok(FIRST_HANDLE + self.entries.len() as i64 - 1)
    }
    /// Why an operation on `name` in `dir` failed, as the library reads it.
    fn code(error: &std::io::Error, dir: &Dir, name: &std::ffi::OsStr) -> i64 {
        use std::io::ErrorKind;
        let link = || dir.symlink_metadata(name).is_ok_and(|meta| meta.file_type().is_symlink());
        match error.kind() {
            ErrorKind::NotFound => NOT_FOUND,
            // A link where a directory or file was wanted: never followed.
            _ if error.raw_os_error() == Some(sys::ELOOP) => INVALID_PATH,
            ErrorKind::NotADirectory if link() => INVALID_PATH,
            ErrorKind::NotADirectory => WRONG_KIND,
            ErrorKind::AlreadyExists => EXISTS,
            // cap-std's own refusal of a path that would leave the directory (it carries no OS
            // error). Plain names never get there; if one did, it is a path, refused.
            ErrorKind::PermissionDenied if error.raw_os_error().is_none() => INVALID_PATH,
            _ => IO,
        }
    }
    /// The directory holding the last component of a path below `base`, and that component.
    /// The path is the library's: plain names joined by `/`, so `..`, `.`, empty and absolute
    /// paths are refused as written, whether or not they would stay inside. Each directory on
    /// the way is opened from the one before it by cap-std without following a link, so no link
    /// is taken anywhere, inside the grant or out, and cap-std keeps every step below `base`.
    fn resolve(base: &Dir, pending: &[u8]) -> Result<(Dir, std::ffi::OsString), i64> {
        Self::resolve_on(base, device(base).map_err(|_| IO)?, pending)
    }
    /// `resolve`, each directory on the way held to `device`, the granted directory's: a file
    /// system mounted inside the grant, before the run or during it, is never stepped onto.
    fn resolve_on(base: &Dir, device: u64, pending: &[u8]) -> Result<(Dir, std::ffi::OsString), i64> {
        if pending.is_empty() || pending.len() > MAX_PATH { return Err(INVALID_PATH); }
        let text = std::str::from_utf8(pending).map_err(|_| INVALID_PATH)?;
        let parts: Vec<&str> = text.split('/').collect();
        if parts.iter().any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains(['\\', '\0']) || !Self::named(part)) { return Err(INVALID_PATH); }
        let (last, steps) = parts.split_last().ok_or(INVALID_PATH)?;
        let mut dir = base.try_clone().map_err(|_| IO)?;
        for step in steps {
            dir = open_dir_on(&dir, step.as_ref(), device).map_err(|e| Self::code(&e, &dir, step.as_ref()))?;
        }
        Ok((dir, (*last).into()))
    }
    /// The path a directory was given, resolved, if the directory has the rights asked for.
    fn take_path(&mut self, handle: i64, (read, write): (bool, bool)) -> wasmtime::Result<Result<(Dir, std::ffi::OsString), i64>> {
        let Entry::Dir { dir, read: can_read, write: can_write, pending, .. } = self.entry(handle)? else { return Err(reject("invalid directory capability")) };
        if (read && !*can_read) || (write && !*can_write) { return Err(reject("the directory does not have the right this asks for")); }
        let taken = std::mem::take(pending);
        Ok(Self::resolve(dir, &taken))
    }
    /// The rights an open asks for, as the compiler encodes them from the receiver's type.
    fn rights(code: i64) -> wasmtime::Result<(bool, bool)> {
        match code { 0 => Ok((true, false)), 1 => Ok((false, true)), 2 => Ok((true, true)), _ => Err(reject("invalid directory right")) }
    }
    /// What is at a name: 1 a file, 2 a directory, 3 something else, 0 nothing; a link is
    /// never followed (`INVALID_PATH`).
    fn kind(parent: &Dir, name: &std::ffi::OsStr) -> Result<u8, i64> {
        use std::os::unix::fs::MetadataExt;
        let Ok(within) = device(parent) else { return Err(IO) };
        match open_entry(parent, name) {
            Ok(file) => match file.metadata() { Ok(meta) if meta.dev() != within => Err(INVALID_PATH), Ok(meta) if meta.is_file() => Ok(1), Ok(meta) if meta.is_dir() => Ok(2), Ok(_) => Ok(3), Err(_) => Err(IO) },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(Self::code(&e, parent, name)),
        }
    }
    /// Before a write, remove or rename changes the entry at `name`: it is not a secret's source
    /// (by device and inode, without following a link, through the directory's own `Dir`), and
    /// a directory holds none anywhere below it. A match stops the run: the program would
    /// replace, move or delete a secret's file, and the next run would read a key the program
    /// chose (one source, one route: a source changes by no grant's route). A directory that
    /// cannot be looked through whole is a failure code, not a move. Nothing is checked when the
    /// run has no secret source to compare.
    fn guard(&self, parent: &Dir, name: &std::ffi::OsStr) -> wasmtime::Result<Result<(), i64>> {
        if self.secret_objects.is_empty() { return Ok(Ok(())); }
        let touched = || reject("the program writes, removes or renames a secret slot's source");
        // One entry: nothing (`None`), a directory or not, or a failure. A secret's source stops
        // the run. A symbolic link is never followed by a grant, but a launcher may read a key
        // through one (review of 499f8240: renaming a file onto `cur -> key` swapped the key the
        // next run read). So a link that resolves to a source stops the run, and any other link
        // is not changed while the run has secrets: what is behind a link to a directory is not
        // looked through (fail closed).
        let entry = |dir: &Dir, name: &std::ffi::OsStr| -> wasmtime::Result<Result<Option<bool>, i64>> {
            let meta = match dir.symlink_metadata(name) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(None)),
                Err(e) => return Ok(Err(Self::code(&e, dir, name))),
            };
            if self.secret_objects.contains(&identity(&meta)) { return Err(touched()); }
            if meta.file_type().is_symlink() {
                if linked_identity(dir, name).is_some_and(|object| self.secret_objects.contains(&object)) { return Err(touched()); }
                return Ok(Err(INVALID_PATH));
            }
            Ok(Ok(Some(meta.is_dir())))
        };
        match entry(parent, name)? {
            Ok(Some(true)) => {}
            Ok(_) => return Ok(Ok(())),
            Err(code) => return Ok(Err(code)),
        }
        // Everything below, without following a link, up to a bound: a directory too large to
        // look through, or one that cannot be listed whole, is not moved (fail closed).
        let Ok(top) = parent.open_dir_nofollow(name) else { return Ok(Err(IO)) };
        let (mut pending, mut seen) = (vec![top], 0usize);
        while let Some(dir) = pending.pop() {
            let Ok(children) = dir.entries() else { return Ok(Err(IO)) };
            for child in children {
                let Ok(child) = child else { return Ok(Err(IO)) };
                let child = child.file_name();
                seen += 1;
                if seen > MAX_GUARDED { return Ok(Err(IO)); }
                match entry(&dir, &child)? {
                    Ok(Some(true)) => match dir.open_dir_nofollow(&child) { Ok(below) => pending.push(below), Err(_) => return Ok(Err(IO)) },
                    Ok(_) => {}
                    Err(code) => return Ok(Err(code)),
                }
            }
        }
        Ok(Ok(()))
    }
    fn open_file(&mut self, handle: i64, mode: i64) -> wasmtime::Result<i64> {
        // A file has one right: read (0) or write (1).
        let (_, write) = Self::rights(mode)?;
        if mode == 2 { return Err(reject("invalid directory right")); }
        let resolved = self.take_path(handle, Self::rights(mode)?)?;
        let entry = match resolved {
            Err(code) => Entry::Failed(code),
            Ok((parent, name)) if write => match Self::kind(&parent, &name) {
                // Refused now if it is a secret's source, and again when it is replaced (`close`).
                Ok(0 | 1) => match self.guard(&parent, &name)? {
                    Ok(()) => Entry::Writing { parent, name, data: Vec::new(), exclusive: false, append: false },
                    Err(code) => Entry::Failed(code),
                },
                Ok(_) => Entry::Failed(WRONG_KIND),
                Err(code) => Entry::Failed(code),
            },
            Ok((parent, name)) => match self.read_whole(&parent, &name)? { Ok(data) => Entry::Reading { data, at: 0 }, Err(code) => Entry::Failed(code) },
        };
        self.issue(entry)
    }
    /// A file's whole contents, or a failure code. Every read of a file's contents through a
    /// grant comes here: the file opened is checked (`fstat` of the handle cap-std opened), not
    /// the name, so a secret's source file is refused through any path or hard link in the
    /// grant (and a rename meanwhile changes nothing). The run stops: the program asked for a
    /// secret by another route.
    fn read_whole(&mut self, parent: &Dir, name: &std::ffi::OsStr) -> wasmtime::Result<Result<Vec<u8>, i64>> {
        let Ok(within) = device(parent) else { return Ok(Err(IO)) };
        self.read_on(parent, name, Some(within))
    }
    /// `read_whole`, the file opened held to `within`, its directory's device: a file on another
    /// file system (one mounted over the name) is refused as a path that leaves the grant, by
    /// what was opened, so a mount made at any time is caught. `None` only for a granted file
    /// (`--file`), which is the grant itself wherever it is.
    fn read_on(&mut self, parent: &Dir, name: &std::ffi::OsStr, within: Option<u64>) -> wasmtime::Result<Result<Vec<u8>, i64>> {
        use std::os::unix::fs::MetadataExt;
        let file = match open_entry(parent, name) { Ok(file) => file, Err(e) => return Ok(Err(Self::code(&e, parent, name))) };
        let Ok(meta) = file.metadata() else { return Ok(Err(IO)) };
        if within.is_some_and(|device| meta.dev() != device) { return Ok(Err(INVALID_PATH)); }
        if self.secret_objects.contains(&(meta.dev(), meta.ino())) { return Err(reject("the program reads a secret slot's source as a file")); }
        if !meta.is_file() { return Ok(Err(WRONG_KIND)); }
        let room = self.budget - self.used;
        let mut data = Vec::new();
        if file.take(room + 1).read_to_end(&mut data).is_err() { return Ok(Err(IO)); }
        if data.len() as u64 > room { return Ok(Err(Self::full(self.budget, self.share))); }
        self.used += data.len() as u64;
        Ok(Ok(data))
    }
    fn open_dir(&mut self, handle: i64, mode: i64) -> wasmtime::Result<i64> {
        // The subtree gets the rights asked for, which the directory must have.
        let (read, write) = Self::rights(mode)?;
        if self.alias_root == Some(handle) && read && !write {
            let Entry::Dir { read: true, pending, .. } = self.entry(handle)?
                else { return Err(reject("invalid readable directory capability")); };
            let path = std::mem::take(pending);
            if let Some((_, directory)) = self.read_aliases.iter().find(|(name, _)| *name == path) {
                let dir = directory.try_clone()?;
                return self.issue(Entry::Dir { dir, read: true, write: false, pending: Vec::new(), from: Vec::new() });
            }
            let Entry::Dir { pending, .. } = self.entry(handle)?
                else { return Err(reject("invalid directory capability")); };
            *pending = path;
        }
        let resolved = self.take_path(handle, (read, write))?;
        let entry = match resolved {
            Err(code) => Entry::Failed(code),
            Ok((parent, name)) => match device(&parent).and_then(|within| open_dir_on(&parent, &name, within)) {
                Ok(dir) => Entry::Dir { dir, read, write, pending: Vec::new(), from: Vec::new() },
                Err(e) => Entry::Failed(Self::code(&e, &parent, &name)),
            },
        };
        self.issue(entry)
    }
    fn make_dir(&mut self, handle: i64) -> wasmtime::Result<i64> {
        use cap_std::fs::DirBuilderExt;
        let resolved = self.take_path(handle, (false, true))?;
        Ok(match resolved {
            Err(code) => code,
            Ok((parent, name)) => match self.place(true, &name) {
                Err(code) => code,
                Ok(()) => match parent.create_dir_with(&name, cap_std::fs::DirBuilder::new().mode(0o755)) { Ok(()) => 0, Err(e) => Self::code(&e, &parent, &name) },
            },
        })
    }
    fn close(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let entry = std::mem::replace(self.entry(handle)?, Entry::Closed);
        Ok(match entry {
            Entry::Closed => return Err(reject("invalid file capability")),
            // What is at the name now, not when it was opened: a secret's file moved there since
            // (by another grant's rename) is never replaced.
            Entry::Writing { parent, name, data, append: true, .. } => self.append(&parent, &name, &data)?,
            Entry::Writing { parent, name, data, exclusive, .. } => match self.guard(&parent, &name)? {
                // A name that is not there yet is one more entry; replacing one is not.
                Ok(()) => match Self::kind(&parent, &name) {
                    Err(code) => code,
                    Ok(kind) => match self.place(kind == 0, &name) {
                        Err(code) => code,
                        Ok(()) => match self.staging_for(&parent) {
                            Err(code) => code,
                            Ok(staging) => Self::commit(&parent, &name, &data, exclusive, Self::ordinal(), staging),
                        },
                    },
                },
                Err(code) => code,
            },
            _ => 0,
        })
    }
    /// The engine's own directory in a data directory, for the files it writes before it moves
    /// them into place. Reserved: no program can name it (it begins with `.`), and a launcher's
    /// scan knows exactly this name.
    const STAGING: &str = ".witchy-staging";
    /// The `N` of the next temporary's name (`PID-N`): one count for the process, not for a
    /// `Files`, so that no two instances of one run (`--instance-per-connection`), each with a
    /// `Files` of its own, ever stage two files under one name.
    fn ordinal() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
    /// A temporary in the staging directory: `PID-N`, digits only.
    fn staged_name(name: &str) -> bool {
        let digits = |text: &str| !text.is_empty() && text.len() <= 20 && text.bytes().all(|byte| byte.is_ascii_digit());
        name.split_once('-').is_some_and(|(process, ordinal)| digits(process) && digits(ordinal))
    }
    /// The staging directory of a data directory, opened and checked; made if `make` and it is
    /// not there; `None` if it is not there. It is what the engine made or it is refused, with
    /// the reason: a real directory (opened without following a link), on the data directory's
    /// device (so a rename out of it is atomic), owned by this user, writable by no one else,
    /// holding only regular files with one name each, named as the engine names its
    /// temporaries. Those are what a run that was killed between writing and renaming left, and
    /// they are removed here, if no other engine is at work in the directory: each engine holds
    /// a shared lock on the staging directory while it runs, and only one that gets the
    /// exclusive lock clears it (a lock ends with its process, however that ends).
    fn staging_in(root: &Dir, make: bool) -> Result<Option<Dir>, String> {
        use cap_std::fs::{DirBuilderExt, MetadataExt};
        let name: &std::ffi::OsStr = Self::STAGING.as_ref();
        let refused = |why: &str| format!("the granted directory's {} {why}: the engine keeps its unfinished writes there and made it otherwise; nothing runs until it is removed", Self::STAGING);
        match root.symlink_metadata(name) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !make { return Ok(None); }
                match root.create_dir_with(name, cap_std::fs::DirBuilder::new().mode(0o700)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(refused("cannot be made")),
                }
            }
            Err(_) => return Err(refused("cannot be examined")),
        }
        let within = device(root).map_err(|_| refused("cannot be examined"))?;
        let staging = open_dir_on(root, name, within).map_err(|_| refused("is not a directory on the granted directory's device"))?;
        // cap-std may use O_PATH on Linux. Open the same directory for reading so that
        // flock can hold a lock, without resolving the staging name a second time.
        use rustix::fs::{Mode, OFlags, openat};
        let lockable = openat(&staging, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(|_| refused("cannot be opened for locking"))?;
        let staging = Dir::from_std_file(lockable.into());
        let meta = staging.dir_metadata().map_err(|_| refused("cannot be examined"))?;
        if !meta.is_dir() || meta.uid() != sys::effective_user() || meta.mode() & 0o077 != 0 { return Err(refused("is not a directory of this user's alone (owned by the user the engine runs as, mode 0700)")); }
        let alone = rustix::fs::flock(&staging, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
        if alone {
            let held = staging.entries().and_then(|entries| entries.map(|entry| entry.map(|entry| entry.file_name())).collect::<std::io::Result<Vec<_>>>()).map_err(|_| refused("cannot be listed"))?;
            for stale in held {
                let made = stale.to_str().is_some_and(Self::staged_name) && staging.symlink_metadata(&stale).is_ok_and(|meta| meta.is_file() && meta.nlink() == 1);
                if !made { return Err(refused("holds an entry the engine did not make")); }
                staging.remove_file(&stale).map_err(|_| refused("holds a stale file that cannot be removed"))?;
            }
        }
        // Shared from here on (from exclusive, or beside the engines that hold it shared; one
        // that is clearing is waited for, which is a few removals).
        rustix::fs::flock(&staging, rustix::fs::FlockOperation::LockShared).map_err(|_| refused("cannot be locked"))?;
        Ok(Some(staging))
    }
    /// Where a file written to `parent` is staged: the data directory's staging directory if
    /// the run has one and `parent` is on its device, made at the first write.
    fn staging_for(&mut self, parent: &Dir) -> Result<Option<Dir>, i64> {
        let Some(root) = &self.data_root else { return Ok(None) };
        if self.staging.is_none() { self.staging = Self::staging_in(root, true).map_err(|_| IO)?; }
        let Some(staging) = &self.staging else { return Err(IO) };
        if device(parent).ok() != device(staging).ok() { return Ok(None); }
        staging.try_clone().map(Some).map_err(|_| IO)
    }
    /// Write and sync a file that did not exist, to be moved to `name` in `parent`: in the
    /// staging directory (`PID-N`), or with no staging directory a sibling (`.NAME.witchy-PID-N`).
    /// Returns the directory it is in and its name there.
    fn stage(parent: &Dir, name: &std::ffi::OsStr, data: &[u8], ordinal: u64, staging: Option<Dir>) -> Result<(Dir, std::ffi::OsString), i64> {
        use cap_std::fs::OpenOptionsExt;
        let (held, temporary) = match staging {
            Some(staging) => (staging, std::ffi::OsString::from(format!("{}-{ordinal}", std::process::id()))),
            None => {
                let mut temporary = std::ffi::OsString::from(".");
                temporary.push(name);
                temporary.push(format!(".witchy-{}-{ordinal}", std::process::id()));
                (parent.try_clone().map_err(|_| IO)?, temporary)
            }
        };
        // Only a temporary this created is ever removed: one already at that name (the name is
        // predictable) is someone else's, perhaps a link to a secret's file.
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true).follow(FollowSymlinks::No).mode(0o644);
        let Ok(file) = held.open_with(&temporary, &options) else { return Err(IO) };
        let mut file = file.into_std();
        if file.write_all(data).and_then(|()| file.sync_all()).is_err() { let _ = held.remove_file(&temporary); return Err(IO); }
        Ok((held, temporary))
    }
    /// Write a file that did not exist, then rename it over the name: a reader sees the old
    /// contents or the new, never part of them. The rename is within one device (`staging_for`).
    fn commit(parent: &Dir, name: &std::ffi::OsStr, data: &[u8], exclusive: bool, ordinal: u64, staging: Option<Dir>) -> i64 {
        let (held, temporary) = match Self::stage(parent, name, data, ordinal, staging) { Ok(staged) => staged, Err(code) => return code };
        let placed = if exclusive {
            // Moved only to where nothing is, in one step: of two creators, one wins, and the
            // file never has two names (a launcher's scan, or another engine's append, that
            // looked between a hard link and the removal of the temporary saw a file with two
            // and refused it: review of 2026-11-22). Where the filesystem has no such rename,
            // a hard link, which is also made only where nothing is, then the removal.
            match rustix::fs::renameat_with(&held, &temporary, parent, name, rustix::fs::RenameFlags::NOREPLACE) {
                Ok(()) => Ok(()),
                Err(error) if error == rustix::io::Errno::EXIST => { let _ = held.remove_file(&temporary); Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists)) }
                Err(error) if matches!(error, rustix::io::Errno::NOTSUP | rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
                    let linked = held.hard_link(&temporary, parent, name);
                    let _ = held.remove_file(&temporary);
                    linked
                }
                Err(error) => { let _ = held.remove_file(&temporary); Err(std::io::Error::from(error)) }
            }
        } else {
            let renamed = held.rename(&temporary, parent, name);
            if renamed.is_err() { let _ = held.remove_file(&temporary); }
            renamed
        };
        match placed { Ok(()) => 0, Err(e) => Self::code(&e, parent, name) }
    }
    fn probe(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let resolved = self.take_path(handle, (true, false))?;
        Ok(match resolved.map(|(parent, name)| Self::kind(&parent, &name)) { Ok(Ok(1)) => 1, Ok(Ok(2)) => 2, _ => 0 })
    }
    fn open_list(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let Entry::Dir { dir, pending, read: true, .. } = self.entry(handle)? else { return Err(reject("invalid directory capability")) };
        pending.clear();
        // The names in the directory, and whether each is a directory (a link is not one), at
        // most 65,537 of them; one that cannot be read fails the whole listing.
        let listed: std::io::Result<Vec<(std::ffi::OsString, bool)>> = dir.entries().and_then(|entries| entries.take(65_537)
            .map(|entry| entry.and_then(|entry| Ok((entry.file_name(), entry.file_type()?.is_dir())))).collect());
        let entry = match listed {
            Err(_) => Entry::Failed(IO),
            Ok(found) => {
                let mut names: Vec<String> = found.into_iter().filter_map(|(name, directory)| {
                    // Under `--data-only`, only what the program could name: nothing hidden, nothing
                    // outside the data names, not the engine's own staging directory.
                    let name = name.into_string().ok().filter(|n| !n.contains('\n') && Self::named(n))?;
                    Some(if directory { format!("{name}/") } else { name })
                }).collect();
                names.sort();
                let data: Vec<u8> = names.iter().flat_map(|n| n.bytes().chain(std::iter::once(b'\n'))).collect();
                if data.len() as u64 > self.budget - self.used { Entry::Failed(Self::full(self.budget, self.share)) } else { self.used += data.len() as u64; Entry::Reading { data, at: 0 } }
            }
        };
        self.issue(entry)
    }
    fn remove(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let resolved = self.take_path(handle, (false, true))?;
        Ok(match resolved {
            Err(code) => code,
            Ok((parent, name)) => match Self::kind(&parent, &name) {
                Ok(1) => match self.guard(&parent, &name)? {
                    Ok(()) => match parent.remove_file(&name) { Ok(()) => 0, Err(e) => Self::code(&e, &parent, &name) },
                    Err(code) => code,
                },
                Ok(0) => NOT_FOUND,
                Ok(_) => WRONG_KIND,
                Err(code) => code,
            },
        })
    }
    /// Keep the path built so far for the next `rename`.
    fn rename_from(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let Entry::Dir { pending, from, write: true, .. } = self.entry(handle)? else { return Err(reject("invalid directory capability")) };
        *from = std::mem::take(pending);
        Ok(0)
    }
    /// Move what is at the kept path to the path built since, both resolved below this
    /// directory a name at a time. A link is never followed or moved.
    fn rename(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let Entry::Dir { from, .. } = self.entry(handle)? else { return Err(reject("invalid directory capability")) };
        let kept = std::mem::take(from);
        let target = self.take_path(handle, (false, true))?;
        let Entry::Dir { dir, .. } = self.entry(handle)? else { return Err(reject("invalid directory capability")) };
        let source = Self::resolve(dir, &kept);
        Ok(match (source, target) {
            (Err(code), _) | (_, Err(code)) => code,
            // Neither what moves (with all it holds) nor what it replaces is a secret's source.
            (Ok((from_dir, from_name)), Ok((to_dir, to_name))) => match Self::kind(&from_dir, &from_name) {
                Ok(1 | 2) => match (self.guard(&from_dir, &from_name)?, self.guard(&to_dir, &to_name)?) {
                    (Ok(()), Ok(())) => match self.place(false, &to_name) {
                        Err(code) => code,
                        Ok(()) => match from_dir.rename(&from_name, &to_dir, &to_name) { Ok(()) => 0, Err(e) => Self::code(&e, &to_dir, &to_name) },
                    },
                    (Err(code), _) | (_, Err(code)) => code,
                },
                Ok(0) => NOT_FOUND,
                Ok(_) => WRONG_KIND,
                Err(code) => code,
            },
        })
    }
    /// Add `data` at the end of the file at `name` (made with them if nothing is there),
    /// in one `write` on a descriptor opened with `O_APPEND`, then sync it: the bytes are after
    /// whatever the file held at that moment, and on disk before the program is told so. The
    /// file is opened from its directory without following a link and checked as opened: a
    /// regular file, on the directory's device, with one name (the engine never writes through
    /// a hard link: a file with two names is `InvalidPath`, as a link is), and not a secret's
    /// source. Only the bytes added were charged, as they were written to the handle.
    ///
    /// Not atomic, unlike a replacement: a run killed, or a disk that fills, in the middle of
    /// the `write` leaves a prefix of `data` at the end of the file. A reader of an appended
    /// file must take a torn last record (docs/wasm-files.md).
    fn append(&mut self, parent: &Dir, name: &std::ffi::OsStr, data: &[u8]) -> wasmtime::Result<i64> {
        use cap_std::fs::OpenOptionsExt;
        use std::os::unix::fs::MetadataExt;
        if Self::guarded(name) { return Ok(INVALID_PATH); }
        if let Err(code) = self.guard(parent, name)? { return Ok(code); }
        let Ok(within) = device(parent) else { return Ok(IO) };
        let mut options = cap_std::fs::OpenOptions::new();
        options.append(true).follow(FollowSymlinks::No).custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
        // Nothing there (when the handle was opened, or now): the file is made whole with these
        // bytes, but only where nothing is. If another writer made it meanwhile, these bytes are
        // added to theirs like any append: an append never replaces a file (review of
        // 2026-11-22: two handles opened while the file was absent each replaced it).
        let mut tries = 0;
        let mut file = loop {
            match parent.open_with(name, &options) {
                Ok(file) => break file.into_std(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && tries < 8 => {
                    tries += 1;
                    if tries == 1 { if let Err(code) = self.place(true, name) { return Ok(code); } }
                    let staging = match self.staging_for(parent) { Ok(staging) => staging, Err(code) => return Ok(code) };
                    match Self::commit(parent, name, data, true, Self::ordinal(), staging) { EXISTS => continue, code => return Ok(code) }
                }
                Err(e) => return Ok(Self::code(&e, parent, name)),
            }
        };
        let Ok(meta) = file.metadata() else { return Ok(IO) };
        if self.secret_objects.contains(&(meta.dev(), meta.ino())) { return Err(reject("the program writes, removes or renames a secret slot's source")); }
        if !meta.is_file() { return Ok(WRONG_KIND); }
        if meta.dev() != within || meta.nlink() != 1 { return Ok(INVALID_PATH); }
        if data.is_empty() { return Ok(0); }
        Ok(if file.write_all(data).and_then(|()| file.sync_all()).is_ok() { 0 } else { IO })
    }
    /// A file to write: added to if it is there (`append`), or made only if nothing is (`exclusive`).
    fn open_writing(&mut self, handle: i64, append: bool) -> wasmtime::Result<i64> {
        let resolved = self.take_path(handle, (false, true))?;
        let entry = match resolved {
            Err(code) => Entry::Failed(code),
            Ok((parent, name)) => match Self::kind(&parent, &name) {
                // Nothing there: `create_new` makes it only if still nothing is at `finish`; an
                // append makes it then, or adds to what another writer made meanwhile.
                Ok(0) => Entry::Writing { parent, name, data: Vec::new(), exclusive: !append, append },
                // A file that is there is added to at `finish`; nothing of it is read or charged.
                Ok(1) if append => match self.guard(&parent, &name)? {
                    Ok(()) => Entry::Writing { parent, name, data: Vec::new(), exclusive: false, append: true },
                    Err(code) => Entry::Failed(code),
                },
                Ok(1) => Entry::Failed(EXISTS),
                Ok(_) => Entry::Failed(WRONG_KIND),
                Err(code) => Entry::Failed(code),
            },
        };
        self.issue(entry)
    }
}
/// A socket in non-blocking mode while this lives. `MSG_DONTWAIT` alone did not stop a send on
/// macOS from waiting out the socket's 2 s write timeout once the peer stopped reading (review:
/// a task waiting to write held every other task, its time limit included); the sockets stay
/// blocking otherwise, for the operations that do wait.
struct Unblocked<'a>(&'a std::net::TcpStream);
impl<'a> Unblocked<'a> {
    fn new(stream: &'a std::net::TcpStream) -> Self { let _ = stream.set_nonblocking(true); Self(stream) }
}
impl Drop for Unblocked<'_> { fn drop(&mut self) { let _ = self.0.set_nonblocking(false); } }
/// The endpoints a run may dial and the connections it has made. A `Net` carries a set of
/// `host:port` strings, exactly as the run granted them; `only` keeps one of them, and a
/// connection is made only to an address in the set. Bytes cross one per call.
#[derive(Default)]
struct Network { allowed: Vec<String>, budget: u64, used: u64, entries: Vec<NetEntry>,
    /// Whose the byte budget is, for the line said when it is reached.
    share: Share,

    /// The one address the run may listen on (`--listen`), bound before the program starts.
    listening: Option<std::net::TcpListener>,
    /// What a request to that address names in its `Host` header: the granted host (an IPv6
    /// literal in brackets) and the bound port. From the grant alone.
    authority: Option<String>,
    /// `--instance-per-connection`: the one connection this instance of the program was started
    /// for, until `main` takes it (`net_authorize(2)`, once).
    connection: Option<NetEntry>,
    /// When that connection's time is up (`--connection-deadline-ms`): no wait goes past it.
    until: Option<std::time::Instant> }
enum NetEntry {
    /// A set of connections the program waits on together (`Group`): the sockets moved into it,
    /// each kept with the handle it had, and the listener it was made from, if any.
    Group { members: Vec<Option<(i64, NetEntry)>>, listener: Option<i64> },
    /// A socket that is in a group: its handle is not usable until the group gives it back.
    Grouped,
    Net { set: Vec<String>, pending: Vec<u8> },
    /// `drained`: the last `try_recv` came up short, so the next one says WouldBlock without
    /// asking (once): a task's `read` then pauses, and its poll finds what comes.
    /// `accepted`: it came from the granted listener, not from a `Net` that dialled.
    Socket { stream: std::net::TcpStream, out: Vec<u8>, input: Vec<u8>, at: usize, ended: bool, drained: bool, accepted: bool },
    /// Accepts connections on the granted address; dials nothing.
    Listener(std::net::TcpListener),
    /// A timer the program made with its `Clock`, due at a wall-clock millisecond: a poll finds
    /// it readable once due, and waits no longer than that.
    Timer(u64),
    Failed(i64),
    Closed,
}
// Error codes, as library/wasm-net.wy reads them.
const NOT_GRANTED: i64 = 1; const INVALID_ADDRESS: i64 = 2; const NET_TOO_LARGE: i64 = 3; const REFUSED: i64 = 4; const NET_IO: i64 = 5; const TIMED_OUT: i64 = 6;
/// A `try_` operation found nothing to do now.
const WOULD_BLOCK: i64 = 7;
/// The slot a wait names for the listener its group was made from.
const LISTENER_SLOT: u64 = 0xFF_FFFF;
impl Network {
    /// The byte budget is used up: said once, and the code the program gets.
    fn full(budget: u64, share: Share) -> i64 {
        match share {
            Share::Connection => limit::reached(limit::NET_EACH, || format!("network bytes: a connection's instance has sent and received its {budget} bytes ({}); that instance sends and receives nothing more, and the server goes on", limit::name("--net-bytes-per-connection"))),
            _ => { let budget = share.of_run(budget); limit::reached(limit::NET, || format!("network bytes: the run has sent and received its {budget} granted bytes ({}); nothing more is sent or received", limit::name("--net-bytes"))) }
        }
        NET_TOO_LARGE
    }
    fn entry(&mut self, handle: i64) -> wasmtime::Result<&mut NetEntry> {
        let index = handle.checked_sub(FIRST_HANDLE).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| reject("invalid network capability"))?;
        self.entries.get_mut(index).ok_or_else(|| reject("invalid network capability"))
    }
    /// A new handle, reusing a closed one's number: a server that accepts and closes connections
    /// for as long as it runs needs no more handles than it has open at once. A number is reused
    /// only after its handle was given back, which source code cannot then use (it was moved).
    fn issue(&mut self, entry: NetEntry) -> wasmtime::Result<i64> {
        if let Some(free) = self.entries.iter().position(|e| matches!(e, NetEntry::Closed)) {
            self.entries[free] = entry;
            return Ok(FIRST_HANDLE + free as i64);
        }
        if self.entries.len() >= MAX_HANDLES { return Err(reject("network handle limit exceeded")); }
        self.entries.push(entry);
        Ok(FIRST_HANDLE + self.entries.len() as i64 - 1)
    }
    /// A group of connections, made from a listener (which it may then wait on) or a `Net`.
    fn group(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let listener = match self.entry(handle)? { NetEntry::Listener(_) => Some(handle), NetEntry::Net { .. } => None, _ => return Err(reject("invalid network capability")) };
        self.issue(NetEntry::Group { members: Vec::new(), listener })
    }
    /// Move a socket into a group: its slot number.
    fn group_add(&mut self, group: i64, socket: i64) -> wasmtime::Result<i64> {
        if !matches!(self.entry(group)?, NetEntry::Group { .. }) { return Err(reject("invalid network capability")); }
        let entry = std::mem::replace(self.entry(socket)?, NetEntry::Grouped);
        if !matches!(entry, NetEntry::Socket { .. }) { *self.entry(socket)? = entry; return Err(reject("invalid network capability")); }
        let NetEntry::Group { members, .. } = self.entry(group)? else { unreachable!("checked above") };
        let slot = members.iter().position(Option::is_none).unwrap_or(members.len());
        if slot == members.len() { if slot >= MAX_HANDLES { return Err(reject("network handle limit exceeded")); } members.push(None); }
        members[slot] = Some((socket, entry));
        Ok(slot as i64)
    }
    /// Give a socket back from its slot, with the handle it had.
    fn group_take(&mut self, group: i64, slot: i64) -> wasmtime::Result<i64> {
        let NetEntry::Group { members, .. } = self.entry(group)? else { return Err(reject("invalid network capability")) };
        let (handle, entry) = usize::try_from(slot).ok().and_then(|s| members.get_mut(s)).and_then(Option::take).ok_or_else(|| reject("no socket in this slot of the group"))?;
        *self.entry(handle)? = entry;
        Ok(handle)
    }
    /// Bytes queued a byte at a time, sent without waiting: 0 once none are left,
    /// `WOULD_BLOCK` while the socket takes no more, `NET_IO` when it failed. A `try_`
    /// operation never waits, so it never flushes by blocking.
    fn flush_now(stream: &std::net::TcpStream, out: &mut Vec<u8>) -> i64 {
        use std::os::fd::AsRawFd;
        if out.is_empty() { return 0; }
        let _apart = Unblocked::new(stream);
        while !out.is_empty() {
            match sys::try_send(stream.as_raw_fd(), out) {
                Ok(count) => { out.drain(..count); }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return WOULD_BLOCK,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => { out.clear(); return NET_IO; }
            }
        }
        0
    }
    /// The number of a socket's or listener's handle, for a wait entry.
    fn handle_number(&mut self, handle: i64) -> wasmtime::Result<i64> {
        match self.entry(handle)? { NetEntry::Socket { .. } | NetEntry::Listener(_) => Ok(handle), _ => Err(reject("invalid network capability")) }
    }
    /// As `wait`, with entries naming handles (`handle << 8 | wanted`): the program's own
    /// sockets and listeners, which tasks pause waiting on.
    fn poll_handles(&mut self, entries: &mut [u64]) -> wasmtime::Result<i64> {
        use std::os::fd::AsRawFd;
        // What an entry is: a descriptor to poll, input already received (ready now), one of
        // the program's timers, or a number that names none of the program's sockets,
        // listeners and timers (closed or never one), which reads as closed, at once: one
        // task's bad token never fails the others' wait. Only a `Clock` makes a timer, so a
        // program without one cannot ask the time this way.
        const BUFFERED: i32 = -2;
        const NONE: i32 = -3;
        const TIMER: i32 = -4;
        let now_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut dues = Vec::with_capacity(entries.len());
        let mut fds = Vec::with_capacity(entries.len());
        let mut soonest: Option<u64> = None;
        for entry in entries.iter() {
            let wanted = *entry & 3;
            let events = if wanted & 1 != 0 { sys::POLLIN } else { 0 } | if wanted & 2 != 0 { sys::POLLOUT } else { 0 };
            let mut due = 0;
            // 0 is a hole: passed over, never ready (`Waiting` keeps its listener's place so).
            if *entry == 0 { dues.push(0); fds.push(sys::PollFd { fd: -1, events: 0, revents: 0 }); continue; }
            // Polling only looks: it never writes (a blocking flush here could outlast the run's
            // deadline), and a socket's own operations flush what it has queued.
            let fd = match i64::try_from(*entry >> 8).ok().and_then(|handle| self.entry(handle).ok()) {
                Some(NetEntry::Socket { stream, input, at, .. }) => {
                    // Bytes already received are ready now.
                    if wanted & 1 != 0 && *at < input.len() { BUFFERED } else { stream.as_raw_fd() }
                }
                Some(NetEntry::Listener(l)) => l.as_raw_fd(),
                Some(NetEntry::Timer(at)) => { due = *at; soonest = Some(soonest.map_or(due, |s| s.min(due))); TIMER }
                _ => NONE,
            };
            dues.push(due);
            fds.push(sys::PollFd { fd, events, revents: 0 });
        }
        let now: Vec<i32> = fds.iter().map(|f| f.fd).collect();
        // An entry ready without polling makes the wait return at once; a timer bounds it.
        let at_once = now.iter().any(|&fd| fd == BUFFERED || fd == NONE);
        for f in fds.iter_mut().filter(|f| f.fd < 0) { f.fd = -1; f.revents = 0; }
        let run_left = DEADLINE_AT.get().map(|at| at.saturating_duration_since(std::time::Instant::now()).as_millis().min(i32::MAX as u128) as i32).unwrap_or(1000);
        let timer_left = soonest.map(|due| due.saturating_sub(now_ms()).min(i32::MAX as u64) as i32);
        let left = if at_once { 0 } else { self.within(timer_left.map_or(run_left, |t| t.min(run_left))) };
        sys::poll(&mut fds, left).map_err(|_| reject("waiting on the network failed"))?;
        let after = if soonest.is_some() { now_ms() } else { 0 };
        let mut ready = 0;
        for (((entry, fd), kind), due) in entries.iter_mut().zip(&fds).zip(&now).zip(&dues) {
            let mut got = 0;
            if fd.revents & sys::POLLIN != 0 || *kind == BUFFERED || (*kind == TIMER && after >= *due) { got |= 4; }
            if fd.revents & sys::POLLOUT != 0 { got |= 8; }
            if fd.revents & (sys::POLLERR | sys::POLLHUP) != 0 || *kind == NONE { got |= 16; }
            // Readable now: the next `try_recv` asks the socket again.
            if fd.revents & sys::POLLIN != 0 && let Ok(NetEntry::Socket { drained, .. }) = self.entry((*entry >> 8) as i64) { *drained = false; }
            *entry = (*entry & !0x1c) | got;
            if got != 0 { ready += 1; }
        }
        Ok(ready)
    }
    /// Wait until at least one of `entries` is ready, or the run's deadline: each entry is
    /// `slot << 8 | wanted` (1 read, 2 write) and gets `4` (readable), `8` (writable) or `16`
    /// (closed or failed) added. How many are ready.
    fn wait(&mut self, group: i64, entries: &mut [u64]) -> wasmtime::Result<i64> {
        use std::os::fd::AsRawFd;
        let (fds, listener) = {
            let NetEntry::Group { members, listener } = self.entry(group)? else { return Err(reject("invalid network capability")) };
            let mut fds = Vec::with_capacity(entries.len());
            for entry in entries.iter() {
                let (slot, wanted) = (*entry >> 8, *entry & 3);
                let events = if wanted & 1 != 0 { sys::POLLIN } else { 0 } | if wanted & 2 != 0 { sys::POLLOUT } else { 0 };
                // A slot holding no socket (taken back, or never filled) reads as closed, at once,
                // as a poll's entry naming no socket does: one stale slot fails nothing else.
                let fd = if slot == LISTENER_SLOT { -1 } else {
                    match usize::try_from(slot).ok().and_then(|s| members.get(s)) {
                        Some(Some((_, NetEntry::Socket { stream, .. }))) => stream.as_raw_fd(),
                        _ => -3,
                    }
                };
                fds.push(sys::PollFd { fd, events, revents: 0 });
            }
            (fds, *listener)
        };
        let mut fds = fds;
        for (entry, fd) in entries.iter().zip(fds.iter_mut()) {
            if *entry >> 8 == LISTENER_SLOT {
                let Some(NetEntry::Listener(l)) = listener.map(|h| self.entry(h)).transpose()? else { return Err(reject("this group was not made from a listener")) };
                fd.fd = l.as_raw_fd();
            }
        }
        let empty: Vec<bool> = fds.iter().map(|f| f.fd == -3).collect();
        for f in fds.iter_mut().filter(|f| f.fd == -3) { f.fd = -1; }
        let left = if empty.contains(&true) { 0 } else { DEADLINE_AT.get().map(|at| at.saturating_duration_since(std::time::Instant::now()).as_millis().min(i32::MAX as u128) as i32).unwrap_or(1000) };
        let left = self.within(left);
        sys::poll(&mut fds, left).map_err(|_| reject("waiting on the group failed"))?;
        let mut ready = 0;
        let NetEntry::Group { members, .. } = self.entry(group)? else { return Err(reject("invalid network capability")) };
        for ((entry, fd), empty) in entries.iter_mut().zip(&fds).zip(&empty) {
            let mut got = if *empty { 16 } else { 0 };
            if fd.revents & sys::POLLIN != 0 { got |= 4; }
            // Readable now: the next `try_recv` asks the socket again.
            if fd.revents & sys::POLLIN != 0 && let Some(Some((_, NetEntry::Socket { drained, .. }))) = usize::try_from(*entry >> 8).ok().and_then(|slot| members.get_mut(slot)) { *drained = false; }
            if fd.revents & sys::POLLOUT != 0 { got |= 8; }
            if fd.revents & (sys::POLLERR | sys::POLLHUP) != 0 { got |= 16; }
            *entry = (*entry & !0x1c) | got;
            if got != 0 { ready += 1; }
        }
        Ok(ready)
    }
    /// What a socket has received, without waiting, copied into `buffer`: `count | error << 32`,
    /// 0 at the end, `WOULD_BLOCK << 32` when nothing has come.
    fn try_receive(&mut self, handle: i64, buffer: &mut [u8]) -> wasmtime::Result<i64> {
        use std::os::fd::AsRawFd;
        let (room, budget) = (self.budget.saturating_sub(self.used), self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, input, at, ended, drained, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        match Self::flush_now(stream, out) { 0 => {}, code => return Ok(code << 32) }
        let limit = buffer.len().min(usize::try_from(room).unwrap_or(usize::MAX)).min(65_536);
        if limit == 0 { return Ok(if room == 0 { Self::full(budget, share) << 32 } else { 0 }); }
        let count = if *at < input.len() {
            let end = (*at + limit).min(input.len());
            buffer[..end - *at].copy_from_slice(&input[*at..end]);
            let count = end - *at;
            *at = end;
            count
        } else if *ended { 0 } else if *drained {
            *drained = false;
            return Ok(WOULD_BLOCK << 32);
        } else {
            let _apart = Unblocked::new(stream);
            match sys::try_recv(stream.as_raw_fd(), &mut buffer[..limit]) {
                Ok(0) => { *ended = true; 0 }
                Ok(count) => { *drained = count < limit; count }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(WOULD_BLOCK << 32),
                Err(_) => return Ok(NET_IO << 32),
            }
        };
        self.used += count as u64;
        Ok(count as i64)
    }
    /// As much of `bytes` as the socket takes now: `count | error << 32`.
    fn try_send(&mut self, handle: i64, bytes: &[u8]) -> wasmtime::Result<i64> {
        use std::os::fd::AsRawFd;
        let (room, budget) = (self.budget.saturating_sub(self.used), self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        match Self::flush_now(stream, out) { 0 => {}, code => return Ok(code << 32) }
        let allowed = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        if allowed == 0 && !bytes.is_empty() { return Ok(Self::full(budget, share) << 32); }
        let sent = { let _apart = Unblocked::new(stream); sys::try_send(stream.as_raw_fd(), &bytes[..allowed]) };
        let result = match sent {
            Ok(count) => { self.used += count as u64; count as i64 }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => WOULD_BLOCK << 32,
            Err(_) => NET_IO << 32,
        };
        Ok(result)
    }
    /// The next waiting connection, without waiting: a socket, or one whose status is
    /// `WOULD_BLOCK`.
    fn try_accept(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let (used, budget) = (self.used, self.budget); let share = self.share;
        let NetEntry::Listener(listener) = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        // As `accept`: with the budget used up no connection is taken (it could be neither
        // read nor answered), and the program is told so, at once and by the budget's own code.
        if used >= budget { let code = Self::full(budget, share); return self.issue(NetEntry::Failed(code)); }
        let timeout = std::time::Duration::from_secs(2);
        let entry = match listener.accept() {
            Ok((stream, _)) => {
                let ready = stream.set_nonblocking(false).and_then(|()| stream.set_read_timeout(Some(timeout))).and_then(|()| stream.set_write_timeout(Some(timeout)))
                    .and_then(|()| stream.set_nodelay(true));
                if ready.is_ok() { NetEntry::Socket { stream, out: Vec::new(), input: Vec::new(), at: 0, ended: false, drained: false, accepted: true } } else { NetEntry::Failed(NET_IO) }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => NetEntry::Failed(WOULD_BLOCK),
            Err(_) => NetEntry::Failed(NET_IO),
        };
        self.issue(entry)
    }
    /// The granted listening authority a socket was accepted on, written to `out`: its length,
    /// or 0 for a socket the program dialled, or where `out` is too short for all of it. What
    /// the grant names, never what the peer sent or the system resolves.
    fn accepted_on(&mut self, handle: i64, out: &mut [u8]) -> wasmtime::Result<i64> {
        let NetEntry::Socket { accepted, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        let accepted = *accepted;
        Ok(match &self.authority {
            Some(authority) if accepted && authority.len() <= out.len() => { out[..authority.len()].copy_from_slice(authority.as_bytes()); authority.len() as i64 }
            _ => 0,
        })
    }
    /// The address built on a `Net`, if that `Net` reaches it.
    fn address(&mut self, handle: i64) -> wasmtime::Result<Result<String, i64>> {
        let NetEntry::Net { set, pending } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        let taken = std::mem::take(pending);
        let Ok(address) = String::from_utf8(taken) else { return Ok(Err(INVALID_ADDRESS)) };
        Ok(if set.contains(&address) { Ok(address) } else { Err(NOT_GRANTED) })
    }
    fn only(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let entry = match self.address(handle)? { Ok(address) => NetEntry::Net { set: vec![address], pending: Vec::new() }, Err(code) => NetEntry::Failed(code) };
        self.issue(entry)
    }
    fn connect(&mut self, handle: i64) -> wasmtime::Result<i64> {
        use std::net::ToSocketAddrs;
        let entry = match self.address(handle)? {
            Err(code) => NetEntry::Failed(code),
            Ok(address) => {
                let timeout = std::time::Duration::from_secs(2);
                let mut connected = Err(INVALID_ADDRESS);
                if let Ok(candidates) = address.to_socket_addrs() {
                    connected = Err(REFUSED);
                    for candidate in candidates {
                        if let Ok(stream) = std::net::TcpStream::connect_timeout(&candidate, timeout) { connected = Ok(stream); break; }
                    }
                }
                match connected {
                    Ok(stream) => {
                        let _ = stream.set_read_timeout(Some(timeout));
                        let _ = stream.set_write_timeout(Some(timeout));
                        // The host buffers what a program sends and writes it when told to (a
                        // flush or a bulk send), so Nagle's delay only adds latency.
                        let _ = stream.set_nodelay(true);
                        NetEntry::Socket { stream, out: Vec::new(), input: Vec::new(), at: 0, ended: false, drained: false, accepted: false }
                    }
                    Err(code) => NetEntry::Failed(code),
                }
            }
        };
        self.issue(entry)
    }
    /// A wait of `ms`, cut to what is left of this connection's time, if it has one.
    fn within(&self, ms: i32) -> i32 {
        match self.until { Some(at) => ms.min(at.saturating_duration_since(std::time::Instant::now()).as_millis().min(i32::MAX as u128) as i32), None => ms }
    }
    /// A connection just accepted, as the program holds it: blocking, with the two-second
    /// read and write waits every socket has, and no Nagle delay (the host buffers).
    fn accepted(stream: std::net::TcpStream) -> NetEntry {
        let timeout = std::time::Duration::from_secs(2);
        // An accepted socket may inherit the listener's non-blocking mode.
        let ready = stream.take_error().and_then(|pending| pending.map_or(Ok(()), Err))
            .and_then(|()| stream.set_nonblocking(false)).and_then(|()| stream.set_read_timeout(Some(timeout)))
            .and_then(|()| stream.set_write_timeout(Some(timeout))).and_then(|()| stream.set_nodelay(true));
        if ready.is_ok() { NetEntry::Socket { stream, out: Vec::new(), input: Vec::new(), at: 0, ended: false, drained: false, accepted: true } } else { NetEntry::Failed(NET_IO) }
    }
    /// Sleeps until `listener` has a connection waiting or `until` has passed: whether there is
    /// time left to try again. Never longer than `until`, rounded up to a millisecond.
    fn await_readable(listener: &std::net::TcpListener, until: std::time::Instant) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd;
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() { return Ok(false); }
        let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        let mut fds = [sys::PollFd { fd: listener.as_raw_fd(), events: sys::POLLIN, revents: 0 }];
        sys::poll(&mut fds, ms).map(|_| true)
    }
    /// The next connection to the granted address, waiting up to two seconds for one.
    fn accept(&mut self, handle: i64) -> wasmtime::Result<i64> {
        // The network budget is used up: a connection accepted now could be neither read nor
        // answered, so none is taken from the queue, and the program is told at the place it
        // decides whether to go on serving: `TooLarge`, after the whole wait (a loop that
        // ignores it turns once in two seconds, not as fast as it can).
        if self.used >= self.budget && matches!(self.entry(handle)?, NetEntry::Listener(_)) {
            std::thread::sleep(std::time::Duration::from_secs(2));
            return self.issue(NetEntry::Failed(Self::full(self.budget, self.share)));
        }
        let NetEntry::Listener(listener) = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        let timeout = std::time::Duration::from_secs(2);
        let until = std::time::Instant::now() + timeout;
        let entry = loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // An accepted socket may inherit the listener's non-blocking mode.
                    let ready = stream.set_nonblocking(false).and_then(|()| stream.set_read_timeout(Some(timeout))).and_then(|()| stream.set_write_timeout(Some(timeout)))
                        .and_then(|()| stream.set_nodelay(true));
                    break if ready.is_ok() { NetEntry::Socket { stream, out: Vec::new(), input: Vec::new(), at: 0, ended: false, drained: false, accepted: true } } else { NetEntry::Failed(NET_IO) };
                }
                // Nothing waiting: sleep until the listener is readable, or the wait is over. The
                // system wakes this thread when a connection arrives (it used to look every 5 ms,
                // which put a floor of about 7 ms under every request answered one at a time).
                // The run's deadline and the lifeline are other threads that end the process, so
                // a thread asleep here holds neither back; an error ends the wait as a timeout
                // does, never as a connection.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // A wait that failed is a failure, not a whole wait: it refills nothing.
                    match Self::await_readable(listener, until) { Ok(true) => {}, Ok(false) => break NetEntry::Failed(TIMED_OUT), Err(_) => break NetEntry::Failed(NET_IO) }
                }
                Err(_) => break NetEntry::Failed(NET_IO),
            }
        };
        self.issue(entry)
    }
    fn flush(stream: &mut std::net::TcpStream, out: &mut Vec<u8>) -> i64 {
        let written = stream.write_all(out).and_then(|()| stream.flush());
        out.clear();
        if written.is_ok() { 0 } else { NET_IO }
    }
    fn send(&mut self, handle: i64, byte: i64) -> wasmtime::Result<i64> {
        if !(0..256).contains(&byte) { return Err(reject("invalid network capability")); }
        let (full, budget) = (self.used >= self.budget, self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        if full { return Ok(Self::full(budget, share)); }
        out.push(byte as u8);
        let code = if out.len() >= 65_536 { Self::flush(stream, out) } else { 0 };
        self.used += 1;
        Ok(code)
    }
    fn receive(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let (full, budget) = (self.used >= self.budget, self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, input, at, ended, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        // The budget counts every byte handed over, buffered or not.
        if full { let _ = Self::full(budget, share); return Ok(257); }
        if *at == input.len() {
            // What was queued goes out before an answer is awaited (and only then: a byte
            // already received needs no flush, which would be a write per byte echoed).
            if !out.is_empty() && Self::flush(stream, out) != 0 { return Ok(257); }
            if *ended { return Ok(256); }
            input.resize(4096, 0);
            match stream.read(input) {
                Ok(0) => { input.clear(); *at = 0; *ended = true; return Ok(256); }
                Ok(count) => { input.truncate(count); *at = 0; }
                Err(_) => { input.clear(); *at = 0; return Ok(257); }
            }
        }
        let byte = input[*at];
        *at += 1;
        self.used += 1;
        Ok(i64::from(byte))
    }
    /// Send `bytes`, after whatever was queued a byte at a time: as many as the connection
    /// accepts, `count | error << 32`. A failed write still reports how much went out before it.
    fn send_bulk(&mut self, handle: i64, bytes: &[u8]) -> wasmtime::Result<i64> {
        let (room, budget) = (self.budget.saturating_sub(self.used), self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        if !out.is_empty() && Self::flush(stream, out) != 0 { return Ok(NET_IO << 32); }
        let allowed = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        let mut sent = 0;
        let mut code = if allowed < bytes.len() { Self::full(budget, share) } else { 0 };
        while sent < allowed {
            match stream.write(&bytes[sent..allowed]) {
                Ok(0) => { code = NET_IO; break; }
                Ok(count) => sent += count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => { code = TIMED_OUT; break; }
                Err(_) => { code = NET_IO; break; }
            }
        }
        self.used += sent as u64;
        Ok(sent as i64 | code << 32)
    }
    /// Up to `limit` bytes received: what was already buffered, or one read. 0 at the end;
    /// `count | error << 32`.
    fn receive_bulk(&mut self, handle: i64, limit: usize) -> wasmtime::Result<(Vec<u8>, i64)> {
        let (room, budget) = (self.budget.saturating_sub(self.used), self.budget); let share = self.share;
        let NetEntry::Socket { stream, out, input, at, ended, .. } = self.entry(handle)? else { return Err(reject("invalid network capability")) };
        if !out.is_empty() && Self::flush(stream, out) != 0 { return Ok((Vec::new(), NET_IO << 32)); }
        let limit = limit.min(usize::try_from(room).unwrap_or(usize::MAX)).min(65_536);
        if limit == 0 { return Ok((Vec::new(), if room == 0 { Self::full(budget, share) << 32 } else { 0 })); }
        let taken = if *at < input.len() {
            let end = (*at + limit).min(input.len());
            let part = input[*at..end].to_vec();
            *at = end;
            part
        } else if *ended {
            Vec::new()
        } else {
            let mut buffer = vec![0u8; limit];
            match stream.read(&mut buffer) {
                Ok(0) => { *ended = true; Vec::new() }
                Ok(count) => { buffer.truncate(count); buffer }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok((Vec::new(), TIMED_OUT << 32)),
                Err(_) => return Ok((Vec::new(), NET_IO << 32)),
            }
        };
        self.used += taken.len() as u64;
        let count = taken.len() as i64;
        Ok((taken, count))
    }
    fn close(&mut self, handle: i64) -> wasmtime::Result<i64> {
        Ok(match std::mem::replace(self.entry(handle)?, NetEntry::Closed) {
            NetEntry::Closed | NetEntry::Grouped => return Err(reject("invalid network capability")),
            NetEntry::Socket { mut stream, mut out, .. } => { let code = Self::flush(&mut stream, &mut out); let _ = stream.shutdown(std::net::Shutdown::Both); code }
            // A group gives back every socket still in it: each is closed, and its handle freed.
            NetEntry::Group { members, .. } => {
                for (member, entry) in members.into_iter().flatten() {
                    if let NetEntry::Socket { mut stream, mut out, .. } = entry { let _ = Self::flush(&mut stream, &mut out); let _ = stream.shutdown(std::net::Shutdown::Both); }
                    if let Ok(slot) = self.entry(member) { *slot = NetEntry::Closed; }
                }
                0
            }
            _ => 0,
        })
    }
}

/// The secrets a run grants: a store's by name (`--secret=NAME`, or `--secret-seal=NAME` for
/// signing only), each with its value from this process's variable NAME, or from a descriptor
/// it was started with (`--secret=NAME@fd:N`, read with the fixed secrets); and `main`'s fixed
/// secrets (`--secret-slot=`), each bound by its parameter's name before the program starts.
/// `variables`: every variable a secret flag names, taken out of the environment by `main`
/// before anything else ran (`take_secret_variables`), so no program the run starts can read
/// it from this process.
#[derive(Default)]
struct Secrets { granted: std::collections::BTreeMap<Vec<u8>, (Vec<u8>, bool)>, slots: Vec<SecretSlot>, entries: Vec<SecretEntry>,
    variables: std::collections::BTreeMap<String, Vec<u8>>,
    /// What each `fd:N` source is (device and inode), kept after it is closed: no other grant
    /// may reach the same object (`--input=/dev/fd/N` would drain a sealed key into the input).
    objects: Vec<(u64, u64)>,
    /// Whose source each of `objects` is, in the same order: `slot NAME` or `store secret NAME`.
    object_slots: Vec<String>,
    /// A store's secrets that come from a descriptor (`--secret=NAME@fd:N`): each name is in
    /// `granted` from the flag on, its value read by `bind` with the fixed secrets' bounds.
    store_sources: Vec<StoreSource> }
/// `--secret=NAME@fd:N` or `--secret-seal=NAME@fd:N`: the store's secret NAME, read from
/// descriptor N (held from the moment the flag is read, so its number cannot be reused).
struct StoreSource { name: Vec<u8>, source: Option<std::os::fd::OwnedFd>, number: u32 }
enum SecretEntry {
    Store { pending: Vec<u8> },
    /// `reveal`: the run granted reading the bytes; `seal`: signing with them. A store's secret
    /// always signs; a fixed secret does exactly what its use says, one or the other.
    Secret { value: Vec<u8>, reveal: bool, seal: bool, message: Vec<u8>, digest: [u8; 32] },
    /// `main`'s `Signer`: an ML-DSA-65 key, held by the provider, and the domain every message
    /// it signs begins with, `witchy/v2/ROLE\0` for the role its grant names. Nothing reads the
    /// key: the entry has no byte, message or digest operation.
    Signer { key: aws_lc_rs::signature::PqdsaKeyPair, domain: Vec<u8> },
    ApplicationSigner(mldsa_signer::Signer),
    TlsSigner(std::sync::Arc<aws_lc_rs::signature::EcdsaKeyPair>),
    Failed(i64),
}
/// Where a fixed secret's bytes come from: a variable of this process, or a descriptor it was
/// started with (held from the moment the flag is read, so its number cannot be reused).
enum SecretSource { Env(String), Fd(std::os::fd::OwnedFd, u32) }
/// `--secret-slot=NAME:USE:SOURCE`: `main`'s parameter NAME, its use (`kind`: 0 `reveal-bytes`,
/// 1 `seal`, 2 `sign/witchy-2/ROLE`, with `role`), and `env:VARIABLE` or `fd:N`. `value` is
/// read before the program starts (a signer's becomes `signer`, its key, and is cleared);
/// `slot` is the module's place for it, and `taken` whether `main` has been handed it (once).
struct SecretSlot { name: String, kind: u8, role: String, source: Option<SecretSource>, label: String, value: Vec<u8>, slot: usize, taken: bool,
    signer: Option<SigningKey> }

enum SigningKey {
    Registry(aws_lc_rs::signature::PqdsaKeyPair),
    Application(aws_lc_rs::signature::PqdsaKeyPair),
    Tls(std::sync::Arc<aws_lc_rs::signature::EcdsaKeyPair>),
}
/// What `main` takes for a slot of each kind, and the use its grant states.
fn slot_type(kind: u8) -> &'static str { match kind { 0 => "Secret<Reveal>", 1 => "Secret<Seal>", 2 => "Signer", 3 => "TlsSigner", _ => "Mldsa87Signer" } }
fn slot_use(kind: u8) -> &'static str { match kind { 0 => "reveal-bytes", 1 => "seal", 2 => "sign/witchy-2/ROLE", 3 => "sign/tls13/server-p384", _ => "sign/ml-dsa-87/DOMAIN" } }
/// A `witchy-2` role a signer's use names: 1 to 32 lowercase letters, digits and `-`.
fn plain_role(role: &str) -> bool { !role.is_empty() && role.len() <= 32 && role.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }
/// The most a signer signs, or a public operation hashes or verifies, at once.
const MAX_SIGNED: usize = 1 << 20;
const MAX_HASHED: usize = 64 << 20;
/// The most a fixed secret holds, as a store's secret.
const MAX_SECRET: usize = 65_536;
/// How long the descriptor sources of a run have, together, to reach their end: from when the
/// engine starts reading them, and never past the run's deadline less `SECRET_READ_MARGIN`, so
/// a writer that never closes is named by its slot, not reported as the run's timeout.
const SECRET_READ: std::time::Duration = std::time::Duration::from_secs(1);
const SECRET_READ_MARGIN: std::time::Duration = std::time::Duration::from_millis(100);
const MAX_MESSAGE: usize = 1 << 20;
impl Secrets {
    // A worker starts each connection with fresh handles and slot state.
    fn for_connection(&self) -> Self {
        let slots = self.slots.iter().filter_map(|slot| {
            let Some(SigningKey::Tls(key)) = &slot.signer else { return None; };
            Some(SecretSlot {
                name: slot.name.clone(), kind: slot.kind, role: String::new(),
                source: None, label: String::new(), value: Vec::new(), slot: slot.slot,
                taken: false, signer: Some(SigningKey::Tls(key.clone())),
            })
        }).collect();
        Self { slots, ..Self::default() }
    }

    fn entry(&mut self, handle: i64) -> wasmtime::Result<&mut SecretEntry> {
        let index = handle.checked_sub(FIRST_HANDLE).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| reject("invalid secret capability"))?;
        self.entries.get_mut(index).ok_or_else(|| reject("invalid secret capability"))
    }
    fn issue(&mut self, entry: SecretEntry) -> wasmtime::Result<i64> {
        if self.entries.len() >= MAX_HANDLES { return Err(reject("secret handle limit exceeded")); }
        self.entries.push(entry);
        Ok(FIRST_HANDLE + self.entries.len() as i64 - 1)
    }
    /// `sealed`: the program asks for the sealing view only; otherwise for both rights, which
    /// the run must have granted.
    fn get(&mut self, handle: i64, sealed: bool) -> wasmtime::Result<i64> {
        let SecretEntry::Store { pending } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        let name = std::mem::take(pending);
        let entry = match self.granted.get(&name) {
            Some((_, false)) if !sealed => SecretEntry::Failed(2),
            Some((value, reveal)) => SecretEntry::Secret { value: value.clone(), reveal: *reveal && !sealed, seal: true, message: Vec::new(), digest: [0; 32] },
            None => SecretEntry::Failed(1),
        };
        self.issue(entry)
    }
    /// Whether descriptor `number` is already a source, of a slot or of a store's secret: taken
    /// once only (two owners would close it twice).
    fn descriptor_selected(&self, number: u32) -> bool {
        self.slots.iter().any(|s| matches!(s.source, Some(SecretSource::Fd(_, n)) if n == number)) || self.store_sources.iter().any(|s| s.number == number)
    }
    /// A name a grant may use: letters, digits and `_`.
    fn plain(name: &str) -> bool { !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') }
    /// `main`'s fixed secret for `code` (`16 * (slot + 1) + kind`): a handle with
    /// exactly its use's right, once. Anything else stops the run.
    fn take_slot(&mut self, code: i64) -> wasmtime::Result<i64> {
        let refused = || reject("invalid secret capability");
        let (place, kind) = (usize::try_from(code / 16).ok().and_then(|p| p.checked_sub(1)).ok_or_else(refused)?, code % 16);
        if !(0..=4).contains(&kind) { return Err(refused()); }
        let slot = self.slots.iter_mut().find(|s| s.slot == place).ok_or_else(refused)?;
        if slot.taken || i64::from(slot.kind) != kind { return Err(refused()); }
        slot.taken = true;
        if slot.kind == 2 {
            let Some(SigningKey::Registry(key)) = slot.signer.take() else { return Err(refused()); };
            let domain = [b"witchy/v2/", slot.role.as_bytes(), b"\0"].concat();
            return self.issue(SecretEntry::Signer { key, domain });
        }
        if slot.kind == 3 {
            let Some(SigningKey::Tls(key)) = &slot.signer else { return Err(refused()); };
            let key = key.clone();
            return self.issue(SecretEntry::TlsSigner(key));
        }
        if slot.kind == 4 {
            let Some(SigningKey::Application(key)) = slot.signer.take() else { return Err(refused()); };
            let signer = mldsa_signer::Signer::new(key, &slot.role);
            return self.issue(SecretEntry::ApplicationSigner(signer));
        }
        let (value, seal) = (std::mem::take(&mut slot.value), slot.kind == 1);
        self.issue(SecretEntry::Secret { value, reveal: !seal, seal, message: Vec::new(), digest: [0; 32] })
    }
    /// A signer's public key, into a range of exactly its length: 0, or 2 for another length.
    /// Stops the run unless `handle` is a signer.
    fn signer(&mut self, handle: i64) -> wasmtime::Result<()> {
        match self.entry(handle)? { SecretEntry::Signer { .. } => Ok(()), _ => Err(reject("invalid secret capability")) }
    }
    fn public_key(&mut self, handle: i64, out: &mut [u8]) -> wasmtime::Result<i64> {
        let SecretEntry::Signer { key, .. } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        let public = sha::mldsa65_public_key(key);
        if out.len() != public.len() { return Ok(2); }
        out.copy_from_slice(public);
        Ok(0)
    }
    /// A signer's signature of `message`, or the code the guest is told: 1 the message does not
    /// begin with the signer's domain, 3 it is too long, 4 the provider failed.
    fn sign(&mut self, handle: i64, message: &[u8]) -> wasmtime::Result<Result<[u8; sha::MLDSA65_SIGNATURE], i64>> {
        let SecretEntry::Signer { key, domain } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        if message.len() > MAX_SIGNED { return Ok(Err(3)); }
        if !message.starts_with(domain) { return Ok(Err(1)); }
        // A provider that is not the FIPS module in FIPS mode signs nothing: "failed", 4.
        if sha::provider().is_err() { return Ok(Err(4)); }
        Ok(sha::mldsa65_sign(key, message).map_err(|()| 4))
    }
    /// The byte of a secret at `index`, 256 past its end, or 257 when it may not be read.
    fn byte(&mut self, handle: i64, index: i64) -> wasmtime::Result<i64> {
        let SecretEntry::Secret { value, reveal, .. } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        // A secret granted for signing only is never read, whatever the module asks.
        if !*reveal { return Ok(257); }
        Ok(usize::try_from(index).ok().and_then(|i| value.get(i)).map_or(256, |b| i64::from(*b)))
    }
    /// A byte of the message to sign: only with a secret that may sign.
    fn message_byte(&mut self, handle: i64, byte: i64) -> wasmtime::Result<i64> {
        let SecretEntry::Secret { message, seal: true, .. } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        if message.len() >= MAX_MESSAGE { return Err(reject("secret message limit exceeded")); }
        message.push(byte as u8);
        Ok(0)
    }
    fn hmac(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let SecretEntry::Secret { value, message, digest, seal: true, .. } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        sha::provider().map_err(reject)?;
        *digest = sha::hmac_sha256(value, message);
        message.clear();
        Ok(0)
    }
    fn digest_byte(&mut self, handle: i64, index: i64) -> wasmtime::Result<i64> {
        let SecretEntry::Secret { digest, seal: true, .. } = self.entry(handle)? else { return Err(reject("invalid secret capability")) };
        Ok(usize::try_from(index).ok().and_then(|i| digest.get(i)).map_or(256, |b| i64::from(*b)))
    }
    /// Binds each fixed secret to the module's slot of its name, then reads its bytes: all
    /// before the program starts. A slot without a binding, a binding without a slot, another
    /// use than the program's type, or a source that cannot be read stops the run, named by
    /// its slot and the kind of failure and never by a value.
    fn bind(&mut self, layout: &[profile::Slot]) -> wasmtime::Result<()> {
        let bound = self.bind_slots(layout);
        // Whatever stopped the run, no descriptor or value stays held for it.
        if bound.is_err() {
            for slot in &mut self.slots { slot.source = None; slot.value.fill(0); slot.value.clear(); slot.signer = None; }
            for store in &mut self.store_sources { store.source = None; }
            for (value, _) in self.granted.values_mut() { value.fill(0); value.clear(); }
        }
        bound
    }
    fn bind_slots(&mut self, layout: &[profile::Slot]) -> wasmtime::Result<()> {
        for wanted in layout {
            if !self.slots.iter().any(|s| s.name == wanted.name) { return Err(reject(&format!("secret slot {}: no binding (`--secret-slot={}:{}:SOURCE`)", wanted.name, wanted.name, slot_use(wanted.kind)))); }
        }
        for slot in &mut self.slots {
            let Some(place) = layout.iter().position(|s| s.name == slot.name) else { return Err(reject(&format!("secret slot {}: not a secret parameter of main", slot.name))) };
            if layout[place].kind != slot.kind { return Err(reject(&format!("secret slot {}: wrong use (main takes a {})", slot.name, slot_type(layout[place].kind)))); }
            slot.slot = place;
        }
        let now = std::time::Instant::now();
        let until = (now + SECRET_READ).min(DEADLINE_AT.get().map_or(now + SECRET_READ, |at| at.checked_sub(SECRET_READ_MARGIN).unwrap_or(now)));
        for slot in &mut self.slots {
            let fail = |class: &str| reject(&format!("secret slot {}: {class} ({})", slot.name, slot.label));
            slot.value = match slot.source.take() {
                // Taken out of the environment when the process started (`take_secret_variables`).
                Some(SecretSource::Env(variable)) => self.variables.remove(&variable).ok_or_else(|| fail("missing input"))?,
                // To its end, within the bound in size and in time; then closed, whatever happened.
                Some(SecretSource::Fd(fd, _)) => sys::read_until(fd, MAX_SECRET, until).map_err(|why| fail(match why {
                    sys::ReadFailure::Unreadable => "unreadable descriptor",
                    sys::ReadFailure::TooLarge => "input too large",
                    sys::ReadFailure::NotClosed => "descriptor not closed in time",
                }))?,
                None => return Err(fail("no source")),
            };
            if slot.value.len() > MAX_SECRET { return Err(fail("input too large")); }
            // A signer's key is made now, before the program starts: exactly a 32-byte ML-DSA-65
            // seed (FIPS 204's xi), or the run stops. The seed is cleared once the provider has it.
            if slot.kind == 2 {
                let made = sha::provider().and_then(|()| sha::mldsa65_from_seed(&slot.value));
                slot.value.fill(0);
                slot.value.clear();
                slot.signer = Some(SigningKey::Registry(made.map_err(fail)?));
            } else if slot.kind == 3 {
                let bytes = zeroize::Zeroizing::new(std::mem::take(&mut slot.value));
                let key = tls_signer::from_pkcs8(&bytes).map_err(fail)?;
                slot.signer = Some(SigningKey::Tls(std::sync::Arc::new(key)));
            } else if slot.kind == 4 {
                let bytes = zeroize::Zeroizing::new(std::mem::take(&mut slot.value));
                let key = sha::provider().and_then(|()| sha::mldsa87_from_seed(&bytes)).map_err(fail)?;
                slot.signer = Some(SigningKey::Application(key));
            }
        }
        // A store's secrets from descriptors: the same reading, bounds and failures, named by
        // the secret (never a value).
        for store in &mut self.store_sources {
            let name = String::from_utf8_lossy(&store.name).into_owned();
            let fail = |class: &str| reject(&format!("secret {name}: {class} (fd:{})", store.number));
            let Some(fd) = store.source.take() else { return Err(fail("no source")) };
            let value = sys::read_until(fd, MAX_SECRET, until).map_err(|why| fail(match why {
                sys::ReadFailure::Unreadable => "unreadable descriptor",
                sys::ReadFailure::TooLarge => "input too large",
                sys::ReadFailure::NotClosed => "descriptor not closed in time",
            }))?;
            if value.len() > MAX_SECRET { return Err(fail("input too large")); }
            let Some(entry) = self.granted.get_mut(&store.name) else { return Err(fail("no source")) };
            entry.0 = value;
        }
        Ok(())
    }
}
/// When the run's deadline falls, for host I/O that uses the remaining time.
static DEADLINE_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
/// The programs a run may start (`--exec=DIR:name,...`) and what each run left behind.
/// A program is the file of that name in the granted directory, opened from it without
/// following a link, copied into a private directory so nothing can swap it before it starts,
/// and run there with the arguments given, an empty environment and no input.
#[derive(Default)]
struct Programs { dir: Option<Dir>, names: Vec<String>, budget: u64, used: u64, runs: u64, entries: Vec<ExecEntry>,
    /// The variable names a program may be given (`--exec-env=`, a grant's `env`).
    env_names: Vec<String>,
    /// Argument bytes held across every `Exec` and not yet run: one bound for all of them.
    pending: usize }
const MAX_PENDING: usize = 1 << 20;
enum ExecEntry {
    Exec { names: Vec<String>, name: Vec<u8>, args: Vec<Vec<u8>>, arg: Vec<u8>, settings: RunSettings },
    Run { out: Vec<u8>, at: usize, err: Vec<u8>, err_at: usize, code: i64 },
    Failed(i64),
}
/// What the next run of an `Exec` was narrowed to (the library's `Command`): its input, a shorter
/// time limit, a smaller output limit. A setting can only narrow: one asking for more than the
/// run grants fails that run with `EXEC_WIDER`, a zero time limit with `EXEC_INVALID`.
#[derive(Default)]
struct RunSettings { input: Vec<u8>, deadline_ms: Option<u64>, output_limit: Option<u64>, refused: i64,
    /// Declared variables (`NAME`, value), the one being built, and the directory to run in (a
    /// granted `Dir` the program gave up: its opened descriptor, never a path).
    env: Vec<(Vec<u8>, Vec<u8>)>, env_name: Vec<u8>, env_value: Vec<u8>, cwd: Option<Dir> }
/// The time limit of one program, and the most any setting may ask for.
const EXEC_RUN_MS: u64 = 2000;
// Error codes, as library/wasm-exec.wy reads them.
const EXEC_NOT_GRANTED: i64 = 1; const EXEC_INVALID: i64 = 2; const EXEC_TOO_LARGE: i64 = 3; const EXEC_NOT_FOUND: i64 = 4; const EXEC_FAILED: i64 = 5; const EXEC_TIMED_OUT: i64 = 6; const EXEC_WIDER: i64 = 7;
const MAX_ARGS: usize = 64; const MAX_ARG: usize = 4096;
impl Programs {
    fn entry(&mut self, handle: i64) -> wasmtime::Result<&mut ExecEntry> {
        let index = handle.checked_sub(FIRST_HANDLE).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| reject("invalid exec capability"))?;
        self.entries.get_mut(index).ok_or_else(|| reject("invalid exec capability"))
    }
    fn issue(&mut self, entry: ExecEntry) -> wasmtime::Result<i64> {
        if self.entries.len() >= MAX_HANDLES { return Err(reject("exec handle limit exceeded")); }
        self.entries.push(entry);
        Ok(FIRST_HANDLE + self.entries.len() as i64 - 1)
    }
    /// A name a grant may list: a plain file name.
    fn plain(name: &str) -> bool { !name.is_empty() && name.len() <= 255 && name != "." && name != ".." && !name.contains(['/', '\\', '\0', ',', ':']) }
    /// The name built on an `Exec`, if that `Exec` may run it.
    fn chosen(names: &[String], name: Vec<u8>) -> Result<String, i64> {
        let name = String::from_utf8(name).map_err(|_| EXEC_INVALID)?;
        if !Self::plain(&name) { return Err(EXEC_INVALID); }
        if names.contains(&name) { Ok(name) } else { Err(EXEC_NOT_GRANTED) }
    }
    fn only(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let ExecEntry::Exec { names, name, .. } = self.entry(handle)? else { return Err(reject("invalid exec capability")) };
        let taken = std::mem::take(name);
        let entry = match Self::chosen(names, taken) { Ok(name) => ExecEntry::Exec { names: vec![name], name: Vec::new(), args: Vec::new(), arg: Vec::new(), settings: RunSettings::default() }, Err(code) => ExecEntry::Failed(code) };
        self.issue(entry)
    }
    fn run(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let ExecEntry::Exec { names, name, args, arg, settings } = self.entry(handle)? else { return Err(reject("invalid exec capability")) };
        let (taken, arguments, settings) = (std::mem::take(name), std::mem::take(args), std::mem::take(settings));
        let released = arguments.iter().map(Vec::len).sum::<usize>() + arg.len() + settings.input.len()
            + settings.env.iter().map(|(n, v)| n.len() + v.len()).sum::<usize>() + settings.env_name.len() + settings.env_value.len();
        arg.clear();
        let chosen = Self::chosen(names, taken);
        self.pending = self.pending.saturating_sub(released);
        let entry = match chosen.and_then(|name| if settings.refused != 0 { Err(settings.refused) } else { self.start(&name, &arguments, &settings) }) {
            Ok((out, err, code)) => ExecEntry::Run { out, at: 0, err, err_at: 0, code },
            Err(code) => ExecEntry::Failed(code),
        };
        self.issue(entry)
    }
    /// A setting for the next run: only ever narrower than what the run grants.
    /// One byte of the variable being built (`value` false: its name), or the end of it.
    fn env_byte(&mut self, handle: i64, value: bool, byte: i64) -> wasmtime::Result<i64> {
        if !(0..256).contains(&byte) { return Err(reject("invalid exec capability")); }
        if self.pending >= MAX_PENDING { return Err(reject("exec argument limit exceeded")); }
        let ExecEntry::Exec { settings, .. } = self.entry(handle)? else { return Err(reject("invalid exec capability")) };
        let part = if value { &mut settings.env_value } else { &mut settings.env_name };
        let added = usize::from(part.len() <= if value { 4096 } else { 64 });
        if added == 1 { part.push(byte as u8); }
        self.pending += added;
        Ok(0)
    }
    fn env_end(&mut self, handle: i64) -> wasmtime::Result<i64> {
        let declared = self.env_names.clone();
        let ExecEntry::Exec { settings, .. } = self.entry(handle)? else { return Err(reject("invalid exec capability")) };
        let (name, value) = (std::mem::take(&mut settings.env_name), std::mem::take(&mut settings.env_value));
        if name.len() > 64 || value.len() > 4096 || value.contains(&0) || settings.env.len() >= 16 || settings.env.iter().any(|(n, _)| *n == name) {
            settings.refused = EXEC_INVALID;
        } else if !declared.iter().any(|d| d.as_bytes() == name.as_slice()) {
            settings.refused = EXEC_NOT_GRANTED;
        } else {
            settings.env.push((name, value));
        }
        Ok(0)
    }
    fn narrow(&mut self, handle: i64, deadline_ms: Option<u64>, output_limit: Option<u64>) -> wasmtime::Result<i64> {
        let budget = self.budget;
        let ExecEntry::Exec { settings, .. } = self.entry(handle)? else { return Err(reject("invalid exec capability")) };
        if let Some(ms) = deadline_ms {
            if ms == 0 { settings.refused = EXEC_INVALID; } else if ms > EXEC_RUN_MS { settings.refused = EXEC_WIDER; } else { settings.deadline_ms = Some(ms); }
        }
        if let Some(limit) = output_limit {
            if limit > budget { settings.refused = EXEC_WIDER; } else { settings.output_limit = Some(limit); }
        }
        Ok(0)
    }
    /// Copy the program into a private directory, run it, and keep what it wrote.
    fn start(&mut self, name: &str, args: &[Vec<u8>], settings: &RunSettings) -> Result<(Vec<u8>, Vec<u8>, i64), i64> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::CommandExt;
        if args.len() > MAX_ARGS || args.iter().any(|a| a.contains(&0)) { return Err(EXEC_INVALID); }
        let dir = self.dir.as_ref().ok_or(EXEC_NOT_FOUND)?;
        if name.contains('\0') { return Err(EXEC_INVALID); }
        let mut source = open_entry(dir, name.as_ref()).map_err(|_| EXEC_NOT_FOUND)?;
        let meta = source.metadata().map_err(|_| EXEC_NOT_FOUND)?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 || meta.len() > 256 << 20 { return Err(EXEC_NOT_FOUND); }
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes).map_err(|_| EXEC_FAILED)?;
        // A fresh directory only this user can enter, named so no one can guess it first.
        let mut nonce = [0u8; 8];
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut nonce)).map_err(|_| EXEC_FAILED)?;
        self.runs += 1;
        let private = std::env::temp_dir().join(format!("witchy-exec-{}-{}-{:016x}", std::process::id(), self.runs, u64::from_le_bytes(nonce)));
        std::fs::DirBuilder::new().mode(0o700).create(&private).map_err(|_| EXEC_FAILED)?;
        let result = (|| {
            let program = private.join(name);
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o700).open(&program).and_then(|mut f| f.write_all(&bytes)).map_err(|_| EXEC_FAILED)?;
            let executable = std::env::current_exe().map_err(|_| EXEC_FAILED)?;
            let (gate, worker) = UnixStream::pair().map_err(|_| EXEC_FAILED)?;
            let mut command = std::process::Command::new(executable);
            command.arg(exec_child::WORKER).arg(worker.as_raw_fd().to_string()).arg(&program)
                .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a))).env_clear().current_dir(&private);
            if let Some(cwd) = &settings.cwd {
                // The granted directory by its descriptor: fchdir in the child, after the private
                // directory was entered, before exec.
                let fd = cwd.as_raw_fd();
                unsafe { command.pre_exec(move || if sys::fchdir(fd) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }); }
            }
            command
                .stdin(if settings.input.is_empty() { std::process::Stdio::null() } else { std::process::Stdio::piped() })
                .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let run_for = std::time::Duration::from_millis(settings.deadline_ms.unwrap_or(EXEC_RUN_MS));
            let until = std::time::Instant::now() + run_for;
            let mut child = exec_child::RunningChild::start(command, worker, gate, &settings.env, until)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => EXEC_TIMED_OUT,
                    _ => EXEC_FAILED,
                })?;
            // What is left of the run's budget, or less if the run was narrowed: standard output
            // and error together.
            let limit = (self.budget - self.used).min(settings.output_limit.unwrap_or(u64::MAX));
            let mut stdout = child.child.stdout.take().ok_or(EXEC_FAILED)?;
            let mut stderr = child.child.stderr.take().ok_or(EXEC_FAILED)?;
            if let Some(mut stdin) = child.child.stdin.take() {
                let input = settings.input.clone();
                // Written beside the reads: a program that answers before it has read all its
                // input never blocks the run; one that stops reading gets EPIPE.
                std::thread::spawn(move || { let _ = stdin.write_all(&input); });
            }
            let (sender, received) = std::sync::mpsc::channel();
            let (err_sender, err_received) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut out = Vec::new();
                let read = (&mut stdout).take(limit + 1).read_to_end(&mut out);
                let _ = sender.send((out, read.is_ok()));
            });
            std::thread::spawn(move || {
                let mut err = Vec::new();
                let read = (&mut stderr).take(limit + 1).read_to_end(&mut err);
                let _ = err_sender.send((err, read.is_ok()));
            });
            // The child has its own timeout. The host watchdog stops its group if the
            // enclosing run ends first.
            let exited = loop {
                match child.exited() {
                    Ok(true) => break true,
                    Ok(false) if std::time::Instant::now() < until => std::thread::sleep(std::time::Duration::from_millis(2)),
                    _ => break false,
                }
            };
            // Whatever the program started goes too, so nothing it left holds the output open.
            let status = child.finish().map_err(|_| EXEC_FAILED)?;
            // A descendant that left the group may still hold the output open: stop waiting for it.
            let (out, read) = received.recv_timeout(std::time::Duration::from_millis(200)).map_err(|_| EXEC_FAILED)?;
            let (err, err_read) = err_received.recv_timeout(std::time::Duration::from_millis(200)).map_err(|_| EXEC_FAILED)?;
            // Too much output first: a program past the budget blocks on its pipe and times out.
            let total = out.len() as u64 + err.len() as u64;
            if total > limit { return Err(EXEC_TOO_LARGE); }
            if !exited { return Err(EXEC_TIMED_OUT); }
            if !read || !err_read { return Err(EXEC_FAILED); }
            self.used += total;
            Ok((out, err, status.code().map_or(256, i64::from)))
        })();
        let _ = std::fs::remove_dir_all(&private);
        result
    }
}

/// Whether a variable name is on the refusal list the compiler and the native broker read too
/// (src/compiler/exec_env_refused.txt): one that makes a program load code or configuration from a
/// path the module chooses. `--exec-env=` naming one refuses the run.
fn exec_env_refused(name: &str) -> bool {
    include_str!("../../../src/compiler/exec_env_refused.txt").lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|entry| match entry.strip_suffix('*') { Some(prefix) => name.starts_with(prefix), None => name == entry })
}

/// A range of the program's own memory, given to a bulk operation as `address | length << 32`:
/// the address and length of a checked `&[U8]` or `&mut [U8]`. Only memory 0 is exported (as
/// `memory`), so no operation can reach the allocator's memory; every range is checked
/// against the memory's size here, whatever the module claims.
fn guest_range(caller: &mut Caller<'_, State>, packed: i64) -> wasmtime::Result<(wasmtime::Memory, usize, usize)> {
    let memory = caller.get_export("memory").and_then(|e| e.into_memory()).ok_or_else(|| reject("bulk operation needs the program's memory"))?;
    let (address, length) = ((packed as u64 & 0xffff_ffff) as usize, (packed as u64 >> 32) as usize);
    if address.checked_add(length).is_none_or(|end| end > memory.data_size(&*caller)) { return Err(reject("bulk operation outside the program's memory")); }
    Ok((memory, address, length))
}
// Literal bytes live between the frame stack and the heap. Read-only host
// operations can consume them, but host writes must stay outside that region.
fn check_guest_write(address: usize, length: usize, memory_size: usize) -> wasmtime::Result<()> {
    if length != 0 && address < profile::literal_high(memory_size > 131_072) as usize
        && address + length > profile::STACK_HIGH as usize
    {
        return Err(reject("bulk write overlaps the Wasm literal region"));
    }
    Ok(())
}

#[test]
fn bulk_writes_respect_literal_region_boundaries() {
    let low = profile::STACK_HIGH as usize;
    let high = profile::LITERAL_HIGH as usize;
    assert!(check_guest_write(low - 1, 1, 196_608).is_ok());
    assert!(check_guest_write(high, 1, 196_608).is_ok());
    assert!(check_guest_write(high, 1, 131_072).is_err());
    assert!(check_guest_write(low, 0, 196_608).is_ok());
    assert!(check_guest_write(low - 1, 2, 196_608).is_err());
    assert!(check_guest_write(low, 1, 196_608).is_err());
    assert!(check_guest_write(high - 1, 2, 196_608).is_err());
}

fn read_guest(caller: &mut Caller<'_, State>, packed: i64) -> wasmtime::Result<Vec<u8>> {
    let (memory, address, length) = guest_range(caller, packed)?;
    Ok(memory.data(&*caller)[address..address + length].to_vec())
}
// The result codes are shared by all three public digests. Hash into a local
// value before writing so input and output may be represented by overlapping ranges.
fn register_public_digest<const N: usize>(
    linker: &mut Linker<State>,
    namespace: &str,
    name: &str,
    hash: fn(&[u8]) -> [u8; N],
) -> wasmtime::Result<()> {
    linker.func_wrap(namespace, name, move |mut caller: Caller<'_, State>, data: i64, digest: i64| -> wasmtime::Result<i64> {
        if !caller.data().active { return Err(reject("crypto outside the program")); }
        let (_, _, capacity) = guest_range(&mut caller, digest)?;
        let (memory, at, length) = guest_range(&mut caller, data)?;
        if capacity != N { return Ok(2); }
        if length > MAX_HASHED { return Ok(3); }
        if sha::provider().is_err() { return Ok(4); }
        let hashed = hash(&memory.data(&caller)[at..at + length]);
        write_guest(&mut caller, digest, &hashed)?;
        Ok(0)
    })?;
    Ok(())
}

/// Copy `bytes` to the start of the range, which is at least as long.
fn write_guest(caller: &mut Caller<'_, State>, packed: i64, bytes: &[u8]) -> wasmtime::Result<()> {
    let (memory, address, length) = guest_range(caller, packed)?;
    if bytes.len() > length { return Err(reject("bulk operation outside the program's memory")); }
    check_guest_write(address, bytes.len(), memory.data_size(&*caller))?;
    memory.data_mut(&mut *caller)[address..address + bytes.len()].copy_from_slice(bytes);
    Ok(())
}
/// A limit a run has reached, said once on standard error when it is first hit, and never
/// again in that run: a program told "no room" answers as it sees fit (a server may drop the
/// connection), and without this line nothing says which budget ended the service. The flag is
/// the engine's; a Grimoire manifest sets each as a `[run]` key of the same name.
mod limit {
    use std::sync::atomic::{AtomicU16, Ordering};
    pub const NET: u16 = 1; pub const FILES: u16 = 2; pub const OUTPUT: u16 = 4; pub const FUEL: u16 = 8; pub const DEADLINE: u16 = 16; pub const ENTRIES: u16 = 32; pub const CONNECTION: u16 = 64;
    /// A connection's own budgets (`--…-per-connection`), and the input's.
    pub const NET_EACH: u16 = 128; pub const FILES_EACH: u16 = 256; pub const ENTRIES_EACH: u16 = 512; pub const OUTPUT_EACH: u16 = 1024; pub const INPUT: u16 = 2048;
    static SAID: AtomicU16 = AtomicU16::new(0);
    /// True the first time `which` is reached in this process.
    pub fn first(which: u16) -> bool { SAID.fetch_or(which, Ordering::SeqCst) & which == 0 }
    pub fn reached(which: u16, what: impl FnOnce() -> String) { if first(which) { say!("limit reached: {}", what()); } }
    #[cfg(test)]
    pub fn forget() { SAID.store(0, Ordering::SeqCst); }
    /// `--limit-name=FLAG=TEXT`: what the launcher calls one of the engine's limits where its
    /// operator set it (Grimoire: `--limit-name=--fuel=[run] fuel`). A line that says a limit
    /// was reached then names what the operator can change, not a flag they never wrote.
    /// Only a limit's flag, once each, and a short plain text: it is shown, never interpreted.
    pub const FLAGS: [&str; 13] = ["--fuel", "--fuel-per-accept", "--deadline-ms", "--net-bytes-per-connection", "--fs-bytes-per-connection", "--fs-entries-per-connection", "--output-bytes-per-connection", "--net-bytes", "--fs-bytes", "--fs-entries", "--output-bytes", "--input-bytes", "--connection-deadline-ms"];
    static NAMES: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());
    pub fn parse_name(text: &str) -> Option<(&'static str, String)> {
        let (flag, said) = FLAGS.iter().find_map(|flag| Some((*flag, text.strip_prefix(flag)?.strip_prefix('=')?)))?;
        let plain = !said.is_empty() && said.len() <= 48 && said.bytes().all(|byte| byte.is_ascii_alphanumeric() || b" ._-[]".contains(&byte));
        plain.then(|| (flag, said.to_owned()))
    }
    pub fn name_as(text: &str) -> Result<(), ()> {
        let (flag, said) = parse_name(text).ok_or(())?;
        let mut names = NAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if names.iter().any(|(named, _)| *named == flag) { return Err(()); }
        names.push((flag, said));
        Ok(())
    }
    /// The launcher's name for a limit, or the engine's flag.
    pub fn name(flag: &'static str) -> String {
        NAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().find(|(named, _)| *named == flag).map_or_else(|| flag.to_owned(), |(_, said)| said.clone())
    }
    /// The launcher's name for a limit, if it gave one.
    pub fn named(flag: &'static str) -> Option<String> {
        NAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().find(|(named, _)| *named == flag).map(|(_, said)| said.clone())
    }
    /// What is said when the instruction count runs out: the advice, and the `limit reached`
    /// line. `per_accept`: the count given at each accepted connection, if the run has one.
    /// `name`: a limit's name as above. The count is the store's, one for the whole program,
    /// so running out ends the run and every connection with it, and the line says so: a
    /// program stopped part-way through one request cannot be resumed for the next
    /// (docs/decisions.md, 2026-11-22, "Running out of instructions ends the run").
    pub fn fuel_lines(per_accept: Option<&str>, name: &dyn Fn(&'static str) -> String) -> [String; 2] {
        let (fuel, per) = (name("--fuel"), name("--fuel-per-accept"));
        let named = fuel != "--fuel" || per != "--fuel-per-accept";
        match per_accept {
            Some(grant) => [
                if named { format!("the instruction budget ran out: raise {per} (the work after one accepted connection), or {fuel} (the start, before the first)") }
                else { "the instruction budget ran out: raise it (--fuel-per-accept=N for the work after one accepted connection, --fuel=N for the start)".to_owned() },
                format!("instructions: the program ran out of its count ({fuel} at the start, then {grant} from each accepted connection, {per}); the whole run ends, not only that connection: the count is one for the program, and a program stopped part-way through a request cannot go on to the next"),
            ],
            None => [
                if named { format!("the instruction budget ran out: raise {fuel}") } else { "the instruction budget ran out: raise it (--fuel=N), or run with --unmetered".to_owned() },
                format!("instructions: the program executed its whole count ({fuel}); the run ends"),
            ],
        }
    }
}
/// `--stream-output`: what the program writes goes to standard output as it is written, a
/// line at a time (or 8 KiB of a line that has not ended), still counted against
/// `--output-bytes`. Without the flag output is kept and committed only for a run that
/// finishes. Held here, not in the store, so that the deadline's thread can write what is left.
mod stream {
    use std::io::Write;
    static PENDING: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    const HELD: usize = 8192;
    /// `--plain-output`: what goes out is text a terminal only shows (`plain`).
    pub static PLAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    /// Whether the last byte passed on left a line unfinished.
    static MID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    /// The words the launcher's and the engine's own lines begin with.
    /// How the engine's and the launcher's lines begin, all of them (`said`): two prefixes,
    /// not a list of what they say.
    const OWN: [&[u8]; 2] = [crate::said::PREFIX.trim_ascii_end().as_bytes(), crate::said::LAUNCHER.trim_ascii_end().as_bytes()];
    /// What the program wrote, made safe to show an operator. Every control character but a
    /// newline and a tab (ESC and CR among them), every byte that is not UTF-8, and the
    /// characters that reorder or hide text (bidirectional and line-separator formatting) is
    /// written out as `\xNN` or `\u{NNNN}`: the program cannot move the cursor, recolor, or
    /// rewrite a line already shown. A line that begins as the engine's or the launcher's own
    /// lines all do (`witchy: `, `grimoire: `) is marked `| ` in front, so nothing the program
    /// prints reads as said by either. `mid`: the text continues a
    /// line already begun.
    pub fn plain(bytes: &[u8], mid: &mut bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 8);
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !*mid && OWN.iter().any(|own| line.starts_with(own)) { out.extend_from_slice(b"| "); }
            for chunk in line.utf8_chunks() {
                for ch in chunk.valid().chars() {
                    let code = ch as u32;
                    let hidden = matches!(code, 0x7f..=0x9f | 0x061c | 0x200b..=0x200f | 0x2028..=0x202e | 0x2060..=0x2069 | 0xfeff | 0xfff9..=0xfffb | 0xe0000..=0xe007f);
                    if ch == '\n' || ch == '\t' || (code >= 0x20 && !hidden) { let mut buffer = [0u8; 4]; out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes()); }
                    else if code < 0x100 { out.extend_from_slice(format!("\\x{code:02x}").as_bytes()); }
                    else { out.extend_from_slice(format!("\\u{{{code:04x}}}").as_bytes()); }
                }
                for byte in chunk.invalid() { out.extend_from_slice(format!("\\x{byte:02x}").as_bytes()); }
            }
            *mid = !line.ends_with(b"\n");
        }
        out
    }
    /// An overlong line is cut between characters, not inside one.
    pub fn whole(pending: &[u8], count: usize) -> usize {
        if count == 0 || pending[count - 1] == b'\n' { return count; }
        let start = (count.saturating_sub(3)..count).rev().find(|at| pending[*at] & 0xc0 != 0x80).unwrap_or(count);
        let needs = match pending.get(start) { Some(lead) if *lead >= 0xf0 => 4, Some(lead) if *lead >= 0xe0 => 3, Some(lead) if *lead >= 0xc0 => 2, _ => 1 };
        if start < count && start + needs > count { start } else { count }
    }
    fn pass(out: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        if !PLAIN.load(Ordering::SeqCst) { return out.write_all(bytes); }
        let mut mid = MID.load(Ordering::SeqCst);
        let shown = plain(bytes, &mut mid);
        MID.store(mid, Ordering::SeqCst);
        out.write_all(&shown)
    }
    /// How much of `pending` goes out now: through its last newline, or all of an overlong line.
    pub fn ready(pending: &[u8]) -> usize {
        match pending.iter().rposition(|byte| *byte == b'\n') { Some(at) => at + 1, None if pending.len() >= HELD => pending.len(), None => 0 }
    }
    pub fn write(bytes: &[u8]) -> std::io::Result<()> {
        let mut pending = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.extend_from_slice(bytes);
        let count = whole(&pending, ready(&pending));
        if count == 0 { return Ok(()); }
        let mut out = std::io::stdout().lock();
        pass(&mut out, &pending[..count])?;
        out.flush()?;
        pending.drain(..count);
        Ok(())
    }
    /// Whole lines of one instance among several (`State::lines`), in one write that no other
    /// instance's lines come into.
    pub fn lines(bytes: &[u8]) -> std::io::Result<()> {
        let _held = PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out = std::io::stdout().lock();
        pass(&mut out, bytes)?;
        out.flush()
    }
    /// The rest (a last line with no newline), when the run ends however it ends. From the
    /// deadline's thread this never waits for a writer stopped on a full pipe.
    pub fn finish(wait: bool) {
        let pending = if wait { Some(PENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())) } else { PENDING.try_lock().ok() };
        // Nothing held back: standard output is not touched (its lock may be held by a
        // writer stopped on a full pipe).
        if let Some(mut pending) = pending.filter(|pending| !pending.is_empty()) {
            let mut out = std::io::stdout().lock();
            let _ = pass(&mut out, &pending);
            let _ = out.flush();
            pending.clear();
        }
    }
}
/// `--fuel-per-accept`: the count starts again from the grant when a connection is accepted,
/// and when a blocking accept has waited its whole two seconds for one (an idle service spends
/// a little each time round its loop, and would otherwise run dry with no one connected; at
/// most one grant in two seconds comes that way). An accept that does not wait (`try_accept`
/// with nothing there) gives nothing, or a loop of them would be a count without end. Never
/// added to what is left: unused fuel is not saved up.
fn refill_on_accept(caller: &mut Caller<'_, State>, handle: i64) -> wasmtime::Result<()> {
    let Some(grant) = caller.data().refill else {
        // No count to start again: what the run has used so far is still copied for the
        // `limit: used:` line, so a service stopped from outside can say it.
        if refills(caller.data_mut().network.entry(handle)?) { usage::copy(caller.data(), false); }
        return Ok(());
    };
    if refills(caller.data_mut().network.entry(handle)?) {
        // What the interval that ends here used, for the `limit: used:` line.
        let left = caller.get_fuel()?;
        let state = caller.data_mut();
        usage::interval(!state.interval_started, state.interval.saturating_sub(left));
        (state.interval, state.interval_started) = (grant, true);
        usage::copy(state, false);
        caller.set_fuel(grant)?;
    }
    Ok(())
}
fn refills(entry: &NetEntry) -> bool { matches!(entry, NetEntry::Socket { .. } | NetEntry::Failed(TIMED_OUT)) }
/// What the program writes to its console: within the output budget, kept for the end of the
/// run or, streamed, passed on now.
fn emit(caller: &mut Caller<'_, State>, bytes: &[u8]) -> wasmtime::Result<()> {
    let state = caller.data_mut();
    let used = state.streamed.unwrap_or(state.output.len());
    if used.saturating_add(bytes.len()) > state.output_limit {
        let granted = state.output_limit;
        match state.output_share {
            Share::Run => limit::reached(limit::OUTPUT, || format!("output bytes: the program has written its {granted} granted bytes ({}); the run ends", limit::name("--output-bytes"))),
            Share::Connection => limit::reached(limit::OUTPUT_EACH, || format!("output bytes: a connection's instance has written its {granted} bytes ({}); that connection is closed and the server goes on", limit::name("--output-bytes-per-connection"))),
            Share::Rest(ceiling) => limit::reached(limit::OUTPUT, || format!("output bytes: the run has written its {ceiling} granted bytes ({}); an instance that writes more is stopped", limit::name("--output-bytes"))),
        }
        return Err(reject("output budget exceeded"));
    }
    match (&mut state.streamed, &mut state.lines) {
        (None, _) => state.output.extend_from_slice(bytes),
        (Some(count), None) => { *count += bytes.len(); stream::write(bytes).map_err(|_| reject("standard output is closed"))?; }
        (Some(count), Some(line)) => {
            *count += bytes.len();
            line.extend_from_slice(bytes);
            let ready = stream::whole(line, stream::ready(line));
            if ready > 0 { stream::lines(&line[..ready]).map_err(|_| reject("standard output is closed"))?; line.drain(..ready); }
        }
    }
    Ok(())
}
/// The most a byte budget can be set to (`--net-bytes`, `--fs-bytes`, `--output-bytes`): 1 TiB.
/// Each is a count of bytes that cross, not memory held (one file or message is still bounded
/// by what a module's memory holds), except output that is not streamed, which is kept: that
/// stays at 256 MiB.
const BYTE_BUDGET_MAX: u64 = 1 << 40;
/// What `--input-stdin` stores where `--input=FILE` stores a path. Not an absolute path, so no
/// `--input=` can spell it.
const STANDARD_INPUT: &str = "standard input";
const USAGE: &str = "usage: witchy-wasmtime-host (MODULE.wasm | --module-fd=N) [--output] [--allocator] [--fuel=N] [--memory-pages=N] [--module-bytes=N] [--module-pin=sha384:HEX] [--deadline-ms=N] [--input=FILE] [--dir=DIR] [--env=NAME[=VALUE]] ...";
fn reject(message: &str) -> wasmtime::Error { wasmtime::Error::msg(message.to_owned()) }
/// The module as read, for saying why a trap stopped it (`trap_reason`).
static MODULE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// The check that stopped the run, when the compiler wrote which just before the `unreachable`
/// at `offset` (`i32.const why; drop; unreachable`, src/compiler/emit.rs `trap_for`).
fn trap_reason(offset: usize) -> Option<std::borrow::Cow<'static, str>> {
    use std::borrow::Cow;

    let bytes = MODULE.get()?;
    if offset < 3 || bytes.get(offset) != Some(&0x00) || bytes[offset - 3] != 0x41 || bytes[offset - 1] != 0x1a { return None; }
    let reason = match bytes[offset - 2] {
        1 => "an overflow in `+`",
        2 => "`-` went below zero",
        3 => "an overflow in `*`",
        4 => "an I64 overflow",
        5 => "a value too large for its type",
        6 => "an index out of range",
        7 => "a shift by the width or more",
        8 => "a slice range out of order or past the end",
        10 => "a substring endpoint lies inside a UTF-8 scalar",
        // A trap from a function the compiler wrote into its caller (src/compiler/inline.rs): before the
        // mark, `i32.const` of that function's index in five bytes, then `drop`.
        9 => {
            if offset < 10 || bytes[offset - 10] != 0x41 || bytes[offset - 4] != 0x1a { return None; }
            let index = bytes[offset - 9..offset - 4].iter().rev().fold(0u32, |value, byte| (value << 7) | u32::from(byte & 0x7f));
            let name = function_name(bytes, index).unwrap_or_else(|| format!("function {index}"));
            return Some(Cow::Owned(format!("an `unreachable` reached in `{name}`, which the compiler wrote into this function")));
        }
        _ => return None,
    };
    Some(Cow::Borrowed(reason))
}

/// A function's name from the module's `name` section.
fn function_name(bytes: &[u8], index: u32) -> Option<String> {
    use wasmtime::wasmparser::{KnownCustom, Name, Parser, Payload};
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(Payload::CustomSection(section)) = payload else { continue };
        let KnownCustom::Name(reader) = section.as_known() else { continue };
        for name in reader {
            let Ok(Name::Function(map)) = name else { continue };
            for naming in map { if let Ok(naming) = naming && naming.index == index { return Some(naming.name.to_string()); } }
        }
    }
    None
}

/// The engine settings for a run: what the run's flags and the module's memory decide, and
/// nothing else. `resident`: a runner that forks (`resident.rs`) handles traps with signals,
/// which a forked process inherits (Wasmtime's Mach ports do not survive `fork`), and compiles
/// on one thread, so the process it forks from never has another.
fn engine_config(unmetered: bool, reservation: u64, resident: bool) -> Config {
    let mut config = Config::new();
    #[cfg(feature = "guest-profile")]
    config.epoch_interruption(profile_guest::wanted());
    config.consume_fuel(!unmetered).max_wasm_stack(1024 * 1024)
        .memory_reservation(reservation).memory_reservation_for_growth(0)
        .wasm_memory64(false).wasm_multi_memory(true).wasm_simd(true)
        .wasm_bulk_memory(false)
        .wasm_relaxed_simd(false).wasm_tail_call(false);
    if resident { config.macos_use_mach_ports(false).parallel_compilation(false); }
    config
}
/// Compiling for one run: a new engine and module from the exact bytes, every time.
fn compile_fresh(unmetered: bool, reservation: u64, bytes: &[u8]) -> wasmtime::Result<(Engine, Module)> {
    let engine = Engine::new(&engine_config(unmetered, reservation, false))?;
    let module = Module::new(&engine, bytes)?;
    Ok((engine, module))
}
/// `args`: the command line, the program's name first. `compile`: how the admitted bytes become
/// a module (`compile_fresh`, or the resident runner's, which may reuse one compiled from the
/// same bytes with the same settings).
type Compile<'a> = &'a mut dyn FnMut(bool, u64, &[u8]) -> wasmtime::Result<(Engine, Module)>;
fn execute(args: &[String], variables: std::collections::BTreeMap<String, Vec<u8>>, compile: Compile<'_>, mut test: Option<test_batch::Invocation<'_>>) -> wasmtime::Result<Vec<u8>> {
    // `--time`: startup (reading, admission, engine compilation, instantiation) and the run, apart.
    let started = std::time::Instant::now();
    let mut timed = false;
    let mut sqlite_path = None;
    let mut sqlite_reply_bytes = None;
    let mut sqlite_rights = sqlite::Rights::default();
    let mut local_service_fd = None;
    let mut local_service_reply = None;
    let mut output = false;
    let mut allocator = false;
    let mut fuel = false;
    // The wall clock and the host's randomness, each granted on its own.
    let (mut clock, mut rand) = (false, false);
    // The endpoints a program may dial, and the bytes it may send and receive.
    let (mut net, mut net_bytes) = (None::<Vec<String>>, 1_048_576u64);
    let mut listen = None::<(String, String)>;
    // `--secret=NAME` and `--secret-seal=NAME`: the secrets a run may get, and how.
    let mut secrets = Secrets { variables, ..Secrets::default() };
    // `--secret-object=SLOT:DEV:INO`, checked once every flag is read (a slot's flag may come later).
    let mut secret_objects: Vec<(String, (u64, u64))> = Vec::new();
    // `--grant=Type.field=VALUE`: the policy values of the program's grantable capabilities.
    // Data, never authority: a grantable capability holds no host capability (the compiler
    // checks), so these widen nothing a program can touch.
    let mut grants = std::collections::BTreeMap::<Vec<u8>, i64>::new();
    // `--exec=DIR:name,name`: the programs a run may start, and the directory they are in.
    let (mut exec, mut exec_bytes) = (None::<(String, Vec<String>)>, 1_048_576u64);
    let mut exec_env = None::<Vec<String>>;
    // The environment a run may read: only the names and values the caller grants, which it
    // places under WITCHY_GRANT_ in this process's otherwise empty environment.
    let (mut env, mut variables) = (false, std::collections::BTreeMap::<Vec<u8>, Vec<u8>>::new());
    let mut granted_env_names = std::collections::BTreeSet::<String>::new();
    // No instruction count at all: the run is bounded by the deadline, the memory limit and the
    // stack limit only. Counting costs about a quarter of the run time.
    let (mut unmetered, mut budgeted) = (false, false);
    // `--partial-output`: on a trap, what the program wrote goes to stderr (for finding where it
    // trapped); standard output still gets nothing.
    let mut partial = false;
    // `--stream-output`: standard output as it is written (`stream`), not committed at the end.
    let mut stream = false;
    // `--plain-output`: with `--stream-output`, what is passed on is made safe to show
    // (`stream::plain`). The count against `--output-bytes` is of what the program wrote.
    let mut plain = false;
    // `--fuel-per-accept=N`: the instruction count is set back to N each time the program
    // accepts a connection, so N bounds the work between one accepted connection and the next
    // (a request, for a server that answers one at a time) and the service as a whole is
    // bounded by its deadline and byte budgets, not by a count that every request eats into.
    let mut fuel_per_accept = None::<u64>;
    // `--instance-per-connection`: the engine accepts, and runs the program once for each
    // connection in a fresh instance (`serve_connections`). `--connection-deadline-ms=N`: how
    // long one connection's instance may run. `--connections=N`: the run ends, well, after N.
    let (mut per_connection, mut connection_ms, mut connections) = (false, None::<u64>, None::<u64>);
    // Each connection's own budgets (`--net-bytes-per-connection=N` and the like, in the order
    // network bytes, file bytes, names made, output bytes): what one instance may use, beside
    // the whole run's, which may then be `unlimited`. `--waiting-connections=N`: accepted
    // connections the engine holds while the instance is busy. (One instance at a time.)
    let mut own = [None::<u64>; 4];
    let mut waiting = None::<u64>;
    let mut input_unlimited = false;
    // `--args=COUNT:BYTES`: the program is granted the launch's words, at most so many and so
    // many bytes in all. `--arg=WORD`: one word, in order; the rest of the flag is the word,
    // whatever it holds, and is only ever handed to the program as bytes.
    let (mut args_grant, mut words) = (None::<(u64, u64)>, Vec::<Vec<u8>>::new());
    // The file a program may take bytes from, and how large it may be. No file: no input import.
    let (mut input, mut input_bytes) = (None::<String>, 65_536u64);
    let mut sqlite_request = false;
    // `--input-id=DEVICE:INODE`: the input file the launcher examined (`open_input`).
    let mut input_id = None::<(u64, u64)>;
    // The directory, its rights (read unless said otherwise), and the bytes a run may read and write there.
    let (mut dir, mut dir_rights, mut fs_bytes) = (None::<std::path::PathBuf>, None, 1_048_576u64);
    let mut alias_specs = Vec::<(Vec<u8>, u32)>::new();
    let mut fs_entries = 256u64;
    // `--data-only` (`Files::data_only`), and `--dir-id=DEVICE:INODE`: the granted directory
    // must be the very one the launcher examined, or nothing runs.
    let (mut data_only, mut dir_id) = (false, None::<(u64, u64)>);
    // `--output-bytes=N`: how much the program may write to the console (kept until it finishes).
    let mut output_bytes = 65_536u64;
    // A separate directory for the write right: a program may read one tree and write another.
    let mut write_dir = None::<std::path::PathBuf>;
    // One file to read and one to write, handed to `main` as `File<Read>` and `File<Write>`.
    let (mut read_file, mut write_file) = (None::<std::path::PathBuf>, None::<std::path::PathBuf>);
    // What the caller grants beyond the defaults: instructions, pages over both memories, module size.
    let (mut budget, mut pages, mut module_bytes) = (1_000_000u64, 4u64, 65_536u64);
    // The module to run: a path or a regular file inherited from the launcher.
    let mut module_path = None::<std::path::PathBuf>;
    let mut module_fd = None;
    let mut module_pin = None;
    let mut ready_stdout = false;
    let number = |text: &str, low: u64, high: u64| text.parse::<u64>().ok().filter(|n| (low..=high).contains(n)).ok_or_else(|| reject("invalid grant"));
    // A whole run's budget: a number, or `unlimited` (`u64::MAX`; allowed only beside a
    // connection's own budget, checked once every flag is read).
    let ceiling = |text: &str, high: u64| if text == "unlimited" { Ok(u64::MAX) } else { number(text, 0, high) };
    let once = |slot: &mut Option<u64>, value: u64| if slot.replace(value).is_some() { Err(reject("invalid grant")) } else { Ok(()) };
    // `--no-links`: every granted directory, and each granted file's directory, is opened by its
    // path as given, with no symbolic link anywhere on it (not resolved first, then opened, when
    // a link could be swapped in between). Read before the other flags, which it changes.
    let no_links = args.iter().skip(1).any(|arg| arg == "--no-links");
    let granted_dir = |path: &str| -> wasmtime::Result<std::path::PathBuf> {
        if no_links { return Ok(std::path::PathBuf::from(path)); }
        let dir = std::fs::canonicalize(path).map_err(|_| reject("granted directory cannot be opened"))?;
        if !dir.is_dir() { return Err(reject("granted directory is not a directory")); }
        Ok(dir)
    };
    for arg in args.iter().skip(1).cloned() {
        // A refused flag is named in the message, with the usual reason when it is one.
        let mut step = || -> wasmtime::Result<()> { match arg.as_str() {
            "--output" if !output => output = true,
            "--allocator" if !allocator => allocator = true,
            // Measurement only: report the instructions the run used on stderr. The budget is unchanged.
            "--fuel" if !fuel => fuel = true,
            "--time" if !timed => timed = true,
            "--unmetered" if !unmetered => unmetered = true,
            "--partial-output" if !partial => partial = true,
            "--stream-output" if !stream => stream = true,
            "--plain-output" if !plain => plain = true,
            "--data-only" if !data_only => data_only = true,
            "--usage" => usage::ASKED.store(true, std::sync::atomic::Ordering::Relaxed),
            "--instance-per-connection" if !per_connection => per_connection = true,
            "--listener-ready-stdout" if !ready_stdout => ready_stdout = true,
            "--clock" if !clock => clock = true,
            "--rand" if !rand => rand = true,
            "--no-links" => {}
            other if !other.starts_with("--") => {
                if module_path.is_some() { return Err(reject(USAGE)); }
                module_path = Some(std::path::PathBuf::from(other));
            }
            other => {
                // `--fuel=N` raises the instruction budget (`--budget=N` is the same); bare
                // `--fuel` reports what the run used.
                if let Some(text) = other.strip_prefix("--budget=").or_else(|| other.strip_prefix("--fuel=")) {
                    if budgeted { return Err(reject("invalid grant")); }
                    budget = number(text, 1, 1_000_000_000_000)?; budgeted = true;
                }
                // `--env=NAME=VALUE` grants one variable; `--env=NAME` passes on this process's own
                // value, if it has one (granted as absent if not). Nothing else in the
                // environment is read.
                else if let Some(text) = other.strip_prefix("--env=") {
                    use std::os::unix::ffi::OsStrExt;
                    let (name, given) = match text.split_once('=') { Some((n, v)) => (n, Some(v.as_bytes().to_vec())), None => (text, None) };
                    let plain = name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
                    if !plain || variables.len() >= 256 || !granted_env_names.insert(name.to_owned()) { return Err(reject("invalid grant")); }
                    env = true;
                    if let Some(value) = given.or_else(|| std::env::var_os(name).map(|v| v.as_bytes().to_vec())) {
                        if value.len() > 65_536 { return Err(reject("invalid grant")); }
                        variables.insert(name.as_bytes().to_vec(), value);
                    }
                }
                else if let Some(text) = other.strip_prefix("--memory-pages=") { pages = number(text, 4, 32_768)?; }
                else if let Some(text) = other.strip_prefix("--module-bytes=") { module_bytes = number(text, 8, 16_777_216)?; }
                else if let Some(text) = other.strip_prefix("--module-fd=") {
                    once(&mut module_fd, number(text, 3, i32::MAX as u64)?)?;
                }
                else if let Some(text) = other.strip_prefix("--module-pin=") {
                    let hex = text.strip_prefix("sha384:").filter(|hex| hex.len() == 96 &&
                        hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
                    let Some(hex) = hex else { return Err(reject("--module-pin requires a sha384: pin with 96 lowercase hex digits")); };
                    if module_pin.replace(hex.to_owned()).is_some() { return Err(reject("--module-pin given twice")); }
                }
                else if other.strip_prefix("--deadline-ms=").is_some() || other.strip_prefix("--lifeline=").is_some() { /* read by main */ }
                else if let Some(text) = other.strip_prefix("--limit-name=") { limit::name_as(text).map_err(|()| reject("invalid grant"))?; }
                else if let Some(text) = other.strip_prefix("--fuel-per-accept=") {
                    if fuel_per_accept.is_some() { return Err(reject("invalid grant")); }
                    fuel_per_accept = Some(number(text, 1, 1_000_000_000_000)?);
                }
                else if let Some(word) = other.strip_prefix("--arg=") { words.push(word.as_bytes().to_vec()); }
                else if let Some(text) = other.strip_prefix("--args=") {
                    let (count, bytes) = text.split_once(':').ok_or_else(|| reject("invalid grant"))?;
                    if args_grant.is_some() { return Err(reject("invalid grant")); }
                    args_grant = Some((number(count, 0, 256)?, number(bytes, 0, 65_536)?));
                }
                else if let Some(text) = other.strip_prefix("--connection-deadline-ms=") {
                    if connection_ms.is_some() { return Err(reject("invalid grant")); }
                    connection_ms = Some(number(text, 1, 600_000)?);
                }
                else if let Some(text) = other.strip_prefix("--connections=") {
                    if connections.is_some() { return Err(reject("invalid grant")); }
                    connections = Some(number(text, 1, 1_000_000_000_000)?);
                }
                else if let Some(text) = other.strip_prefix("--input-bytes=") {
                    // More than 16 MiB, or `unlimited`: standard input only (checked below).
                    if text == "unlimited" { input_unlimited = true; } else { input_unlimited = false; input_bytes = number(text, 0, BYTE_BUDGET_MAX)?; }
                }
                else if let Some(text) = other.strip_prefix("--net-bytes-per-connection=") { once(&mut own[0], number(text, 0, BYTE_BUDGET_MAX)?)?; }
                else if let Some(text) = other.strip_prefix("--fs-bytes-per-connection=") { once(&mut own[1], number(text, 0, BYTE_BUDGET_MAX)?)?; }
                else if let Some(text) = other.strip_prefix("--fs-entries-per-connection=") { once(&mut own[2], number(text, 0, 1_000_000)?)?; }
                else if let Some(text) = other.strip_prefix("--output-bytes-per-connection=") { once(&mut own[3], number(text, 0, 268_435_456)?)?; }
                else if let Some(text) = other.strip_prefix("--waiting-connections=") { once(&mut waiting, number(text, 1, 4096)?)?; }
                // The directory a run may reach, and the rights it may hand to `main`.
                else if let Some(path) = other.strip_prefix("--dir=") {
                    if dir.is_some() || !std::path::Path::new(path).is_absolute() { return Err(reject("invalid grant")); }
                    dir = Some(granted_dir(path)?);
                }
                else if let Some(spec) = other.strip_prefix("--read-dir-alias=") {
                    let (name, fd) = spec.split_once(":fd:").ok_or_else(|| reject("invalid grant"))?;
                    let fd = number(fd, 3, i32::MAX as u64)? as u32;
                    if name.is_empty() || name.len() > 64
                        || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        || alias_specs.len() >= 16 || alias_specs.iter().any(|(prior, number)| prior == name.as_bytes() || *number == fd) {
                        return Err(reject("invalid grant"));
                    }
                    alias_specs.push((name.as_bytes().to_vec(), fd));
                }
                else if let Some(path) = other.strip_prefix("--write-dir=") {
                    if write_dir.is_some() || !std::path::Path::new(path).is_absolute() { return Err(reject("invalid grant")); }
                    write_dir = Some(granted_dir(path)?);
                }
                else if let Some(path) = other.strip_prefix("--sqlite=") {
                    if sqlite_path.replace(path.to_owned()).is_some() { return Err(reject("duplicate SQLite grant")); }
                }
                else if let Some(text) = other.strip_prefix("--local-service-fd=") {
                    once(&mut local_service_fd, number(text, 3, 1023)?)?;
                }
                else if let Some(text) = other.strip_prefix("--local-service-reply-bytes=") {
                    once(&mut local_service_reply, number(text, 1, local_service::MAX_REPLY_LIMIT as u64)?)?;
                }
                else if let Some(text) = other.strip_prefix("--sqlite-reply-bytes=") {
                    once(&mut sqlite_reply_bytes, number(text, 1, sqlite::MAX_REPLY_BYTES as u64)?)?;
                }
                else if let Some((verb, tables)) = other.strip_prefix("--sqlite-").and_then(|text| text.split_once('='))
                    && matches!(verb, "read" | "insert" | "delete" | "create") {
                    sqlite_rights.add(verb, tables)?;
                }
                else if let Some(path) = other.strip_prefix("--file=") {
                    if read_file.is_some() || !std::path::Path::new(path).is_absolute() { return Err(reject("invalid grant")); }
                    read_file = Some(std::path::PathBuf::from(path));
                }
                else if let Some(path) = other.strip_prefix("--write-file=") {
                    if write_file.is_some() || !std::path::Path::new(path).is_absolute() { return Err(reject("invalid grant")); }
                    write_file = Some(std::path::PathBuf::from(path));
                }
                else if let Some(text) = other.strip_prefix("--dir-rights=") {
                    if dir_rights.is_some() { return Err(reject("invalid grant")); }
                    dir_rights = Some(match text { "read" => (true, false), "write" => (false, true), "read,write" => (true, true), _ => return Err(reject("invalid grant")) });
                }
                else if let Some(text) = other.strip_prefix("--fs-bytes=") { fs_bytes = ceiling(text, BYTE_BUDGET_MAX)?; }
                else if let Some(text) = other.strip_prefix("--dir-id=") {
                    dir_id = Some(parse_dir_id(text, dir_id)?);
                }
                else if let Some(text) = other.strip_prefix("--fs-entries=") { fs_entries = ceiling(text, 1_000_000)?; }
                else if let Some(text) = other.strip_prefix("--output-bytes=") { output_bytes = ceiling(text, BYTE_BUDGET_MAX)?; }
                else if let Some(text) = other.strip_prefix("--net=") {
                    let endpoints: Vec<String> = text.split(',').map(str::to_owned).collect();
                    if net.is_some() || endpoints.len() > 64 || endpoints.iter().any(|e| e.is_empty() || e.len() > 256 || e.rsplit_once(':').is_none_or(|(h, p)| h.is_empty() || p.parse::<u16>().is_err())) { return Err(reject("invalid grant")); }
                    net = Some(endpoints);
                }
                else if let Some(text) = other.strip_prefix("--exec-env=") {
                    let names: Vec<String> = text.split(',').map(str::to_owned).collect();
                    let plain = |n: &str| !n.is_empty() && n.len() <= 64 && !n.as_bytes()[0].is_ascii_digit() && n.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') && !exec_env_refused(n);
                    if exec_env.is_some() || names.len() > 16 || !names.iter().all(|n| plain(n)) || (1..names.len()).any(|i| names[..i].contains(&names[i])) { return Err(reject("invalid grant")); }
                    exec_env = Some(names);
                }
                else if let Some(text) = other.strip_prefix("--exec=") {
                    let Some((dir, names)) = text.rsplit_once(':') else { return Err(reject("invalid grant")) };
                    let names: Vec<String> = names.split(',').map(str::to_owned).collect();
                    if exec.is_some() || !std::path::Path::new(dir).is_absolute() || names.len() > 64 || !names.iter().all(|n| Programs::plain(n)) { return Err(reject("invalid grant")); }
                    exec = Some((dir.to_owned(), names));
                }
                else if let Some((text, reveal)) = other.strip_prefix("--secret=").map(|n| (n, true)).or_else(|| other.strip_prefix("--secret-seal=").map(|n| (n, false))) {
                    // `--secret=NAME` grants the secret NAME (read and sign), `--secret-seal=NAME`
                    // signing only. The value is this process's variable NAME, or, for tests,
                    // `--secret=NAME=VALUE` (then on the command line, which other users can see).
                    // `--secret=NAME@fd:N`: the value is everything descriptor N (one this process
                    // was started with) holds, read with the fixed secrets before `main` (a
                    // launcher hands it over on a pipe of its own: Grimoire).
                    if let Some((name, at)) = text.split_once("@fd:").filter(|(name, at)| !name.contains('=') && !at.contains('=')) {
                        if !Secrets::plain(name) || secrets.granted.len() >= 64 || secrets.granted.contains_key(name.as_bytes()) { return Err(reject("invalid grant")); }
                        let number = at.parse::<u32>().ok().filter(|n| (3..=1023).contains(n) && !at.starts_with('0') && !at.starts_with('+')).ok_or_else(|| reject("invalid grant"))?;
                        if secrets.descriptor_selected(number) { return Err(reject(&format!("secret {name}: source fd:{number} selected twice"))); }
                        let fd = sys::take_descriptor(number).map_err(|_| reject(&format!("secret {name}: unreadable descriptor (fd:{number})")))?;
                        use std::os::unix::fs::MetadataExt;
                        let meta = fd.try_clone().and_then(|copy| std::fs::File::from(copy).metadata()).map_err(|_| reject(&format!("secret {name}: unreadable descriptor (fd:{number})")))?;
                        // One source, one route, with the fixed secrets' descriptors too.
                        if secrets.objects.contains(&(meta.dev(), meta.ino())) { return Err(reject(&format!("secret {name}: source fd:{number} is the same object as another secret's source"))); }
                        secrets.objects.push((meta.dev(), meta.ino()));
                        secrets.object_slots.push(format!("store secret {name}"));
                        secrets.store_sources.push(StoreSource { name: name.as_bytes().to_vec(), source: Some(fd), number });
                        secrets.granted.insert(name.as_bytes().to_vec(), (Vec::new(), reveal));
                        return Ok(());
                    }
                    let (name, given) = match text.split_once('=') { Some((n, v)) => (n, Some(v.as_bytes().to_vec())), None => (text, None) };
                    if !Secrets::plain(name) || secrets.granted.len() >= 64 || secrets.granted.contains_key(name.as_bytes()) { return Err(reject("invalid grant")); }
                    let value = given.or_else(|| secrets.variables.get(name).cloned()).ok_or_else(|| reject(&format!("no value for secret {name}")))?;
                    if value.len() > 65_536 { return Err(reject("invalid grant")); }
                    secrets.granted.insert(name.as_bytes().to_vec(), (value, reveal));
                }
                // `--secret-slot=NAME:USE:SOURCE` (wasm-grants writes it): `main`'s fixed secret
                // NAME, its use, and where its bytes come from. Never a value.
                else if let Some(text) = other.strip_prefix("--secret-slot=") {
                    let mut parts = text.splitn(4, ':');
                    let (Some(name), Some(use_), Some(source_kind), Some(at)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err(reject("invalid grant")) };
                    let application = mldsa_signer::use_domain(use_);
                    let (kind, role) = match use_ {
                        "reveal-bytes" => (0, ""), "seal" => (1, ""), "sign/tls13/server-p384" => (3, ""),
                        // The witchy-1 suite (ML-DSA-44) is retired: no signer of it is made, so no
                        // object of it is signed by an engine of this profile.
                        _ if use_.starts_with("sign/witchy-1/") => return Err(reject("invalid grant: the witchy-1 suite is retired; a Signer's use is sign/witchy-2/ROLE (ML-DSA-65)")),
                        _ if let Some(domain) = &application => (4, domain.as_str()),
                        _ => match use_.strip_prefix("sign/witchy-2/") { Some(role) if plain_role(role) => (2, role), _ => return Err(reject("invalid grant")) },
                    };
                    if !Secrets::plain(name) || secrets.slots.len() >= 12 { return Err(reject("invalid grant")); }
                    if secrets.slots.iter().any(|s| s.name == name) { return Err(reject(&format!("secret slot {name}: selected twice"))); }
                    let source = match source_kind {
                        // A signer's seed comes from a descriptor only (review of the witchy-2 cutover).
                        "env" if kind == 3 => return Err(reject(&format!("secret slot {name}: a TlsSigner key comes from fd:N"))),
                        "env" if kind == 2 => return Err(reject(&format!("secret slot {name}: a Signer's seed comes from fd:N, never a variable"))),
                        "env" if kind == 4 => return Err(reject(&format!("secret slot {name}: an Mldsa87Signer seed comes from fd:N"))),
                        "env" if at.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && Secrets::plain(at) => SecretSource::Env(at.to_owned()),
                        "fd" => {
                            let number = at.parse::<u32>().ok().filter(|n| (3..=1023).contains(n) && !at.starts_with('0') && !at.starts_with('+')).ok_or_else(|| reject("invalid grant"))?;
                            if secrets.descriptor_selected(number) { return Err(reject(&format!("secret slot {name}: source fd:{number} selected twice"))); }
                            let fd = sys::take_descriptor(number).map_err(|_| reject(&format!("secret slot {name}: unreadable descriptor (fd:{number})")))?;
                            use std::os::unix::fs::MetadataExt;
                            let meta = fd.try_clone().and_then(|copy| std::fs::File::from(copy).metadata()).map_err(|_| reject(&format!("secret slot {name}: unreadable descriptor (fd:{number})")))?;
                            // One source, one route: another slot's descriptor that is the same
                            // file or pipe (the file opened twice, or a duplicate) would read one
                            // secret through two slots, a sealed key through a reveal slot.
                            if secrets.objects.contains(&(meta.dev(), meta.ino())) { return Err(reject(&format!("secret slot {name}: source fd:{number} is the same object as another slot's source"))); }
                            secrets.objects.push((meta.dev(), meta.ino()));
                            secrets.object_slots.push(format!("slot {name}"));
                            SecretSource::Fd(fd, number)
                        }
                        _ => return Err(reject("invalid grant")),
                    };
                    secrets.slots.push(SecretSlot { name: name.to_owned(), kind, role: role.to_owned(), source: Some(source), label: format!("{source_kind}:{at}"), value: Vec::new(), slot: 0, taken: false, signer: None });
                }
                // `--secret-object=SLOT:DEV:INO`: the device and inode of the file a launcher read
                // slot SLOT's value from before handing it over on a pipe (Grimoire). Never a
                // value; it only adds an object no grant may reach (see after the flags).
                else if let Some(text) = other.strip_prefix("--secret-object=") {
                    let mut parts = text.splitn(3, ':');
                    let (Some(name), Some(dev), Some(ino)) = (parts.next(), parts.next(), parts.next()) else { return Err(reject("invalid grant")) };
                    let number = |text: &str| text.parse::<u64>().ok().filter(|_| !text.starts_with('+') && (text == "0" || !text.starts_with('0')));
                    let (Some(dev), Some(ino)) = (number(dev), number(ino)) else { return Err(reject("invalid grant")) };
                    if !Secrets::plain(name) || secret_objects.len() >= 12 { return Err(reject("invalid grant")); }
                    if secret_objects.iter().any(|(slot, _)| slot == name) { return Err(reject(&format!("secret slot {name}: --secret-object given twice"))); }
                    secret_objects.push((name.to_owned(), (dev, ino)));
                }
                else if let Some(text) = other.strip_prefix("--grant=") {
                    let Some((name, value)) = text.split_once('=') else { return Err(reject("invalid grant")) };
                    let plain = name.split_once('.').is_some_and(|(t, f)| !t.is_empty() && !f.is_empty() && !f.contains('.') && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'));
                    let value = match value { "true" => 1, "false" => 0, number => number.parse::<u64>().map_err(|_| reject("invalid grant"))? as i64 };
                    if !plain || grants.len() >= 64 || grants.insert(name.as_bytes().to_vec(), value).is_some() { return Err(reject("invalid grant")); }
                }
                else if let Some(text) = other.strip_prefix("--exec-bytes=") { exec_bytes = number(text, 0, 268_435_456)?; }
                // `--listen=HOST:PORT`: the one address the program may accept connections on.
                else if let Some(text) = other.strip_prefix("--listen=") {
                    let Some((host, port)) = listen_address(text) else { return Err(reject("invalid grant")) };
                    if listen.is_some() { return Err(reject("invalid grant")); }
                    listen = Some((host, port.to_owned()));
                }
                else if let Some(text) = other.strip_prefix("--net-bytes=") { net_bytes = ceiling(text, BYTE_BUDGET_MAX)?; }
                else if let Some(path) = other.strip_prefix("--input=") {
                    if input.is_some() || !std::path::Path::new(path).is_absolute() { return Err(reject("invalid grant")); }
                    input = Some(path.to_owned());
                }
                // The engine's own standard input, as the one input: a pure tool in a pipe. One
                // source only, read whole and bounded like a file (`STANDARD_INPUT`).
                else if let Some(text) = other.strip_prefix("--input-id=") { input_id = Some(parse_dir_id(text, input_id)?); }
                else if other == "--input-stdin" {
                    if input.is_some() { return Err(reject("invalid grant")); }
                    input = Some(STANDARD_INPUT.to_owned());
                }
                else if other == "--sqlite-request-stdin" {
                    if input.is_some() { return Err(reject("invalid grant: only one input source")); }
                    sqlite_request = true;
                    input = Some(STANDARD_INPUT.to_owned());
                }
                else { return Err(reject("invalid grant")); }
            }
        } Ok(()) };
        step().map_err(|error| if error.to_string() != "invalid grant" { error } else {
            let value = arg.split_once('=').map(|(_, value)| value).unwrap_or("");
            let path_flag = ["--input=", "--dir=", "--write-dir=", "--file=", "--write-file=", "--exec="].iter().any(|flag| arg.starts_with(flag));
            let why = if !arg.starts_with("--") || (!arg.contains('=') && !matches!(arg.as_str(), "--output" | "--allocator" | "--fuel" | "--time" | "--unmetered" | "--partial-output" | "--stream-output" | "--plain-output" | "--data-only" | "--clock" | "--rand" | "--no-links" | "--listener-ready-stdout")) { " (not a flag this host knows)" }
                else if path_flag && !std::path::Path::new(value).is_absolute() { " (a path must be absolute)" }
                else if !arg.contains('=') { " (given twice)" }
                else { " (out of range, malformed, or given twice)" };
            // A secret's or a variable's value is not repeated where logs may keep it.
            let shown = match ["--secret=", "--secret-seal=", "--env="].iter().find(|flag| arg.starts_with(*flag)) {
                Some(flag) => { let rest = &arg[flag.len()..]; match rest.split_once('=') { Some((name, _)) => format!("{flag}{name}=..."), None => arg.clone() } }
                None => arg.clone(),
            };
            reject(&format!("invalid grant: {shown}{why}"))
        })?;
    }
    // One source, one route, whatever order the flags came in: a fixed secret's variable is
    // no other slot's, not a store's secret (read from the variable of its name), and not a
    // variable the program reads through `Env`.
    for (index, slot) in secrets.slots.iter().enumerate() {
        let Some(SecretSource::Env(variable)) = &slot.source else { continue };
        let name = &slot.name;
        if secrets.slots[..index].iter().any(|other| matches!(&other.source, Some(SecretSource::Env(v)) if v == variable)) { return Err(reject(&format!("secret slot {name}: source env:{variable} selected twice"))); }
        if secrets.granted.contains_key(variable.as_bytes()) { return Err(reject(&format!("secret slot {name}: source env:{variable} is also a store's secret"))); }
        if granted_env_names.contains(variable) { return Err(reject(&format!("secret slot {name}: source env:{variable} is also granted as ordinary environment"))); }
    }
    // A launcher's `--secret-object` names the file behind a slot's pipe, for a slot with an
    // `fd:` source only (an `env:` value has no file), once per slot. It is added to the objects
    // no grant may read, write, remove or rename, and nothing else reads that list: a wrong
    // identity can refuse more (a file the program needed), never allow more, and the slot's own
    // descriptor stays on the list whatever is given. The same object as another slot's source
    // is one source on two routes, refused as two descriptors on one file are.
    for (name, object) in secret_objects {
        let Some(slot) = secrets.slots.iter().find(|slot| slot.name == name) else { return Err(reject(&format!("secret slot {name}: --secret-object names no --secret-slot"))) };
        if !matches!(slot.source, Some(SecretSource::Fd(..))) { return Err(reject(&format!("secret slot {name}: --secret-object for a source that is not a descriptor"))); }
        match secrets.objects.iter().position(|known| *known == object) {
            // Its own descriptor's file, named again: nothing to add.
            Some(at) if secrets.object_slots[at] == format!("slot {name}") => continue,
            Some(at) => return Err(reject(&format!("secret slot {name}: --secret-object is the same object as {}'s source", secrets.object_slots[at]))),
            None => {}
        }
        secrets.objects.push(object);
        secrets.object_slots.push(format!("slot {name}"));
    }
    // A store's secret is read from the variable of its name: not one `Env` reads too (taken out
    // of the environment, it would read as absent; refused, so the run says why).
    // A store secret given inline (`--secret=NAME=VALUE`, for tests) is in this process's
    // arguments, which any program it starts can read (`ps`): not with `Exec`.
    if exec.is_some() && args.iter().any(|arg| arg.strip_prefix("--secret=").or_else(|| arg.strip_prefix("--secret-seal=")).is_some_and(|rest| rest.contains('='))) {
        return Err(reject("invalid grant: a secret's value on the command line with --exec"));
    }
    if let Some(name) = secrets.granted.keys().find(|name| std::str::from_utf8(name).is_ok_and(|name| granted_env_names.contains(name))) {
        return Err(reject(&format!("secret {}: also granted as ordinary environment", String::from_utf8_lossy(name))));
    }
    // A run without a count has no budget to raise and nothing to report.
    if unmetered && (fuel || budgeted) { return Err(reject("invalid grant: --unmetered with --fuel or --budget")); }
    if unmetered && fuel_per_accept.is_some() { return Err(reject("invalid grant: --unmetered with --fuel-per-accept")); }
    if fuel_per_accept.is_some() && listen.is_none() { return Err(reject("invalid grant: --fuel-per-accept without --listen")); }
    // Words are given only to a run granted them, and within the grant: a launch with more is
    // refused whole, before anything runs (never cut short).
    match args_grant {
        None if !words.is_empty() => return Err(reject("invalid grant: --arg without --args (the program is granted no arguments)")),
        Some((count, bytes)) if words.len() as u64 > count || words.iter().map(|word| word.len() as u64).sum::<u64>() > bytes =>
            return Err(reject(&format!("invalid grant: the launch gives {} words of {} bytes, and the program is granted at most {count} words and {bytes} bytes", words.len(), words.iter().map(Vec::len).sum::<usize>()))),
        _ => {}
    }
    // An instance per connection: what each instance is given is exactly what the first was
    // (`State::renew`), so only grants that can be given again whole are taken. Refused, not
    // ignored: a count for every instance (so no `--unmetered`), no accept of the program's own
    // to refill (`--fuel-per-accept`), and nothing read once or held open across instances (an
    // input, a secret, a started program, a dialled address, the test session).
    if !per_connection && (connection_ms.is_some() || connections.is_some()) { return Err(reject("invalid grant: --connection-deadline-ms and --connections are for --instance-per-connection")); }
    if !per_connection && (own.iter().any(Option::is_some) || waiting.is_some()) { return Err(reject("invalid grant: a budget per connection and --waiting-connections are for --instance-per-connection")); }
    // No ceiling on a whole run's budget only where each connection has its own: a run is
    // never without a bound on what one piece of work may use. Fails closed for a program run
    // once, and for a budget whose own was not given.
    for (at, (flag, budget)) in [("--net-bytes", net_bytes), ("--fs-bytes", fs_bytes), ("--fs-entries", fs_entries), ("--output-bytes", output_bytes)].into_iter().enumerate() {
        if budget == u64::MAX && !(per_connection && own[at].is_some()) {
            return Err(reject(&format!("invalid grant: {flag}=unlimited needs --instance-per-connection and {flag}-per-connection=N (with no ceiling on the run, each connection has its own)")));
        }
    }
    // An input above 16 MiB, or with no bound: only standard input, which is handed over as the
    // program asks for it. A file is read whole before the program starts. Never a stream
    // while the program takes a secret: standard input is then read whole before the program
    // starts, as it always was (checked against every secret's source, `open_input`), so
    // nothing is read from it once a secret is in the instance.
    let secret_granted = !secrets.granted.is_empty() || !secrets.slots.is_empty();
    let tls_granted = secrets.slots.iter().any(|slot| slot.kind == 3);
    if tls_granted && (exec.is_some() || write_file.is_some() || write_dir.is_some()
        || dir_rights.is_some_and(|(_, write)| write))
    {
        return Err(reject("TLS signers require a launch without Exec or writable file and directory grants"));
    }
    let streamed_input = input.as_deref() == Some(STANDARD_INPUT)
        && test.is_none() && !secret_granted && !sqlite_request;
    if sqlite_request && args.iter().any(|arg| arg == "--deadline-ms=none") {
        return Err(reject("invalid grant: SQLite request input needs a finite deadline"));
    }
    if (input_unlimited || input_bytes > 16_777_216) && !streamed_input {
        return Err(reject(if sqlite_request { "invalid grant: SQLite request input is limited to 16777216 bytes" }
            else if secret_granted { "invalid grant: --input-bytes above 16777216, or unlimited, is for a standard input read as a stream, which is never granted beside a secret" }
            else { "invalid grant: --input-bytes above 16777216, or unlimited, is for --input-stdin (an input file is read whole before the program starts)" }));
    }
    if per_connection {
        let refused = if listen.is_none() { Some("needs --listen") } else if unmetered { Some("with --unmetered") } else if fuel_per_accept.is_some() { Some("with --fuel-per-accept (each instance is given --fuel)") }
            else if net.is_some() { Some("with --net") } else if read_file.is_some() || write_file.is_some() { Some("with --file or --write-file (a single file is handed over once; grant its directory)") } else if exec.is_some() { Some("with --exec") } else if !secrets.granted.is_empty() || secrets.slots.iter().any(|slot| slot.kind != 3) { Some("with a secret other than TlsSigner") }
            else if input.is_some() || args.iter().any(|arg| arg == "--input-stdin") { Some("with an input") } else if test.is_some() || fuel || timed || partial { Some("with a test session, --fuel, --time or --partial-output") } else { None };
        if let Some(why) = refused { return Err(reject(&format!("invalid grant: --instance-per-connection {why}"))); }
    }
    let sqlite = match sqlite_path {
        Some(path) => {
            let sealed_slot = secrets.granted.is_empty() && secrets.slots.len() == 1
                && secrets.slots[0].kind == 1 && matches!(secrets.slots[0].source, Some(SecretSource::Fd(..)));
            if sqlite_rights.provision() && secret_granted {
                return Err(reject("SQLite table creation requires a separate grant without secrets"));
            }
            if sqlite_request && (sqlite_rights.provision() || input_id.is_some()) {
                return Err(reject("SQLite request input is for data transactions on a pipe"));
            }
            if per_connection || test.is_some() || (secret_granted && !sealed_slot) || net.is_some() || listen.is_some()
                || dir.is_some() || write_dir.is_some() || read_file.is_some() || write_file.is_some() || exec.is_some()
                || (input.is_some() && !sqlite_request)
            {
                return Err(reject("SQLite requires a one-shot run with console and at most one descriptor-backed sealed secret"));
            }
            Some(sqlite::Grant::new(&path, sqlite_rights, &secrets.objects)?
                .with_reply_bytes(sqlite_reply_bytes.map(|size| size as usize)))
        }
        None if sqlite_request => return Err(reject("SQLite request input requires a database grant")),
        None if sqlite_rights.empty() && sqlite_reply_bytes.is_none() => None,
        None => return Err(reject("SQLite rights and reply limits require a database file")),
    };
    if !stream && output_bytes > 268_435_456 { return Err(reject("invalid grant: --output-bytes above 268435456 without --stream-output")); }
    if stream && (partial || test.is_some()) { return Err(reject("invalid grant: --stream-output with --partial-output")); }
    if plain && !stream { return Err(reject("invalid grant: --plain-output without --stream-output")); }
    stream::PLAIN.store(plain, std::sync::atomic::Ordering::SeqCst);
    if module_fd.is_some() && (module_path.is_some() || test.is_some() || module_pin.is_none()) {
        return Err(reject("--module-fd requires --module-pin and is the only module source"));
    }
    let local_service = match local_service_fd {
        Some(fd) => {
            use std::os::unix::fs::MetadataExt;
            let fd = fd as u32;
            if !per_connection || test.is_some() || secrets.descriptor_selected(fd) {
                return Err(reject("local service requires its own descriptor and a connection instance"));
            }
            let file = sys::take_descriptor(fd).map(std::fs::File::from)
                .map_err(|_| reject("local service descriptor is unavailable"))?;
            let identity = file.metadata()?;
            if secrets.objects.contains(&(identity.dev(), identity.ino())) {
                return Err(reject("local service descriptor aliases a secret source"));
            }
            let reply = local_service_reply.map_or(local_service::REPLY_LIMIT, |bytes| bytes as usize);
            Some(local_service::Grant::from_fd(file.into(), reply)
                .map_err(|_| reject("local service requires a connected Unix stream"))?)
        }
        None if local_service_reply.is_some() => return Err(reject("local service reply limit requires a descriptor")),
        None => None,
    };
    if ready_stdout && (!per_connection || output || test.is_some()) {
        return Err(reject("--listener-ready-stdout requires connection instances and exclusive use of standard output by the host"));
    }
    let granted_input = input.is_some() || test.is_some();
    // Bounded file I/O as well as module checks: reading one byte past the grant detects an
    // oversized module even if the file grows after it is opened.
    let mut disk_bytes = Vec::new();
    let mut module_source;
    let bytes: &[u8] = if let Some(t) = &test { t.module } else {
        module_source = match module_fd {
            Some(fd) => open_module_descriptor(fd as u32, &secrets.objects)?,
            None => open_module(&module_path.ok_or_else(|| reject(USAGE))?, module_pin.is_some(), &secrets.objects)?,
        };
        (&mut module_source).take(module_bytes + 1).read_to_end(&mut disk_bytes)?;
        if disk_bytes.len() as u64 > module_bytes { return Err(reject("rejected: module size limit")); }
        &disk_bytes
    };
    if let Some(pin) = module_pin {
        let digest: String = sha::sha384(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        if pin != digest { return Err(reject("module differs from its independent pin")); }
    }
    if test.is_none() { let _ = MODULE.set(disk_bytes.clone()); }
    let granted_dir = dir.is_some() || write_dir.is_some() || read_file.is_some() || write_file.is_some() ||
        test.as_ref().is_some_and(|t| !t.dirs.is_empty());
    let layout = profile::inspect_slots(&bytes, &profile::Granted { output, allocator, input: granted_input, dir: granted_dir, clock, rand, env,
        net: net.is_some() || listen.is_some(), exec: exec.is_some(), secret: !secrets.granted.is_empty() || !secrets.slots.is_empty(), grant: !grants.is_empty(), args: args_grant.is_some(),
        sqlite: sqlite.is_some(), local_service: local_service.is_some(), memory_pages: pages, module_bytes }).map_err(|why| reject(&format!("rejected: {why}")))?;
    // `main`'s fixed secrets, each bound by name and read now: a run that cannot have them all
    // stops before the program starts.
    // The input is never the object a secret is read from (the same pipe or file reached again
    // through /dev/fd or /proc, or another name for it): checked while the secrets' descriptors
    // are still open, and read only after them (review of 1b5c0ce6: `--input=/dev/fd/N` drained
    // a sealed key into the program's console input).
    let input = match input {
        Some(path) => Some(open_input(&path, input_id, &secrets.objects)?),
        None if input_id.is_some() => return Err(reject("invalid grant: --input-id is for an --input file")),
        None => None,
    };
    if sqlite_request {
        use std::os::unix::fs::FileTypeExt;
        if !input.as_ref().and_then(|file| file.metadata().ok()).is_some_and(|meta| meta.file_type().is_fifo()) {
            return Err(reject("SQLite request input needs a pipe on standard input"));
        }
    }
    secrets.bind(&layout)?;
    // An input file is read whole before the program starts, up to its grant. Standard input
    // is a stream (`InputSource`): nothing of it is read until the program asks.
    let mut input_data = test.as_mut().map(|t| std::mem::take(&mut t.input)).unwrap_or_default();
    let mut source = None;
    let input_granted = if input_unlimited { u64::MAX } else { input_bytes };
    if let Some(file) = input {
        if sqlite_request {
            let until = DEADLINE_AT.get().copied().unwrap_or_else(|| std::time::Instant::now() + SECRET_READ);
            input_data = sys::read_until(file.into(), input_bytes as usize, until).map_err(|failure| match failure {
                sys::ReadFailure::Unreadable => reject("SQLite request input cannot be read"),
                sys::ReadFailure::TooLarge => reject(&input_over(input_bytes, limit::named("--input-bytes").as_deref(), false)),
                sys::ReadFailure::NotClosed => reject("SQLite request input did not close before the deadline"),
            })?;
        } else if streamed_input { source = Some(InputSource { file, granted: input_granted, left: input_granted, taken: 0, ended: false }); }
        else { input_data = read_input(file, input_bytes, limit::named("--input-bytes").as_deref())?; }
    }
    // Again, independently: only the closed profile's section kinds.
    let mut at = 8;
    let mut previous = 0;
    while at < bytes.len() {
        let id = bytes[at]; at += 1;
        // Custom sections after the code only: `witchy.secret-slots` (main's fixed secrets),
        // then one called `name`, last (function names for backtraces). The profile scan
        // checks what each holds.
        const AFTER_SLOTS: u8 = 254;
        let named = id == 0 && (matches!(previous, 10 | 11 | AFTER_SLOTS));
        if !named && (![1, 2, 3, 5, 6, 7, 10, 11].contains(&id) || id <= previous) { return Err(reject("forbidden section")); }
        let after_code = matches!(previous, 10 | 11);
        previous = if named { u8::MAX } else { id };
        let mut size = 0u32;
        let mut complete = false;
        for shift in (0..35).step_by(7) {
            let byte = *bytes.get(at).ok_or_else(|| reject("truncated section"))?; at += 1;
            if shift == 28 && byte & 0xf0 != 0 { return Err(reject("section size")); }
            size |= u32::from(byte & 127) << shift;
            if byte & 128 == 0 { complete = true; break; }
        }
        if !complete { return Err(reject("section size")); }
        let slots = b"\x13witchy.secret-slots";
        if named && after_code && bytes.get(at..at + slots.len()) == Some(slots.as_slice()) { previous = AFTER_SLOTS; }
        else if named && bytes.get(at..at + 5) != Some(b"\x04name".as_slice()) { return Err(reject("forbidden section")); }
        at = at.checked_add(size as usize).filter(|end| *end <= bytes.len()).ok_or_else(|| reject("truncated section"))?;
    }
    // Bound compiler work too, including compact local-count expansion.
    let mut instructions = 0usize;
    // Store confinement, checked here on the exact bytes that run and independently of the
    // compiler that produced them. A module with two memories is a heap module: memory 0 is the
    // program's (frames and cell payloads), memory 1 the allocator's (entries, occupancy,
    // initialization bitmap, counts). Its last HELPERS functions are the allocator's helpers.
    // Every load and store in a helper must name memory 1; every load and store anywhere else
    // must name memory 0. The scan is an allowlist: an instruction outside the closed set below
    // is refused, so nothing that touches a memory in another way can pass.
    const HELPERS: u32 = 7;
    let mut memories = 0u32;
    let mut reservation = 131_072u64;
    let instruction_limit = (module_bytes as usize).max(65_536);
    let mut defined = 0u32;
    let mut body_count = 0u32;
    for payload in wasmtime::wasmparser::Parser::new(0).parse_all(&bytes) {
        use wasmtime::wasmparser::Payload;
        match payload? {
            Payload::TypeSection(types) if types.count() > 4098 => return Err(reject("type limit")),
            Payload::FunctionSection(functions) if functions.count() > 4096 => return Err(reject("function limit")),
            Payload::FunctionSection(functions) => defined = functions.count(),
            Payload::MemorySection(section) => {
                memories = section.count();
                if memories > 2 { return Err(reject("memory limit")); }
                let mut total = 0u64;
                for (index, memory) in section.into_iter().enumerate() {
                    let memory = memory?;
                    if memory.memory64 || memory.shared || memory.maximum != Some(memory.initial) { return Err(reject("memory shape")); }
                    total += memory.initial;
                    reservation = reservation.max(memory.initial.saturating_mul(65_536));
                    // One memory is the two-page frame profile; two are a heap of at least three pages
                    // and the allocator's memory.
                    if memories == 1 && memory.initial != 2 { return Err(reject("memory shape")); }
                    if memories == 2 && index == 0 && memory.initial < 3 { return Err(reject("memory shape")); }
                    if memories == 2 && index == 1 && memory.initial < 1 { return Err(reject("memory shape")); }
                }
                if total > pages { return Err(reject("memory grant exceeded")); }
                if memories == 2 && defined < HELPERS { return Err(reject("heap module without allocator helpers")); }
            }
            Payload::DataSection(data) => {
                use wasmtime::wasmparser::{DataKind, Operator};
                if memories == 0 || data.count() != 1 {
                    return Err(reject("literal data segment shape"));
                }
                for segment in data {
                    let segment = segment?;
                    let DataKind::Active { memory_index: 0, offset_expr } = segment.kind else {
                        return Err(reject("literal segment must initialize program memory"));
                    };
                    let mut ops = offset_expr.get_operators_reader();
                    if !matches!(ops.read()?, Operator::I32Const { value } if i64::from(value) == profile::STACK_HIGH)
                        || !matches!(ops.read()?, Operator::End)
                        || !ops.eof()
                        || segment.data.is_empty()
                        || segment.data.len() as i64 > profile::literal_high(memories == 2) - profile::STACK_HIGH
                        || std::str::from_utf8(segment.data).is_err()
                    {
                        return Err(reject("literal data outside the Wasm literal region"));
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let body_index = body_count; body_count += 1;
                let mut locals = 0u64;
                for local in body.get_locals_reader()? {
                    locals += u64::from(local?.0);
                    if locals > 4096 { return Err(reject("local limit")); }
                }
                let owned = if memories == 2 && body_index >= defined - HELPERS { 1 } else { 0 };
                let mut ops = body.get_operators_reader()?;
                while !ops.eof() {
                    use wasmtime::wasmparser::Operator as Op;
                    match ops.read()? {
                        Op::I32Load { memarg } | Op::I64Load { memarg } | Op::I32Load8U { memarg }
                        | Op::I64Load8U { memarg } | Op::I64Load16U { memarg } | Op::I64Load32U { memarg }
                        | Op::F64Load { memarg } | Op::F64Store { memarg }
                        | Op::I32Store { memarg } | Op::I64Store { memarg } | Op::I32Store8 { memarg }
                        | Op::I64Store8 { memarg } | Op::I64Store16 { memarg } | Op::I64Store32 { memarg } => {
                            if memories == 0 || memarg.memory != owned { return Err(reject("memory access outside the function's own memory")); }
                            if memarg.offset != 0 { return Err(reject("memory offset must be zero")); }
                        }
                        Op::GlobalGet { global_index } | Op::GlobalSet { global_index } => {
                            // The stack pointer, and in a program with tasks the four task globals
                            // (the preflight checked their shape; validation, that they exist).
                            if global_index >= 5 { return Err(reject("only the stack and task globals are allowed")); }
                        }
                        Op::Unreachable | Op::Nop | Op::Block { .. } | Op::Loop { .. } | Op::If { .. } | Op::Else | Op::End
                        | Op::Br { .. } | Op::BrIf { .. } | Op::Return | Op::Call { .. } | Op::Drop | Op::Select
                        | Op::LocalGet { .. } | Op::LocalSet { .. } | Op::LocalTee { .. }
                        | Op::I32Const { .. } | Op::I64Const { .. }
                        | Op::I32Eqz | Op::I32Eq | Op::I32Ne | Op::I32LtU | Op::I32GtU
                        | Op::I32Add | Op::I32Sub | Op::I32Mul | Op::I32And | Op::I32Or | Op::I32WrapI64 | Op::I64ExtendI32U
                        | Op::I64Eqz | Op::I64Eq | Op::I64Ne | Op::I64LtU | Op::I64GtU | Op::I64LeU | Op::I64GeU
                        | Op::I64Add | Op::I64Sub | Op::I64Mul | Op::I64DivU | Op::I64RemU
                        | Op::I64And | Op::I64Or | Op::I64Xor | Op::I64Shl | Op::I64ShrU | Op::I64Ctz | Op::I64Popcnt
                        // Integer SIMD on the value stack, also checked by profile::inspect.
                        | Op::V128Const { .. } | Op::I8x16Shuffle { .. } | Op::I64x2Splat
                        | Op::I64x2ExtractLane { .. } | Op::I64x2ReplaceLane { .. } | Op::I8x16Bitmask
                        | Op::V128And | Op::V128Or | Op::V128Xor
                        | Op::I8x16Add | Op::I16x8Add | Op::I32x4Add | Op::I64x2Add
                        | Op::I8x16Sub | Op::I16x8Sub | Op::I32x4Sub | Op::I64x2Sub
                        | Op::I16x8Mul | Op::I32x4Mul | Op::I64x2Mul
                        | Op::I16x8ExtMulLowI8x16U | Op::I16x8ExtMulHighI8x16U
                        | Op::I8x16Eq | Op::I16x8Eq | Op::I32x4Eq | Op::I64x2Eq
                        | Op::I8x16GtU | Op::I16x8GtU | Op::I32x4GtU | Op::I64x2GtS
                        // I64 arithmetic: signed comparisons, division and remainder. They read and
                        // write the value stack only.
                        | Op::I64LtS | Op::I64GtS | Op::I64LeS | Op::I64GeS | Op::I64DivS | Op::I64RemS
                        // F64 (docs/wasm-f64.md): value-stack operations (F64.bits shows one NaN), and
                        // only i64.trunc_f64_s traps (NaN, or outside I64).
                        | Op::F64Const { .. } | Op::F64Eq | Op::F64Ne | Op::F64Lt | Op::F64Gt | Op::F64Le | Op::F64Ge
                        | Op::F64Abs | Op::F64Neg | Op::F64Floor | Op::F64Sqrt | Op::F64Add | Op::F64Sub | Op::F64Mul | Op::F64Div
                        | Op::I64TruncF64S | Op::F64ConvertI64S | Op::F64ConvertI64U | Op::I64ReinterpretF64 | Op::F64ReinterpretI64 => (),
                        _ => return Err(reject("instruction outside the profile")),
                    }
                    instructions += 1;
                    if instructions > instruction_limit { return Err(reject("instruction limit")); }
                }
            }
            _ => (),
        }
    }
    // `WITCHY_TIME_PHASES=1` with `--time`: startup split into its phases (the `--time` line
    // itself is unchanged, for tools that read it).
    let admitted = std::time::Instant::now();
    // An instance per connection is stopped at its deadline by the engine's epoch, which only
    // this mode compiles in (a check at each loop and call): compiled here, for this run.
    let (engine, module) = if per_connection {
        let mut config = engine_config(unmetered, reservation, false);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, bytes)?;
        (engine, module)
    } else { compile(unmetered, reservation, &bytes)? };
    let compiled = std::time::Instant::now();
    let mut linker = Linker::new(&engine);
    // Only closed scalar/frame/buffer capabilities. Wasmtime validates signatures.
    for namespace in ["witchy.scalar.v1", "witchy.frame.v1", "witchy.buffer.v2"] {
        if output {
            linker.func_wrap(namespace, "print_u64", |mut caller: Caller<'_, State>, handle: i32, value: i64| -> wasmtime::Result<()> {
                if caller.data().active && handle==0 && usize::try_from(value).is_ok_and(|id| id<caller.data().edge_limit) {
                    caller.data_mut().hits.insert(value as u16);
                    return Ok(());
                }
                if !caller.data().active || handle != 1 { return Err(reject("invalid output capability")); }
                emit(&mut caller, format!("{}\n", value as u64).as_bytes())
            })?;
            // One byte of text. The guest passes the value itself; the host reads no guest memory.
            linker.func_wrap(namespace, "print_u8", |mut caller: Caller<'_, State>, handle: i32, value: i64| -> wasmtime::Result<()> {
                if !caller.data().active || handle != 1 { return Err(reject("invalid output capability")); }
                if !(0..256).contains(&value) { return Err(reject("output byte out of range")); }
                emit(&mut caller, &[value as u8])
            })?;
            // Eight bytes of text, the word's lowest byte first: a word of a literal or a String
            // in one call. As for one byte, the guest passes the value and the host reads no guest
            // memory; what the bytes are is the guest's text, checked by the guest as `print_u8`'s.
            linker.func_wrap(namespace, "print_word", |mut caller: Caller<'_, State>, handle: i32, value: i64| -> wasmtime::Result<()> {
                if !caller.data().active || handle != 1 { return Err(reject("invalid output capability")); }
                emit(&mut caller, &(value as u64).to_le_bytes())
            })?;
        }
    }
    if granted_input {
        for namespace in ["witchy.scalar.v1", "witchy.frame.v1", "witchy.buffer.v2"] {
            // The next byte of the input, or 256 once it has ended. The host hands the guest a
            // value and reads or writes no guest memory.
            linker.func_wrap(namespace, "read_u8", |mut caller: Caller<'_, State>, handle: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 3 { return Err(reject("invalid input capability")); }
                let state = caller.data_mut();
                let Some(byte) = next_input(state)? else {
                    return if state.test_control { test_batch::binding_reply(&mut state.output) } else { Ok(256) };
                };
                Ok(i64::from(byte))
            })?;
            // Up to seven bytes, lowest first, with their count in the top byte (0: the input has
            // ended). A run ends after a newline or after a byte of 128 or more, so the guest takes
            // what follows (the next line, a scalar's continuation bytes) as it would have.
            linker.func_wrap(namespace, "read_run", |mut caller: Caller<'_, State>, handle: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 3 { return Err(reject("invalid input capability")); }
                let state = caller.data_mut();
                let mut word: u64 = 0;
                let mut count: u64 = 0;
                while count < 7 {
                    // A run never stops short where the stream's buffer does: only at the input's end.
                    let Some(byte) = next_input(state)? else { break };
                    word |= u64::from(byte) << (8 * count);
                    count += 1;
                    if byte == b'\n' || byte >= 128 { break; }
                }
                Ok((word | (count << 56)) as i64)
            })?;
        }
    }
    if allocator {
        linker.func_wrap("witchy.buffer.v2", "allocator_authorize", |caller: Caller<'_, State>| -> wasmtime::Result<i32> {
            if !caller.data().active { return Err(reject("invalid allocator capability")); }
            Ok(2)
        })?;
    }
    if dir.is_none() && dir_rights.is_some() { return Err(reject("invalid grant")); }
    // `--dir` grants reading unless `--dir-rights` says otherwise; `--write-dir` grants writing
    // there, and then `--dir` cannot also grant it.
    let (read_right, write_right) = if dir.is_some() { dir_rights.unwrap_or((true, false)) } else { (false, false) };
    if !alias_specs.is_empty() && (!read_right || write_right || !no_links || data_only || exec.is_some()
        || per_connection || test.is_some() || secret_granted) {
        return Err(reject("read directory aliases require a read-only --dir with --no-links and a single run without secrets or Exec"));
    }
    let read_aliases = alias_specs.into_iter().map(|(name, number)| {
        let fd = sys::take_descriptor(number).map_err(|_| reject("read directory alias is not an inherited descriptor"))?;
        let directory = std::fs::File::from(fd);
        if !directory.metadata()?.is_dir() { return Err(reject("read directory alias is not a directory")); }
        Ok((name, Dir::from_std_file(directory)))
    }).collect::<wasmtime::Result<Vec<_>>>()?;
    if write_right && write_dir.is_some() { return Err(reject("invalid grant")); }
    // A program's directory must be out of reach of every writable grant: `rename` there could
    // otherwise put an existing executable under a granted name.
    if let Some((exec_dir, _)) = &exec {
        let exec_dir = std::fs::canonicalize(exec_dir).map_err(|_| reject("exec directory unavailable"))?;
        let writable = [write_right.then_some(dir.clone()).flatten(), write_dir.clone()];
        if writable.iter().flatten().any(|w| w.starts_with(&exec_dir) || exec_dir.starts_with(w)) { return Err(reject("exec directory overlaps a writable grant")); }
    }
    // Each granted directory is opened here, once, by the path the launcher gave: the only
    // ambient lookups the run makes. Everything after goes through the `Dir`s opened here.
    let open = |path: &std::path::Path| if no_links { open_without_links(path) } else { Dir::open_ambient_dir(path, cap_std::ambient_authority()) };
    let open_root = |path: &Option<std::path::PathBuf>| path.as_ref().map(|p| open(p).map_err(|_| reject(if no_links { "granted directory cannot be opened without following a link" } else { "granted directory cannot be opened" }))).transpose();
    let (root, write_root) = (open_root(&dir)?, open_root(&write_dir)?);
    if (data_only || dir_id.is_some()) && (root.is_none() || write_dir.is_some()) { return Err(reject("invalid grant: --data-only and --dir-id are for the one --dir")); }
    if let (Some(examined), Some(root)) = (dir_id, &root) { held_to(root, examined)?; }
    Files::data_only().store(match (data_only, write_right) { (false, _) => 0, (true, false) => 1, (true, true) => 2 }, std::sync::atomic::Ordering::SeqCst);
    // A granted file is its directory, opened, and its name in it.
    let leaf = |path: &Option<std::path::PathBuf>| -> wasmtime::Result<Option<(Dir, std::ffi::OsString)>> {
        let Some(path) = path else { return Ok(None) };
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Err(reject("invalid grant")) };
        let dir = if no_links { open_without_links(parent).map_err(|_| reject("granted file's directory cannot be opened without following a link"))? } else {
            let parent = std::fs::canonicalize(parent).map_err(|_| reject("granted file's directory cannot be opened"))?;
            Dir::open_ambient_dir(&parent, cap_std::ambient_authority()).map_err(|_| reject("granted file's directory cannot be opened"))?
        };
        Ok(Some((dir, name.to_owned())))
    };
    let (file_read, file_write) = (leaf(&read_file)?, leaf(&write_file)?);
    // A granted file is never a secret's `fd:` source, by any name or hard link: refused before
    // `main` runs (review of 33face12: `--file` named the sealed key's file). What is at the
    // name is checked as opened, without following a link (the grant never follows one either);
    // reading it is checked again on the descriptor read (`read_whole`).
    for (granted, flag) in [(&file_read, "--file"), (&file_write, "--write-file")] {
        use std::os::unix::fs::MetadataExt;
        let Some((parent, name)) = granted else { continue };
        if let Ok(meta) = open_entry(parent, name).and_then(|file| file.metadata()) {
            if secrets.objects.contains(&(meta.dev(), meta.ino())) { return Err(reject(&format!("invalid grant: {flag} is a secret slot's source"))); }
        }
    }
    // A data directory the run may write: what a killed run left staged is cleared before
    // `main` runs, and a staging directory the engine did not make is refused (`staging_in`).
    let data_root = if data_only && write_right { root.as_ref().map(Dir::try_clone).transpose().map_err(|_| reject("granted directory cannot be opened"))? } else { None };
    let staging = match &data_root { Some(root) => Files::staging_in(root, false).map_err(|why| reject(&why))?, None => None };
    let files = Files { named_dirs: test.as_mut().map(|t| std::mem::take(&mut t.dirs)).unwrap_or_default(), read_aliases, root, write_root, file_read, file_write, read: read_right, write: write_right || write_dir.is_some(), budget: fs_bytes, entry_limit: fs_entries,
        secret_objects: secrets.objects.clone(), data_root, staging, ..Files::default() };
    if granted_dir {
        for namespace in ["witchy.scalar.v1", "witchy.frame.v1", "witchy.buffer.v2"] {
            // `main`'s directory, with the right its parameter names: 0 read, 1 write.
            linker.func_wrap(namespace, "dir_authorize", |mut caller: Caller<'_, State>, right: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                let files = &mut caller.data_mut().files;
                if right >= 16 {
                    let index = usize::try_from(right / 16 - 1)?;
                    let (root, granted) = files.named_dirs.get(index).ok_or_else(|| reject("unbound directory slot"))?;
                    if i64::from(*granted) != right % 16 { return Err(reject("directory slot right mismatch")); }
                    let dir = root.try_clone()?;
                    let read = *granted == 0;
                    return files.issue(Entry::Dir { dir, read, write: !read, pending: vec![], from: vec![] });
                }
                // 3 and 4: the one file granted to read, and the one to write. Each is handed out once.
                if right == 3 {
                    let Some((parent, name)) = files.file_read.take() else { return Err(reject("the program asks for a file the run does not grant")) };
                    let entry = match files.read_on(&parent, &name, None)? { Ok(data) => Entry::Reading { data, at: 0 }, Err(code) => Entry::Failed(code) };
                    return files.issue(entry);
                }
                if right == 4 {
                    let Some((parent, name)) = files.file_write.take() else { return Err(reject("the program asks for a file the run does not grant")) };
                    return files.issue(Entry::Writing { parent, name, data: Vec::new(), exclusive: false, append: false });
                }
                // 0 read, 1 write, 2 both (the book's bare `Dir`, which needs both on one directory).
                let (read, write) = match right {
                    0 if files.read => (true, false),
                    1 if files.write => (false, true),
                    2 if files.read && files.write && files.write_root.is_none() => (true, true),
                    _ => return Err(reject("the program asks for a directory right the run does not grant")),
                };
                if files.root.is_none() && !(write && files.write_root.is_some()) { return Err(reject("the program asks for a directory right the run does not grant")); }
                let root = if write && !read { files.write_root.as_ref().or(files.root.as_ref()) } else { files.root.as_ref() };
                let dir = root.expect("granted directory").try_clone().map_err(|_| reject("granted directory cannot be opened"))?;
                let handle = files.issue(Entry::Dir { dir, read, write, pending: Vec::new(), from: Vec::new() })?;
                if read && !write && !files.read_aliases.is_empty() { files.alias_root = Some(handle); }
                Ok(handle)
            })?;
            linker.func_wrap(namespace, "fs_path_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid directory capability")); }
                let Entry::Dir { pending, .. } = caller.data_mut().files.entry(handle)? else { return Err(reject("invalid directory capability")) };
                // A path past the limit is refused when it is used; it does not grow further.
                if pending.len() <= MAX_PATH { pending.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "fs_open_file", |mut caller: Caller<'_, State>, handle: i64, mode: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.open_file(handle, mode)
            })?;
            linker.func_wrap(namespace, "fs_open_dir", |mut caller: Caller<'_, State>, handle: i64, mode: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.open_dir(handle, mode)
            })?;
            linker.func_wrap(namespace, "fs_make_dir", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.make_dir(handle)
            })?;
            linker.func_wrap(namespace, "fs_status", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                Ok(match caller.data_mut().files.entry(handle)? { Entry::Failed(code) => *code, Entry::Closed => return Err(reject("invalid file capability")), _ => 0 })
            })?;
            linker.func_wrap(namespace, "fs_read_u8", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                let Entry::Reading { data, at } = caller.data_mut().files.entry(handle)? else { return Err(reject("invalid file capability")) };
                let Some(byte) = data.get(*at).copied() else { return Ok(256) };
                *at += 1;
                Ok(i64::from(byte))
            })?;
            linker.func_wrap(namespace, "fs_snapshot_remaining", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                let Entry::Reading { data, at } = caller.data_mut().files.entry(handle)? else { return Err(reject("invalid file capability")) };
                Ok((data.len() - *at) as i64)
            })?;
            linker.func_wrap(namespace, "fs_write_u8", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid file capability")); }
                let files = &mut caller.data_mut().files;
                let (full, budget) = (files.used >= files.budget, files.budget); let share = files.share;
                let Entry::Writing { data, .. } = files.entry(handle)? else { return Err(reject("invalid file capability")) };
                if full { return Ok(Files::full(budget, share)); }
                data.push(byte as u8);
                files.used += 1;
                Ok(0)
            })?;
            // Bulk: up to the range's length from a file being read (0 at its end), or the range
            // added to a file being written; `count | error << 32`.
            linker.func_wrap(namespace, "fs_read", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                let (_, _, length) = guest_range(&mut caller, packed)?;
                let Entry::Reading { data, at } = caller.data_mut().files.entry(handle)? else { return Err(reject("invalid file capability")) };
                let end = (*at + length).min(data.len());
                let bytes = data[*at..end].to_vec();
                *at = end;
                write_guest(&mut caller, packed, &bytes)?;
                Ok(bytes.len() as i64)
            })?;
            linker.func_wrap(namespace, "fs_write", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                let bytes = read_guest(&mut caller, packed)?;
                let files = &mut caller.data_mut().files;
                let (room, budget) = (files.budget.saturating_sub(files.used), files.budget); let share = files.share;
                let Entry::Writing { data, .. } = files.entry(handle)? else { return Err(reject("invalid file capability")) };
                let taken = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
                data.extend_from_slice(&bytes[..taken]);
                files.used += taken as u64;
                Ok(taken as i64 | if taken < bytes.len() { Files::full(budget, share) << 32 } else { 0 })
            })?;
            linker.func_wrap(namespace, "fs_close", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                caller.data_mut().files.close(handle)
            })?;
            linker.func_wrap(namespace, "fs_probe", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.probe(handle)
            })?;
            linker.func_wrap(namespace, "fs_open_list", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.open_list(handle)
            })?;
            linker.func_wrap(namespace, "fs_remove", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.remove(handle)
            })?;
            linker.func_wrap(namespace, "fs_rename_from", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.rename_from(handle)
            })?;
            linker.func_wrap(namespace, "fs_rename", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.rename(handle)
            })?;
            linker.func_wrap(namespace, "fs_open_append", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.open_writing(handle, true)
            })?;
            linker.func_wrap(namespace, "fs_open_new", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid directory capability")); }
                caller.data_mut().files.open_writing(handle, false)
            })?;
            linker.func_wrap(namespace, "fs_abandon", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid file capability")); }
                match std::mem::replace(caller.data_mut().files.entry(handle)?, Entry::Closed) { Entry::Closed => Err(reject("invalid file capability")), _ => Ok(0) }
            })?;
        }
    }
    for namespace in ["witchy.scalar.v1", "witchy.frame.v1", "witchy.buffer.v2"] {
        // Public computation (src/compiler/host.rs `Family::Crypto`): no handle and no grant, every
        // run has it. Each range is checked against the program's memory before any is read.
        if sqlite.is_some() { sqlite::register(&mut linker, namespace)?; }
        if local_service.is_some() { local_service::register(&mut linker, namespace)?; }
        x509::register(&mut linker, namespace)?;
        aead::register(&mut linker, namespace)?;
        linker.func_wrap(namespace, "crypto_verify_tag48", |mut caller: Caller<'_, State>, expected: i64, received: i64| -> wasmtime::Result<i64> {
            if !caller.data().active { return Err(reject("crypto outside the program")); }
            let (memory, a, a_len) = guest_range(&mut caller, expected)?;
            let (_, b, b_len) = guest_range(&mut caller, received)?;
            if a_len != 48 || b_len != 48 || sha::provider().is_err() { return Ok(0); }
            let data = memory.data(&caller);
            let equal = aws_lc_rs::constant_time::verify_slices_are_equal(&data[a..a + a_len], &data[b..b + b_len]);
            Ok(i64::from(equal.is_ok()))
        })?;
        register_public_digest(&mut linker, namespace, "crypto_sha256", sha::sha256)?;
        register_public_digest(&mut linker, namespace, "crypto_sha384", sha::sha384)?;
        register_public_digest(&mut linker, namespace, "crypto_sha512", sha::sha512)?;
        for (name, verify) in [
            ("crypto_mldsa65_verify", sha::mldsa65_verify as fn(&[u8], &[u8], &[u8]) -> bool),
            ("crypto_mldsa87_verify", sha::mldsa87_verify),
            ("crypto_p256_verify", sha::p256_verify),
        ] {
            linker.func_wrap(namespace, name, move |mut caller: Caller<'_, State>, key: i64, message: i64, signature: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("crypto outside the program")); }
                let (memory, key_at, key_length) = guest_range(&mut caller, key)?;
                let (_, message_at, message_length) = guest_range(&mut caller, message)?;
                let (_, signature_at, signature_length) = guest_range(&mut caller, signature)?;
                if message_length > MAX_HASHED || sha::provider().is_err() { return Ok(0); }
                let data = memory.data(&caller);
                Ok(i64::from(verify(&data[key_at..key_at + key_length], &data[message_at..message_at + message_length], &data[signature_at..signature_at + signature_length])))
            })?;
        }
        linker.func_wrap(namespace, "crypto_rsa_pkcs1_sha256_verify",
            |mut caller: Caller<'_, State>, modulus: i64, exponent: i64, message: i64, signature: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("crypto outside the program")); }
                let (memory, n, nl) = guest_range(&mut caller, modulus)?;
                let (_, e, el) = guest_range(&mut caller, exponent)?;
                let (_, m, ml) = guest_range(&mut caller, message)?;
                let (_, s, sl) = guest_range(&mut caller, signature)?;
                if ml > MAX_HASHED || sha::provider().is_err() { return Ok(0); }
                let data = memory.data(&caller);
                Ok(i64::from(sha::rsa_pkcs1_sha256_verify(&data[n..n + nl], &data[e..e + el],
                    &data[m..m + ml], &data[s..s + sl])))
            },
        )?;
        // The clock and randomness each have one value, handed to `main`: handle 4 and 5.
        if clock {
            linker.func_wrap(namespace, "clock_authorize", |caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid clock capability")); }
                Ok(4)
            })?;
            // A timer due at `due` (wall-clock ms): a handle number a poll may name.
            linker.func_wrap(namespace, "clock_timer", |mut caller: Caller<'_, State>, handle: i64, due: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 4 { return Err(reject("invalid clock capability")); }
                caller.data_mut().network.issue(NetEntry::Timer(due as u64))
            })?;
            // Gives a timer's handle number back.
            linker.func_wrap(namespace, "clock_timer_end", |mut caller: Caller<'_, State>, handle: i64, timer: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 4 { return Err(reject("invalid clock capability")); }
                // A number naming no timer is passed over: it is data, and ending nothing is no fault.
                if let Ok(entry) = caller.data_mut().network.entry(timer) && matches!(entry, NetEntry::Timer(_)) { *entry = NetEntry::Closed; }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "clock_now_ms", |caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 4 { return Err(reject("invalid clock capability")); }
                let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| reject("clock before the epoch"))?;
                Ok(i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
            })?;
        }
        if env {
            // The environment's one value is handle 6. A name is sent a byte at a time; the value
            // looked up is read a byte at a time.
            linker.func_wrap(namespace, "env_authorize", |caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid environment capability")); }
                Ok(6)
            })?;
            linker.func_wrap(namespace, "env_name_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 6 || !(0..256).contains(&byte) { return Err(reject("invalid environment capability")); }
                let name = &mut caller.data_mut().env_name;
                if name.len() <= 1024 { name.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "env_lookup", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 6 { return Err(reject("invalid environment capability")); }
                let state = caller.data_mut();
                let name = std::mem::take(&mut state.env_name);
                state.env_value = state.env_variables.get(&name).cloned();
                Ok(state.env_value.as_ref().map_or(-1, |v| v.len() as i64))
            })?;
            linker.func_wrap(namespace, "env_value_byte", |caller: Caller<'_, State>, handle: i64, index: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 6 { return Err(reject("invalid environment capability")); }
                let value = caller.data().env_value.as_deref().unwrap_or_default();
                Ok(usize::try_from(index).ok().and_then(|i| value.get(i)).map_or(256, |b| i64::from(*b)))
            })?;
        }
        if args_grant.is_some() {
            // The launch's words are handle 14: how many, one chosen by its index, and its
            // bytes one at a time. Read-only, and the same however often they are read.
            linker.func_wrap(namespace, "args_authorize", |caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid arguments capability")); }
                Ok(14)
            })?;
            linker.func_wrap(namespace, "args_count", |caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 14 { return Err(reject("invalid arguments capability")); }
                Ok(caller.data().words.len() as i64)
            })?;
            linker.func_wrap(namespace, "args_select", |mut caller: Caller<'_, State>, handle: i64, index: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 14 { return Err(reject("invalid arguments capability")); }
                let state = caller.data_mut();
                state.word = usize::try_from(index).ok().filter(|index| *index < state.words.len());
                Ok(state.word.map_or(-1, |index| state.words[index].len() as i64))
            })?;
            linker.func_wrap(namespace, "args_byte", |caller: Caller<'_, State>, handle: i64, index: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 14 { return Err(reject("invalid arguments capability")); }
                let state = caller.data();
                let word = state.word.map(|index| state.words[index].as_slice()).unwrap_or_default();
                Ok(usize::try_from(index).ok().and_then(|i| word.get(i)).map_or(256, |b| i64::from(*b)))
            })?;
        }
        if !grants.is_empty() {
            // The grants' one reader is handle 12. A `Type.field` name is sent a byte at a time;
            // a name the run does not grant stops the run.
            linker.func_wrap(namespace, "grant_authorize", |caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid grant capability")); }
                Ok(12)
            })?;
            linker.func_wrap(namespace, "grant_name_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 12 || !(0..256).contains(&byte) { return Err(reject("invalid grant capability")); }
                let name = &mut caller.data_mut().grant_name;
                if name.len() <= 128 { name.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "grant_lookup", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 12 { return Err(reject("invalid grant capability")); }
                let state = caller.data_mut();
                let name = std::mem::take(&mut state.grant_name);
                state.grants.get(&name).copied().ok_or_else(|| reject(&format!("the run grants no {}", String::from_utf8_lossy(&name))))
            })?;
        }
        if !secrets.granted.is_empty() || !secrets.slots.is_empty() {
            // 0: the store, when the run grants one; otherwise one of `main`'s fixed secrets. A
            // fixed secret's grant is that secret alone, never a store.
            let store = !secrets.granted.is_empty();
            tls_signer::register(&mut linker, namespace)?;
            mldsa_signer::register(&mut linker, namespace)?;
            linker.func_wrap(namespace, "secret_authorize", move |mut caller: Caller<'_, State>, code: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                if code == 0 && store { return caller.data_mut().secrets.issue(SecretEntry::Store { pending: Vec::new() }); }
                caller.data_mut().secrets.take_slot(code)
            })?;
            linker.func_wrap(namespace, "secret_name_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid secret capability")); }
                let SecretEntry::Store { pending } = caller.data_mut().secrets.entry(handle)? else { return Err(reject("invalid secret capability")) };
                if pending.len() <= 64 { pending.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "secret_get", |mut caller: Caller<'_, State>, handle: i64, sealed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..2).contains(&sealed) { return Err(reject("invalid secret capability")); }
                caller.data_mut().secrets.get(handle, sealed == 1)
            })?;
            linker.func_wrap(namespace, "secret_status", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                Ok(match caller.data_mut().secrets.entry(handle)? { SecretEntry::Failed(code) => *code, _ => 0 })
            })?;
            linker.func_wrap(namespace, "secret_byte", |mut caller: Caller<'_, State>, handle: i64, index: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                caller.data_mut().secrets.byte(handle, index)
            })?;
            linker.func_wrap(namespace, "secret_message_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid secret capability")); }
                caller.data_mut().secrets.message_byte(handle, byte)
            })?;
            linker.func_wrap(namespace, "secret_hmac", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                caller.data_mut().secrets.hmac(handle)
            })?;
            linker.func_wrap(namespace, "secret_hmac_sha384", |mut caller: Caller<'_, State>, handle: i64, message: i64, tag: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                let (memory, input, length) = guest_range(&mut caller, message)?;
                let (_, output, capacity) = guest_range(&mut caller, tag)?;
                check_guest_write(output, capacity, memory.data_size(&caller))?;
                let (data, state) = memory.data_and_store_mut(&mut caller);
                let SecretEntry::Secret { value, seal: true, .. } = state.secrets.entry(handle)?
                    else { return Err(reject("invalid secret capability")); };
                if capacity != 48 { return Ok(2); }
                if length > MAX_MESSAGE { return Ok(3); }
                if sha::provider().is_err() { return Ok(4); }
                let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA384, value);
                let mac = aws_lc_rs::hmac::sign(&key, &data[input..input + length]);
                data[output..output + capacity].copy_from_slice(mac.as_ref());
                Ok(0)
            })?;
            linker.func_wrap(namespace, "secret_digest_byte", |mut caller: Caller<'_, State>, handle: i64, index: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                caller.data_mut().secrets.digest_byte(handle, index)
            })?;
            // A signer's public key into a range of 1952 bytes: 0, or 2 for another length.
            linker.func_wrap(namespace, "signer_public_key", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                let (_, _, length) = guest_range(&mut caller, packed)?;
                let mut public = vec![0u8; if length == sha::MLDSA65_PUBLIC_KEY { length } else { 0 }];
                let code = caller.data_mut().secrets.public_key(handle, &mut public)?;
                if code == 0 { write_guest(&mut caller, packed, &public)?; }
                Ok(code)
            })?;
            // Sign the first range into the second, both checked against the program's memory
            // before anything is read or written: 0; 1 wrong domain; 2 the signature range is
            // not 3309 bytes; 3 the message is too long; 4 the provider failed.
            linker.func_wrap(namespace, "signer_sign", |mut caller: Caller<'_, State>, handle: i64, message: i64, signature: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid secret capability")); }
                let (_, _, out) = guest_range(&mut caller, signature)?;
                let (_, _, length) = guest_range(&mut caller, message)?;
                caller.data_mut().secrets.signer(handle)?;
                if out != sha::MLDSA65_SIGNATURE { return Ok(2); }
                if length > MAX_SIGNED { return Ok(3); }
                let bytes = read_guest(&mut caller, message)?;
                match caller.data_mut().secrets.sign(handle, &bytes)? {
                    Ok(signed) => { write_guest(&mut caller, signature, &signed)?; Ok(0) }
                    Err(code) => Ok(code),
                }
            })?;
        }
        if exec.is_some() {
            // The run's programs are one `Exec`, handed to `main`.
            linker.func_wrap(namespace, "exec_authorize", |mut caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let programs = &mut caller.data_mut().programs;
                let names = programs.names.clone();
                programs.issue(ExecEntry::Exec { names, name: Vec::new(), args: Vec::new(), arg: Vec::new(), settings: RunSettings::default() })
            })?;
            linker.func_wrap(namespace, "exec_name_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid exec capability")); }
                let ExecEntry::Exec { name, .. } = caller.data_mut().programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                if name.len() <= 256 { name.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "exec_only", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.only(handle)
            })?;
            linker.func_wrap(namespace, "exec_arg_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid exec capability")); }
                let programs = &mut caller.data_mut().programs;
                if programs.pending >= MAX_PENDING { return Err(reject("exec argument limit exceeded")); }
                let ExecEntry::Exec { arg, .. } = programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                let added = usize::from(arg.len() <= MAX_ARG);
                if added == 1 { arg.push(byte as u8); }
                programs.pending += added;
                Ok(0)
            })?;
            linker.func_wrap(namespace, "exec_arg_end", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let ExecEntry::Exec { args, arg, .. } = caller.data_mut().programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                // One too many, or one too long, makes the next run fail as invalid.
                let mut taken = std::mem::take(arg);
                if taken.len() > MAX_ARG { taken.push(0); }
                if args.len() <= MAX_ARGS { args.push(taken); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "exec_run", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.run(handle)
            })?;
            linker.func_wrap(namespace, "exec_status", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                Ok(match caller.data_mut().programs.entry(handle)? { ExecEntry::Failed(code) => *code, _ => 0 })
            })?;
            linker.func_wrap(namespace, "exec_out_u8", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let ExecEntry::Run { out, at, .. } = caller.data_mut().programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                Ok(match out.get(*at) { Some(byte) => { *at += 1; i64::from(*byte) } None => 256 })
            })?;
            linker.func_wrap(namespace, "exec_code", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let ExecEntry::Run { code, .. } = caller.data_mut().programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                Ok(*code)
            })?;
            linker.func_wrap(namespace, "exec_input_u8", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid exec capability")); }
                let programs = &mut caller.data_mut().programs;
                if programs.pending >= MAX_PENDING { return Err(reject("exec argument limit exceeded")); }
                let ExecEntry::Exec { settings, .. } = programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                settings.input.push(byte as u8);
                programs.pending += 1;
                Ok(0)
            })?;
            linker.func_wrap(namespace, "exec_deadline_ms", |mut caller: Caller<'_, State>, handle: i64, ms: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.narrow(handle, Some(ms as u64), None)
            })?;
            linker.func_wrap(namespace, "exec_output_limit", |mut caller: Caller<'_, State>, handle: i64, limit: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.narrow(handle, None, Some(limit as u64))
            })?;
            linker.func_wrap(namespace, "exec_env_name_u8", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.env_byte(handle, false, byte)
            })?;
            linker.func_wrap(namespace, "exec_env_value_u8", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.env_byte(handle, true, byte)
            })?;
            linker.func_wrap(namespace, "exec_env_end", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                caller.data_mut().programs.env_end(handle)
            })?;
            // A granted directory moved to the Exec: taken from the file handles (that handle is
            // closed), kept as the opened directory for the next run.
            linker.func_wrap(namespace, "exec_cwd", |mut caller: Caller<'_, State>, handle: i64, dir: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let state = caller.data_mut();
                let ExecEntry::Exec { .. } = state.programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                let taken = match state.files.entry(dir) {
                    Ok(slot @ Entry::Dir { .. }) => std::mem::replace(slot, Entry::Closed),
                    Ok(Entry::Failed(_)) => { if let ExecEntry::Exec { settings, .. } = state.programs.entry(handle)? { settings.refused = EXEC_INVALID; } return Ok(0) }
                    _ => return Err(reject("invalid directory capability")),
                };
                let Entry::Dir { dir, .. } = taken else { unreachable!() };
                let ExecEntry::Exec { settings, .. } = state.programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                settings.cwd = Some(dir);
                Ok(0)
            })?;
            linker.func_wrap(namespace, "exec_err_u8", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid exec capability")); }
                let ExecEntry::Run { err, err_at, .. } = caller.data_mut().programs.entry(handle)? else { return Err(reject("invalid exec capability")) };
                Ok(match err.get(*err_at) { Some(byte) => { *err_at += 1; i64::from(*byte) } None => 256 })
            })?;
        }
        if net.is_some() || listen.is_some() {
            // The run's endpoints are one `Net`, handed to `main` (code 0); the address it may
            // listen on is one `Listener` (code 1). Each needs its own grant.
            linker.func_wrap(namespace, "net_authorize", |mut caller: Caller<'_, State>, code: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let network = &mut caller.data_mut().network;
                match code {
                    0 if !network.allowed.is_empty() => { let set = network.allowed.clone(); network.issue(NetEntry::Net { set, pending: Vec::new() }) }
                    1 => {
                        let listener = network.listening.as_ref().ok_or_else(|| reject("ungranted listening address"))?.try_clone().map_err(|_| reject("listening address unavailable"))?;
                        network.issue(NetEntry::Listener(listener))
                    }
                    // The connection this instance was started for, once.
                    2 => { let connection = network.connection.take().ok_or_else(|| reject("ungranted network capability"))?; network.issue(connection) }
                    _ => Err(reject("ungranted network capability")),
                }
            })?;
            linker.func_wrap(namespace, "net_accept", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let accepted = caller.data_mut().network.accept(handle)?;
                refill_on_accept(&mut caller, accepted)?;
                Ok(accepted)
            })?;
            linker.func_wrap(namespace, "net_addr_byte", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || !(0..256).contains(&byte) { return Err(reject("invalid network capability")); }
                let NetEntry::Net { pending, .. } = caller.data_mut().network.entry(handle)? else { return Err(reject("invalid network capability")) };
                if pending.len() <= 256 { pending.push(byte as u8); }
                Ok(0)
            })?;
            linker.func_wrap(namespace, "net_only", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.only(handle)
            })?;
            linker.func_wrap(namespace, "net_connect", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.connect(handle)
            })?;
            linker.func_wrap(namespace, "net_status", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                Ok(match caller.data_mut().network.entry(handle)? { NetEntry::Failed(code) => *code, NetEntry::Closed => return Err(reject("invalid network capability")), _ => 0 })
            })?;
            linker.func_wrap(namespace, "net_send_u8", |mut caller: Caller<'_, State>, handle: i64, byte: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.send(handle, byte)
            })?;
            linker.func_wrap(namespace, "net_recv_u8", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.receive(handle)
            })?;
            linker.func_wrap(namespace, "net_close", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.close(handle)
            })?;
            // Groups and operations that never wait (docs/design-async.md, step 6).
            linker.func_wrap(namespace, "net_group", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.group(handle)
            })?;
            linker.func_wrap(namespace, "net_group_add", |mut caller: Caller<'_, State>, group: i64, socket: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.group_add(group, socket)
            })?;
            linker.func_wrap(namespace, "net_group_take", |mut caller: Caller<'_, State>, group: i64, slot: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.group_take(group, slot)
            })?;
            linker.func_wrap(namespace, "net_wait", |mut caller: Caller<'_, State>, group: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                // A `&mut [U64]`: the length is a count of words.
                let count = (packed as u64 >> 32) as usize;
                if count > MAX_HANDLES + 1 { return Err(reject("too many entries to wait on")); }
                let bytes_packed = (packed as u64 & 0xffff_ffff) as i64 | ((count * 8) as i64) << 32;
                let bytes = read_guest(&mut caller, bytes_packed)?;
                let mut entries: Vec<u64> = bytes.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().expect("eight bytes"))).collect();
                let ready = caller.data_mut().network.wait(group, &mut entries)?;
                let out: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
                write_guest(&mut caller, bytes_packed, &out)?;
                Ok(ready)
            })?;
            linker.func_wrap(namespace, "net_handle", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                caller.data_mut().network.handle_number(handle)
            })?;
            linker.func_wrap(namespace, "net_poll", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                if !matches!(caller.data_mut().network.entry(handle)?, NetEntry::Listener(_) | NetEntry::Net { .. }) { return Err(reject("invalid network capability")); }
                let count = (packed as u64 >> 32) as usize;
                if count > MAX_HANDLES + 1 { return Err(reject("too many entries to wait on")); }
                let bytes_packed = (packed as u64 & 0xffff_ffff) as i64 | ((count * 8) as i64) << 32;
                let bytes = read_guest(&mut caller, bytes_packed)?;
                let mut entries: Vec<u64> = bytes.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().expect("eight bytes"))).collect();
                let ready = caller.data_mut().network.poll_handles(&mut entries)?;
                let out: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
                write_guest(&mut caller, bytes_packed, &out)?;
                Ok(ready)
            })?;
            linker.func_wrap(namespace, "net_try_recv", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                // Straight into the program's memory: no copy, nothing allocated per call.
                let (memory, address, length) = guest_range(&mut caller, packed)?;
                check_guest_write(address, length, memory.data_size(&caller))?;
                let (data, state) = memory.data_and_store_mut(&mut caller);
                state.network.try_receive(handle, &mut data[address..address + length])
            })?;
            linker.func_wrap(namespace, "net_accepted_on", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let (memory, address, length) = guest_range(&mut caller, packed)?;
                check_guest_write(address, length, memory.data_size(&caller))?;
                let (data, state) = memory.data_and_store_mut(&mut caller);
                state.network.accepted_on(handle, &mut data[address..address + length])
            })?;
            linker.func_wrap(namespace, "net_try_send", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let (memory, address, length) = guest_range(&mut caller, packed)?;
                let (data, state) = memory.data_and_store_mut(&mut caller);
                state.network.try_send(handle, &data[address..address + length])
            })?;
            linker.func_wrap(namespace, "net_try_accept", |mut caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let accepted = caller.data_mut().network.try_accept(handle)?;
                refill_on_accept(&mut caller, accepted)?;
                Ok(accepted)
            })?;
            // Bulk: a range of the program's memory crosses in one call (`address | length << 32`).
            linker.func_wrap(namespace, "net_send", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let bytes = read_guest(&mut caller, packed)?;
                caller.data_mut().network.send_bulk(handle, &bytes)
            })?;
            linker.func_wrap(namespace, "net_recv", |mut caller: Caller<'_, State>, handle: i64, packed: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid network capability")); }
                let (_, _, length) = guest_range(&mut caller, packed)?;
                let (bytes, result) = caller.data_mut().network.receive_bulk(handle, length)?;
                write_guest(&mut caller, packed, &bytes)?;
                Ok(result)
            })?;
        }
        if rand {
            key_agreement::register(&mut linker, namespace)?;
            linker.func_wrap(namespace, "rand_authorize", |caller: Caller<'_, State>, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active { return Err(reject("invalid randomness capability")); }
                Ok(5)
            })?;
            linker.func_wrap(namespace, "rand_u64", |caller: Caller<'_, State>, handle: i64, _: i64| -> wasmtime::Result<i64> {
                if !caller.data().active || handle != 5 { return Err(reject("invalid randomness capability")); }
                // The provider's DRBG, seeded from the operating system, read 256 bytes at a time,
                // each byte handed out once (a source opened and read per draw cost ~40 µs a
                // draw: a shuffle of 20,000 decks ran out of time).
                let mut bytes = [0u8; 8];
                drawn(&mut bytes).map_err(|_| reject("randomness unavailable"))?;
                Ok(i64::from_le_bytes(bytes))
            })?;
        }
    }
    // The programs' directory is opened before the program runs; a grant naming one that
    // cannot be opened stops the run there.
    let programs = match &exec {
        None => Programs::default(),
        Some((dir, names)) => Programs { dir: Some(Dir::open_ambient_dir(dir, cap_std::ambient_authority()).map_err(|_| reject("exec directory unavailable"))?), names: names.clone(), budget: exec_bytes,
            env_names: exec_env.clone().unwrap_or_default(), ..Programs::default() },
    };
    if exec.is_none() && exec_env.is_some() { return Err(reject("invalid grant")); }
    // The listening address is bound before the program runs, so a grant that cannot be
    // honored stops the run before it starts.
    let (listening, authority) = match &listen {
        None => (None, None),
        Some((host, port)) => {
            let (bound, authority) = bind_granted(host, port).map_err(|_| reject("listening address unavailable"))?;
            if port.bytes().all(|byte| byte == b'0') {
                say!("listening on {}", bound.local_addr()?);
            }
            (Some(bound), Some(authority))
        }
    };
    let limits = Counted::new(reservation);
    usage::limits(usage::Limits { fuel: (!unmetered).then_some((budget, fuel_per_accept)), each: per_connection, net: (net.is_some() || listen.is_some()).then_some(net_bytes),
        files: granted_dir.then_some((fs_bytes, fs_entries)), output: output.then_some(output_bytes), memory: pages,
        input: source.is_some().then_some(input_granted), own });
    let state = State { interval: budget, interval_started: false, hits: Default::default(), edge_limit: test.as_ref().map_or(0, |t| t.edges), test_control: test.is_some() && input_data.first()==Some(&255), limits, output: Vec::new(), output_limit: output_bytes as usize, streamed: stream.then_some(0), refill: fuel_per_accept, active: false, input: input_data, taken: 0,
        output_share: Share::Run, lines: (stream && per_connection).then(Vec::new), source, files,
        network: Network { allowed: net.clone().unwrap_or_default(), budget: net_bytes, listening, authority, ..Network::default() },
        programs, secrets, sqlite, local_service, agreements: Default::default(),
        env_variables: variables, env_name: Vec::new(), env_value: None, grants: grants.clone(), grant_name: Vec::new(), words: words.clone(), word: None,
        #[cfg(feature = "guest-profile")] samples: profile_guest::Samples::default() };
    if per_connection {
        let mut state = state;
        // The listener is the engine's in this mode: the program is handed connections, and
        // has no listener to ask for.
        let listener = state.network.listening.take().ok_or_else(|| reject("ungranted listening address"))?;
        // Connections that wait for an instance are held by the engine, not left to the
        // system's queue (128 on macOS, whatever is asked): as many as were asked for, or as
        // this process may have open.
        let waiting = sys::room_for_descriptors(waiting.unwrap_or(1024) as usize, 64 + 16);
        { use std::os::fd::AsRawFd; sys::listen_again(listener.as_raw_fd(), waiting); }
        let ledger = Ledger { net: Pool::new(net_bytes, own[0]), files: Pool::new(fs_bytes, own[1]), entries: Pool::new(fs_entries, own[2]), output: Pool::new(output_bytes, own[3]) };
        let plan = PerConnection { ready_stdout, fuel: budget, deadline: std::time::Duration::from_millis(connection_ms.unwrap_or(10_000)), deadline_ms: connection_ms.unwrap_or(10_000), most: connections, reservation, waiting, ledger };
        let (mut state, _served) = serve_connections(&engine, &module, &linker, &listener, state, &plan)?;
        if stream { stream::finish(true); }
        usage::ended();
        return Ok(std::mem::take(&mut state.output));
    }
    let mut store = Store::new(&engine, state);
    #[cfg(feature = "guest-profile")]
    profile_guest::start(&engine, &mut store);
    store.limiter(|state| &mut state.limits);
    if let Some(test) = &test { store.set_epoch_deadline(test.ticks); }
    if !unmetered { store.set_fuel(budget)?; }
    let instance = linker.instantiate(&mut store, &module)?;
    let run = instance.get_typed_func::<(), ()>(&mut store, "run")?;
    store.data_mut().active = true;
    let running = std::time::Instant::now();
    let result = run.call(&mut store, ());
    let ran = running.elapsed();
    store.data_mut().active = false;
    if !unmetered {
        let left = store.get_fuel()?;
        let state = store.data();
        usage::interval(!state.interval_started, state.interval.saturating_sub(left));
    }
    usage::copy(store.data(), true);
    if fuel { say!("fuel {}", budget.saturating_sub(store.get_fuel()?)); }
    #[cfg(feature = "guest-profile")]
    profile_guest::report(&store.data().samples);
    if timed { say!("time startup {:.3} ms run {:.3} ms", (running - started).as_secs_f64() * 1e3, ran.as_secs_f64() * 1e3); }
    if timed && std::env::var_os("WITCHY_TIME_PHASES").is_some() {
        let ms = |from: std::time::Instant, to: std::time::Instant| (to - from).as_secs_f64() * 1e3;
        say!("time phases read+admit {:.3} ms compile {:.3} ms link+instantiate {:.3} ms", ms(started, admitted), ms(admitted, compiled), ms(compiled, running));
    }
    // Streamed output: the last line goes out however the run ended.
    if stream { stream::finish(true); }
    // Output is committed only after a successful invocation, never on a trap.
    if result.is_err() && stream {}
    else if result.is_err() && partial { 
        // The program's lines, each marked as its: none reads as a line of the engine's.
        let shown = String::from_utf8_lossy(&stream::plain(&store.data().output, &mut false)).into_owned();
        say!("output before the trap:{}", shown.strip_suffix('\n').unwrap_or(&shown).split('\n').map(|line| format!("\n| {line}")).collect::<String>()); }
    else if result.is_err() && !store.data().output.is_empty() {
        say!("the program wrote {} bytes before it stopped; output is kept only for a run that finishes (`--partial-output` shows it on stderr)", store.data().output.len());
    }
    if let Some(test) = test { *test.feedback = std::mem::take(&mut store.data_mut().hits); }
    result?;
    Ok(std::mem::take(&mut store.data_mut().output))
}
/// `--instance-per-connection`: what each connection's instance is given. `waiting`: how
/// many accepted connections the engine holds while the instance is busy. `ledger`: the whole run's budgets, and each connection's own.
struct PerConnection { ready_stdout: bool, fuel: u64, deadline: std::time::Duration, deadline_ms: u64, most: Option<u64>, reservation: u64, waiting: usize, ledger: Ledger }
/// One budget of a run that starts an instance for each connection: the whole run's `ceiling`
/// (`u64::MAX`: unlimited), each connection's `own` if it has one, and under the lock what
/// instances that have ended used, and what those running now have set aside. An instance
/// takes its share when it starts (`take`) and gives back what it did not use when it ends
/// (`settle`), so instances at once can never use more than the ceiling between them, and
/// none waits on another: a budget is not counted byte by byte across threads.
struct Pool { ceiling: u64, own: Option<u64>, counts: std::sync::Mutex<(u64, u64)> }
impl Pool {
    fn new(ceiling: u64, own: Option<u64>) -> Self { Self { ceiling, own, counts: std::sync::Mutex::new((0, 0)) } }
    fn counts(&self) -> std::sync::MutexGuard<'_, (u64, u64)> { self.counts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }
    /// One instance's share, set aside: a connection's own, or what is left of the run's if
    /// that is less (or there is no own), and whose it is said to be when it is reached.
    fn take(&self) -> (u64, Share) {
        let mut counts = self.counts();
        let room = self.ceiling.saturating_sub(counts.0.saturating_add(counts.1));
        let (amount, share) = match self.own { Some(own) if own <= room => (own, Share::Connection), _ => (room, Share::Rest(self.ceiling)) };
        counts.1 = counts.1.saturating_add(amount);
        (amount, share)
    }
    /// An instance that took `taken` has ended, having used `used` of it.
    fn settle(&self, taken: u64, used: u64) {
        let mut counts = self.counts();
        counts.1 = counts.1.saturating_sub(taken);
        counts.0 = counts.0.saturating_add(used.min(taken));
    }
    fn used(&self) -> u64 { self.counts().0 }
    /// Nothing is left that an instance not yet started could be given, once those running end.
    fn spent(&self) -> bool { self.counts().0 >= self.ceiling }
}
/// The budgets of a run that starts an instance for each connection.
struct Ledger { net: Pool, files: Pool, entries: Pool, output: Pool }
/// How many connections were answered, and how many of those ended in a trap or at a limit.
#[derive(Default, Debug, PartialEq)]
struct Served { connections: u64, failed: u64 }
impl State {
    /// The state for the next connection's instance: the grants, and nothing else. Every
    /// table of handles is empty (dropping the old ones closes the sockets, and abandons a
    /// file being written: its bytes never reached the directory), every scratch buffer is
    /// empty, every count is zero (the run's are the ledger's, `Pool`), and the memory limit
    /// is new. Written field by field, with no `..`: a field added to any of these has to be
    /// decided here.
    fn renew(self, reservation: u64) -> State {
        let State { edge_limit: _, hits: _, test_control: _, limits: _, interval: _, interval_started: _, output: _, output_limit: _, streamed, refill: _, active: _, input: _, taken: _, output_share: _, lines, source: _, files, network, programs: _, secrets, sqlite: _, local_service, agreements: _,
            env_variables, env_name: _, env_value: _, grants, grant_name: _, words, word: _,
            #[cfg(feature = "guest-profile")] samples } = self;
        let Files { named_dirs, root, write_root, file_read, file_write, read, write, secret_objects, data_root, staging, .. } = files;
        let files = Files { named_dirs, root, write_root, file_read, file_write, read, write, secret_objects, data_root, staging, ..Files::default() };
        let Network { allowed, budget: _, used: _, entries: _, share: _, listening: _, authority, connection: _, until: _ } = network;
        let network = Network { allowed, budget: 0, used: 0, entries: Vec::new(), share: Share::Run, listening: None, authority, connection: None, until: None };
        State { edge_limit: 0, hits: Default::default(), test_control: false,
            limits: Counted::new(reservation), interval: 0, interval_started: false,
            output: Vec::new(), output_limit: 0, streamed: streamed.map(|_| 0), refill: None, active: false, input: Vec::new(), taken: 0,
            output_share: Share::Run, lines: lines.map(|_| Vec::new()), source: None, files, network, programs: Programs::default(), secrets: secrets.for_connection(), sqlite: None, local_service: local_service.map(|grant| grant.fresh()), agreements: Default::default(),
            env_variables, env_name: Vec::new(), env_value: None, grants, grant_name: Vec::new(), words, word: None,
            #[cfg(feature = "guest-profile")] samples }
    }
    /// The grants again, for the worker thread that runs the instances while this thread
    /// accepts: each directory is the same directory through a descriptor of its own, and
    /// everything else is as `renew` leaves it. What is handed over once (a single file, a test's directories) is not
    /// given: a run with an instance for each connection has none.
    fn fork(&self, reservation: u64) -> std::io::Result<State> {
        let again = |dir: &Option<Dir>| dir.as_ref().map(Dir::try_clone).transpose();
        let files = Files { named_dirs: Vec::new(), root: again(&self.files.root)?, write_root: again(&self.files.write_root)?, file_read: None, file_write: None, read: self.files.read, write: self.files.write,
            secret_objects: self.files.secret_objects.clone(), data_root: again(&self.files.data_root)?, staging: again(&self.files.staging)?, ..Files::default() };
        let network = Network { allowed: self.network.allowed.clone(), budget: 0, used: 0, entries: Vec::new(), share: Share::Run, listening: None, authority: self.network.authority.clone(), connection: None, until: None };
        Ok(State { edge_limit: 0, hits: Default::default(), test_control: false,
            limits: Counted::new(reservation), interval: 0, interval_started: false,
            output: Vec::new(), output_limit: 0, streamed: self.streamed.map(|_| 0), refill: None, active: false, input: Vec::new(), taken: 0,
            output_share: Share::Run, lines: self.lines.as_ref().map(|_| Vec::new()), source: None, files, network, programs: Programs::default(), secrets: self.secrets.for_connection(), sqlite: None, local_service: self.local_service.as_ref().map(local_service::Grant::fresh), agreements: Default::default(),
            env_variables: self.env_variables.clone(), env_name: Vec::new(), env_value: None, grants: self.grants.clone(), grant_name: Vec::new(), words: self.words.clone(), word: None,
            #[cfg(feature = "guest-profile")] samples: Default::default() })
    }
}
/// The clock for one instance at a time (each of the run's workers has one). One thread;
/// `begin` gives it a time and the connection's socket, and when that time comes it raises
/// the engine's epoch and shuts the socket down (a read or write the instance is waiting in
/// returns). The epoch is the engine's, raised for every instance at once: each store asks
/// its own deadline when it is (`epoch_deadline_callback` in `serve_one`) and only the one
/// whose time has come stops. `end` takes the time away under the same lock the thread holds
/// while it acts, so it never shuts a socket for a connection that is over.
struct ConnectionClock { shared: std::sync::Arc<(std::sync::Mutex<ClockSlot>, std::sync::Condvar)> }
#[derive(Default)]
struct ClockSlot { due: Option<(std::time::Instant, Option<std::net::TcpStream>)>, fired: bool, closed: bool }
impl ConnectionClock {
    fn start(engine: &Engine) -> Self {
        let shared = std::sync::Arc::new((std::sync::Mutex::new(ClockSlot::default()), std::sync::Condvar::new()));
        let (watched, engine) = (shared.clone(), engine.clone());
        std::thread::spawn(move || {
            let (slot, changed) = &*watched;
            let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                // Ends with its clock: a run's workers end when the run does.
                if slot.closed { return; }
                let left = slot.due.as_ref().map(|(at, _)| at.saturating_duration_since(std::time::Instant::now()));
                slot = match left {
                    None => changed.wait(slot).unwrap_or_else(|poisoned| poisoned.into_inner()),
                    Some(left) if !left.is_zero() => changed.wait_timeout(slot, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0,
                    Some(_) => {
                        if let Some((_, Some(stream))) = slot.due.take() { let _ = stream.shutdown(std::net::Shutdown::Both); }
                        slot.fired = true;
                        engine.increment_epoch();
                        slot
                    }
                };
            }
        });
        Self { shared }
    }
    fn begin(&self, at: std::time::Instant, stream: Option<std::net::TcpStream>) {
        let mut slot = self.shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = ClockSlot { due: Some((at, stream)), fired: false, closed: false };
        self.shared.1.notify_all();
    }
    /// The connection is over: whether its time ran out.
    fn end(&self) -> bool {
        let mut slot = self.shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.due = None;
        std::mem::take(&mut slot.fired)
    }
}
impl Drop for ConnectionClock { fn drop(&mut self) { self.shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).closed = true; self.shared.1.notify_all(); } }
/// What the acceptor and the workers of `serve_connections` share: the accepted connections
/// no instance has been started for yet, in the order they came; `closed`, no more will come;
/// `failed`, the run's error (the first), which ends it; what instances that finished wrote,
/// where output is kept for the end of the run; and the counts.
#[derive(Default)]
struct Waiting { held: std::sync::Mutex<Held>, changed: std::sync::Condvar, connections: std::sync::atomic::AtomicU64, failed: std::sync::atomic::AtomicU64 }
#[derive(Default)]
struct Held { queue: std::collections::VecDeque<NetEntry>, closed: bool, failed: Option<wasmtime::Error>, kept: Vec<u8> }
impl Waiting {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> { self.held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }
    /// The run's error: the first is kept, nothing that waits is answered, and everyone wakes.
    fn fail(&self, error: wasmtime::Error, wake: &std::io::PipeWriter) {
        let mut held = self.held();
        if held.failed.is_none() { held.failed = Some(error); }
        held.queue.clear();
        drop(held);
        self.changed.notify_all();
        let _ = (&*wake).write(&[1]);
    }
}
/// `--instance-per-connection`: the engine accepts on the granted address and runs the program
/// (`run`, its `main`) once for each connection, in a store and instance made for it: new
/// memory, new handle tables, its own instruction count (`--fuel`), its own deadline
/// (`--connection-deadline-ms`) and its own share of each budget (`Pool`). `main` takes the
/// connection as its `Socket` parameter (`net_authorize(2)`). A trap, the count or the
/// deadline ends that instance and closes that connection; the next connection starts from
/// the module as built, so nothing a stopped instance left half done is ever seen again
/// (docs/decisions.md, 2026-11-22, "An instance for each connection"). Between connections
/// there is only what the host holds: files in a granted directory, and the run's budgets.
///
/// This thread accepts, and holds up to `plan.waiting` connections (beyond the system's own
/// queue, which may be as small as 128) for one worker, which runs one instance at a time.
///
/// Ends: after `most` connections; with an error when the network budget is used up (no
/// connection could be answered) or accepting keeps failing; otherwise with the process (the
/// run's deadline, the lifeline). An instance that cannot be made at all is the run's error.
fn serve_connections(engine: &Engine, module: &Module, linker: &Linker<State>, listener: &std::net::TcpListener, mut state: State, plan: &PerConnection) -> wasmtime::Result<(State, Served)> {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering::SeqCst;
    let worker = state.fork(plan.reservation).map_err(|_| reject("the granted directory cannot be opened again for an instance"))?;
    let waiting = Waiting::default();
    // A worker that ends the run wakes this thread from its wait for a connection.
    let (woken, wake) = std::io::pipe().map_err(|_| reject("no pipe for the run's workers"))?;
    if plan.ready_stdout {
        let mut ready = std::io::stdout().lock();
        ready.write_all(b"READY\n").and_then(|()| ready.flush()).map_err(|_| reject("listener readiness could not be reported"))?;
    }
    let far = 3_600_000;
    let ended = std::thread::scope(|scope| {
        {
            let (waiting, wake) = (&waiting, &wake);
            scope.spawn(move || {
                // A worker that panics ends the run, as an error does, rather than leave the
                // acceptor waiting for a worker that is gone (review).
                struct Gone<'a>(&'a Waiting, &'a std::io::PipeWriter);
                impl Drop for Gone<'_> { fn drop(&mut self) { if std::thread::panicking() { self.0.fail(reject("the instance's worker stopped"), self.1); } } }
                let _gone = Gone(waiting, wake);
                serve_from(engine, module, linker, worker, plan, waiting, wake)
            });
        }
        let accept = || -> wasmtime::Result<()> {
            let (mut failing, mut accepted) = (0, 0u64);
            loop {
                {
                    // Room for one more, or the run has ended.
                    let mut held = waiting.held();
                    loop {
                        if held.failed.is_some() { return Ok(()); }
                        if held.queue.len() < plan.waiting.max(1) { break; }
                        held = waiting.changed.wait(held).unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                }
                if plan.most.is_some_and(|most| accepted >= most) { return Ok(()); }
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        let mut fds = [sys::PollFd { fd: listener.as_raw_fd(), events: sys::POLLIN, revents: 0 }, sys::PollFd { fd: woken.as_raw_fd(), events: sys::POLLIN, revents: 0 }];
                        if sys::poll(&mut fds, far).is_err() { failing += 1; if failing >= 64 { return Err(reject("waiting for connections failed 64 times in a row")); } }
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // A connection gone before it was accepted, or no descriptor or memory for one
                    // now (the instance at work holds descriptors too: its directories), is no
                    // failure of the listener: it waits a little and tries again, uncounted, so
                    // one instance cannot end the service this way (review). One that fails
                    // otherwise every time is, and the run does not spin on it.
                    Err(e) if matches!(e.raw_os_error(), Some(code) if sys::TRANSIENT_ACCEPT.contains(&code)) => { std::thread::sleep(std::time::Duration::from_millis(10)); continue; }
                    Err(_) => {
                        failing += 1;
                        if failing >= 64 { return Err(reject("accepting connections failed 64 times in a row")); }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                };
                failing = 0;
                // A connection already gone (reset before it was accepted) starts no instance: the
                // program is never handed a `Socket` that is not one.
                let connection = Network::accepted(stream);
                if !matches!(connection, NetEntry::Socket { .. }) { continue; }
                accepted += 1;
                waiting.held().queue.push_back(connection);
                waiting.changed.notify_all();
            }
        };
        let ended = accept();
        // No more connections: the workers answer what waits (nothing, after an error), and end.
        let mut held = waiting.held();
        held.closed = true;
        if ended.is_err() { held.queue.clear(); }
        drop(held);
        waiting.changed.notify_all();
        ended
    });
    let served = Served { connections: waiting.connections.load(SeqCst), failed: waiting.failed.load(SeqCst) };
    let mut held = waiting.held.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
    ended?;
    if let Some(error) = held.failed.take() { return Err(error); }
    // What the run used, in the state its caller reads, and what its instances wrote.
    (state.network.used, state.files.used, state.files.made) = (plan.ledger.net.used(), plan.ledger.files.used(), plan.ledger.entries.used());
    state.output = held.kept;
    Ok((state, served))
}
/// One worker: an instance for each connection it takes from those that wait, one at a time.
fn serve_from(engine: &Engine, module: &Module, linker: &Linker<State>, mut state: State, plan: &PerConnection, waiting: &Waiting, wake: &std::io::PipeWriter) {
    let clock = ConnectionClock::start(engine);
    let used_up = || {
        let _ = Network::full(plan.ledger.net.ceiling, Share::Run);
        reject("the network budget is used up: no further connection can be answered, and the run ends")
    };
    loop {
        let connection = {
            let mut held = waiting.held();
            loop {
                if held.failed.is_some() { return; }
                if let Some(connection) = held.queue.pop_front() { break connection; }
                if held.closed { return; }
                held = waiting.changed.wait(held).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        // There is room for the acceptor's next.
        waiting.changed.notify_all();
        if plan.ledger.net.spent() { return waiting.fail(used_up(), wake); }
        match serve_one(engine, module, linker, state, connection, plan, &clock, waiting) {
            Ok(next) => state = next,
            Err(error) => return waiting.fail(error, wake),
        }
        if plan.ledger.net.spent() { return waiting.fail(used_up(), wake); }
    }
}
/// One connection: its instance, from the module as built, with its own share of each
/// budget, until `main` returns or the instance is stopped. Everything it held is closed
/// when this returns, its connection first. The error is the run's (an instance that cannot
/// be made), never the connection's: that is said, counted, and the server goes on.
#[allow(clippy::too_many_arguments)]
fn serve_one(engine: &Engine, module: &Module, linker: &Linker<State>, state: State, connection: NetEntry, plan: &PerConnection, clock: &ConnectionClock, waiting: &Waiting) -> wasmtime::Result<State> {
    use std::sync::atomic::Ordering::SeqCst;
    let watched = match &connection { NetEntry::Socket { stream, .. } => stream.try_clone().ok(), _ => None };
    let mut state = state.renew(plan.reservation);
    let ledger = &plan.ledger;
    let (net, files, entries, output) = (ledger.net.take(), ledger.files.take(), ledger.entries.take(), ledger.output.take());
    (state.network.budget, state.network.share) = net;
    (state.files.budget, state.files.share) = files;
    (state.files.entry_limit, state.files.entry_share) = entries;
    (state.output_limit, state.output_share) = (usize::try_from(output.0).unwrap_or(usize::MAX), output.1);
    let until = std::time::Instant::now() + plan.deadline;
    state.network.connection = Some(connection);
    state.network.until = Some(until);
    let mut store = Store::new(engine, state);
    store.limiter(|state| &mut state.limits);
    // An instance that cannot be made (an import the run does not grant) is the same for
    // every connection: the run's error, not this connection's.
    let made = (|| -> wasmtime::Result<_> {
        store.set_fuel(plan.fuel)?;
        store.set_epoch_deadline(1);
        // The epoch is raised when any instance's time is up: this one stops only at its own.
        store.epoch_deadline_callback(move |_| if std::time::Instant::now() >= until { Err(wasmtime::Trap::Interrupt.into()) } else { Ok(wasmtime::UpdateDeadline::Continue(1)) });
        let instance = linker.instantiate(&mut store, module)?;
        Ok(instance.get_typed_func::<(), ()>(&mut store, "run")?)
    })();
    let ran = made.map(|run| {
        clock.begin(until, watched);
        store.data_mut().active = true;
        let ended = run.call(&mut store, ());
        (ended, clock.end())
    });
    let instructions = plan.fuel.saturating_sub(store.get_fuel().unwrap_or(0));
    let mut state = store.into_data();
    // Output is committed only for an instance that finished (when it is kept at all); what
    // was streamed has gone out, and a last line with no end is given one, so that no line
    // of this connection's runs into the next one's.
    let finished = matches!(&ran, Ok((Ok(()), _)));
    let wrote = match state.streamed {
        Some(count) => {
            if let Some(line) = state.lines.as_mut().filter(|line| !line.is_empty()) { line.push(b'\n'); let _ = stream::lines(line); line.clear(); }
            count as u64
        }
        None if finished => { let kept = std::mem::take(&mut state.output); let count = kept.len() as u64; waiting.held().kept.extend_from_slice(&kept); count }
        None => 0,
    };
    // What this instance used leaves its share; the rest goes back to the run.
    let used = [state.network.used, state.files.used, state.files.made, wrote];
    for (pool, taken, used) in [(&ledger.net, net.0, used[0]), (&ledger.files, files.0, used[1]), (&ledger.entries, entries.0, used[2]), (&ledger.output, output.0, used[3])] { pool.settle(taken, used); }
    usage::settled([ledger.net.used(), ledger.files.used(), ledger.entries.used(), ledger.output.used()], used, state.limits.bytes as u64);
    // Everything the instance held is closed now, its connection first, not when the next
    // connection comes: a client of a stopped instance is not left waiting on an open socket.
    let state = state.renew(plan.reservation);
    let (ended, timed_out) = ran?;
    usage::interval(false, instructions);
    usage::connection(ended.is_err());
    let number = waiting.connections.fetch_add(1, SeqCst) + 1;
    if let Err(error) = ended {
        let failed = waiting.failed.fetch_add(1, SeqCst) + 1;
        let trap = error.downcast_ref::<wasmtime::Trap>().copied();
        if timed_out || trap == Some(wasmtime::Trap::Interrupt) {
            let ms = plan.deadline_ms;
            limit::reached(limit::CONNECTION, || format!("connection deadline: a connection's instance was stopped after its {ms} ms ({}); that connection is closed and the server goes on", limit::name("--connection-deadline-ms")));
        } else if trap == Some(wasmtime::Trap::OutOfFuel) {
            let count = plan.fuel;
            limit::reached(limit::FUEL, || format!("instructions: a connection's instance ran out of its {count} ({}); that connection is closed and the server goes on", limit::name("--fuel")));
        } else if failed <= 8 {
            // The engine's own words (a trap's kind, or the host's refusal), then where the
            // program stopped and at which check, as a program run once is told. Made plain
            // as output is (a refusal may quote a name the module sent): no control,
            // reordering or hidden character, one line, 200 characters.
            let why: String = match trap { Some(trap) => trap.to_string(), None => error.root_cause().to_string() };
            let mut lines = vec![format!("connection {number} ended: {}; it is closed and the server goes on{}", shown_plainly(why.lines().next().unwrap_or("")), if failed == 8 { " (further ones are counted, not said)" } else { "" })];
            lines.extend(stopped_where(&error, &format!("connection {number}")));
            // One write: the lines of two connections that ended together stay apart.
            said::line(&lines.join("\n"));
        }
    }
    Ok(state)
}
/// Text the engine quotes in a line of its own, made plain as output is, on one line, 200 characters.
fn shown_plainly(text: &str) -> String {
    let plain = String::from_utf8_lossy(&stream::plain(text.as_bytes(), &mut true)).into_owned();
    plain.chars().filter(|c| !c.is_control()).take(200).collect()
}
/// Where a trap stopped the program, in the program's words, from the module's function names
/// (the compiler's `name` section): the function that stopped and, when that was one of the
/// compiler's own checks (`witchy.overflow +`), why; then its callers. `what`: `the run`, or
/// `connection N`. Nothing for an error that is not a trap, or a module without names.
fn stopped_where(error: &wasmtime::Error, what: &str) -> Vec<String> {
    use std::borrow::Cow;

    let (Some(trap), Some(trace)) = (error.downcast_ref::<wasmtime::Trap>(), error.downcast_ref::<wasmtime::WasmBacktrace>()) else { return Vec::new() };
    let names: Vec<String> = trace.frames().iter().filter_map(|frame| frame.func_name()).map(shown_plainly).collect();
    let Some(first) = names.first() else { return Vec::new() };
    // Only the compiler's own helpers' names give a reason; another `witchy.` name
    // is just a name (review: a module chose the reason's words).
    let helper = first.strip_prefix("witchy.").filter(|check| matches!(*check, "overflow +" | "overflow -" | "overflow *" | "too large for U8" | "too large for U16" | "too large for U32"));
    let (why, place) = match helper {
        Some(check) => (Cow::Borrowed(check), names.get(1)),
        None => {
            let why = trace.frames().first().and_then(|frame| frame.module_offset()).and_then(trap_reason)
                .unwrap_or_else(|| match trap {
                    wasmtime::Trap::UnreachableCodeReached => Cow::Borrowed("a check failed: an index out of range, a division by zero, an `unreachable`, or a task used wrongly"),
                    wasmtime::Trap::IntegerDivisionByZero => Cow::Borrowed("a division by zero"),
                    wasmtime::Trap::IntegerOverflow => Cow::Borrowed("an overflow"),
                    wasmtime::Trap::StackOverflow => Cow::Borrowed("the stack ran out (a recursion too deep)"),
                    other => Cow::Owned(other.to_string()),
                });
            (why, Some(first))
        }
    };
    let mut lines = vec![match place {
        Some(place) => format!("{what} stopped in `{place}`: {why}"),
        None => format!("{what} stopped: {why}"),
    }];
    if names.len() > 1 { lines.push(format!("called from: {}", names.iter().skip(if helper.is_some() { 2 } else { 1 }).map(|n| format!("`{n}`")).collect::<Vec<_>>().join(" <- "))); }
    lines
}
/// Every variable a secret flag names (`--secret-slot=NAME:USE:env:VARIABLE`, and a store's
/// `--secret=NAME` or `--secret-seal=NAME` without a value), taken out of this process's
/// environment before anything else runs: unset, and its bytes overwritten where the process
/// was started with them. The flags are checked later; a name that is not a plain variable
/// name is not taken.
fn take_secret_variables(args: &[String]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut taken = std::collections::BTreeMap::new();
    let plain = |name: &str| name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && Secrets::plain(name);
    for arg in args {
        let arg = arg.as_str();
        let named = if let Some(text) = arg.strip_prefix("--secret-slot=") {
            let mut parts = text.splitn(4, ':');
            match (parts.next(), parts.next(), parts.next(), parts.next()) { (Some(_), Some(_), Some("env"), Some(variable)) => Some(variable), _ => None }
        } else {
            arg.strip_prefix("--secret=").or_else(|| arg.strip_prefix("--secret-seal=")).filter(|name| !name.contains('='))
        };
        if let Some(name) = named.filter(|name| plain(name) && !taken.contains_key(*name)) {
            if let Some(value) = sys::take_variable(name) { taken.insert(name.to_owned(), value); }
        }
    }
    taken
}
/// The command line as text, or which argument is not: `std::env::args` panics on one.
fn text_arguments(given: impl Iterator<Item = std::ffi::OsString>) -> Result<Vec<String>, usize> {
    given.enumerate().map(|(at, word)| word.into_string().map_err(|_| at)).collect()
}
fn main() {
    let mut raw = std::env::args_os().skip(1);
    if raw.next().as_deref() == Some(std::ffi::OsStr::new(exec_child::WORKER)) {
        std::process::exit(exec_child::worker(raw));
    }
    // Before anything reads the command line: every argument is UTF-8 text, or the run is
    // refused in one line (no panic is reachable from what an operator writes).
    let args = match text_arguments(std::env::args_os()) {
        Ok(args) => args,
        Err(at) => { say!("invalid grant: argument {at} of the command line is not UTF-8 text"); std::process::exit(1); }
    };
    if args.get(1).map(String::as_str) == Some("--test-session") {
        if let Err(error) = test_batch::serve() { say!("test host: {error}"); std::process::exit(2); }
        return;
    }
    #[cfg(feature = "resident")]
    if let Some(code) = resident::serve_if_asked() { std::process::exit(code); }
    let code = host_main(&args, &[], &mut compile_fresh);
    if code != 0 { std::process::exit(code); }
}
/// One run, from the process's start to its exit status: what `main` does with the command
/// line, and what the resident runner's forked process does with a request's (`resident.rs`).
/// `own`: descriptors the host holds for itself (`close_inherited_on_exec`).
fn host_main(args: &[String], own: &[u32], compile: Compile<'_>) -> i32 {
    // First, while this is the only thread: no program a module starts inherits a descriptor
    // this process was started with, or reads a secret's variable from this process.
    if sys::close_inherited_on_exec(own).is_err() { say!("cannot list the descriptors this process was started with (/dev/fd)"); return 1; }
    let variables = take_secret_variables(args);
    // On Linux, no process of the same user may read this one's memory or initial environment
    // (`/proc/PID/mem`, `environ`, ptrace), whatever the Yama setting: a program the module
    // starts runs as the same user. Not checked on macOS, where this is compiled out.
    #[cfg(target_os = "linux")]
    if sys::not_dumpable().is_err() { say!("cannot make this process non-dumpable (prctl PR_SET_DUMPABLE)"); return 1; }
    // The whole run, engine compilation and blocked input and output included: one second
    // unless granted more.
    let deadline = args.iter().find_map(|arg| arg.strip_prefix("--deadline-ms=").map(|text| text.parse::<u64>()));
    let Some(stopped) = sys::stopped_by_signal() else {
        say!("cannot open the shutdown signal pipe");
        return 1;
    };
    if !sys::install_shutdown_signal_handlers() {
        say!("cannot install shutdown signal handlers");
        return 1;
    }
    // `--lifeline=fd:N`: a pipe whose other end the launcher holds and never writes. When it
    // closes (the launcher is gone, however it went) the run ends, with whatever it started.
    // Watched by the engine itself, so nothing between the launcher and the engine can be
    // killed to leave the engine running.
    let lifeline = match args.iter().filter_map(|arg| arg.strip_prefix("--lifeline=")).collect::<Vec<_>>().as_slice() {
        [] => None,
        [text] => match text.strip_prefix("fd:").and_then(|n| n.parse::<u32>().ok()).filter(|n| (3..=1023).contains(n) && (n.to_string() == text[3..])) {
            Some(number) if sys::is_pipe(number) => Some(number),
            _ => { say!("invalid grant: --lifeline takes fd:N, an open pipe's descriptor from 3 to 1023"); return 1; }
        },
        _ => { say!("invalid grant: --lifeline given twice"); return 1; }
    };
    // `--deadline-ms=none`: no deadline, for a service. Only with a lifeline: a run with no
    // end of its own must end with its launcher.
    let unbounded = args.iter().any(|arg| arg == "--deadline-ms=none");
    if unbounded && (lifeline.is_none() || args.iter().filter(|arg| arg.starts_with("--deadline-ms=")).count() != 1) {
        say!("invalid grant: --deadline-ms=none needs --lifeline=fd:N (a run with no deadline ends when its launcher does), and is given once");
        return 1;
    }
    let deadline = match deadline { _ if unbounded => 0, None => 1000, Some(Ok(value)) if (1..=600_000).contains(&value) => value, Some(_) => { say!("invalid grant: --deadline-ms takes 1 to 600000, or none with --lifeline"); return 1; } };
    use std::sync::atomic::{AtomicBool, Ordering};
    static ENDING: AtomicBool = AtomicBool::new(false);
    let ending = |why: &str, status: i32| -> ! {
        // Once: the deadline's, the lifeline's and a signal's threads may all come here; the
        // first ends the run, and the others wait for it (review).
        if ENDING.swap(true, Ordering::SeqCst) { loop { std::thread::park() } }
        // Whatever the lines below wait on (a standard output or error nobody reads, held by
        // a thread stopped in a write), the run is over a quarter of a second from now; with
        // no thread for that, now.
        exec_child::stop();
        let timer = std::thread::Builder::new().spawn(move || { std::thread::sleep(std::time::Duration::from_millis(250)); sys::exit_now(status) });
        if timer.is_err() { sys::exit_now(status) }
        // What the program wrote and had not yet ended with a newline, then why the run ended.
        stream::finish(false);
        // Never `eprintln!`: it panics where standard error is closed, the panic would end
        // only this thread, and the run would outlive its deadline or its launcher (review).
        said::line(why);
        usage::say();
        std::process::exit(status) };
    if let Some(number) = lifeline {
        let Ok(descriptor) = sys::take_descriptor(number) else {
            say!("invalid grant: --lifeline requires its own inherited descriptor");
            return 1;
        };
        std::thread::spawn(move || {
            let mut pipe = std::fs::File::from(descriptor);
            let mut byte = [0u8; 64];
            loop { match pipe.read(&mut byte) { Ok(0) => break, Ok(_) => {}, Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}, Err(_) => break } }
            ending("the launcher is gone: the run ends with it", 125) });
    }
    // A signal that stops the run (SIGINT, SIGTERM): what the run used is said, as when it
    // ends any other way. The status is 130 for SIGINT, and 124 for SIGTERM, as it was before
    // this said anything (a launcher's stop; launchers read 124 as "stopped from outside").
    std::thread::spawn(move || {
        let number = stopped();
        ending(if number == 2 { "stopped: interrupted (SIGINT); the run ends" } else { "stopped: terminated (SIGTERM); the run ends" }, if number == 2 { 130 } else { 124 }) });
    let _ = DEADLINE_AT.set(std::time::Instant::now() + if unbounded { std::time::Duration::from_secs(3_153_600_000) } else { std::time::Duration::from_millis(deadline) });
    if !unbounded {
        std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(deadline));
            let _ = limit::first(limit::DEADLINE);
            ending(&format!("timeout: Wasmtime deadline exceeded\nlimit reached: deadline: the run was stopped after its {deadline} ms ({})", limit::name("--deadline-ms")), 124) });
    }
    let ended = execute(args, variables, compile, None).and_then(|out| Ok(std::io::stdout().write_all(&out)?));
    if ended.is_ok() { usage::say(); }
    if let Err(error) = ended {
        // The whole chain: a host function's refusal is the cause under the trap.
        say!("{error:#}");
        // A backtrace alone does not say why the run stopped: name the trap.
        if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
            say!("trap: {trap}");
            // In the program's words: the function that stopped and at which check (`stopped_where`).
            for line in stopped_where(&error, "the run") { say!("{line}"); }
            if *trap == wasmtime::Trap::OutOfFuel {
                let per_accept = args.iter().find_map(|arg| arg.strip_prefix("--fuel-per-accept="));
                let [advice, reached] = limit::fuel_lines(per_accept, &limit::name);
                say!("{advice}");
                limit::reached(limit::FUEL, || reached);
            }
        }
        usage::say();
        if ENDING.load(Ordering::SeqCst) { loop { std::thread::park() } }
        return 1;
    }
    if ENDING.load(Ordering::SeqCst) { loop { std::thread::park() } }
    0
}

/// Development only (`--features guest-profile`): where a run spends its time, by sampling the
/// guest's call stack from an epoch callback.
#[cfg(feature = "guest-profile")]
mod profile_guest {
    use std::collections::BTreeMap;
    #[derive(Default)]
    pub struct Samples { pub leaf: BTreeMap<String, u64>, pub within: BTreeMap<String, u64>, pub total: u64 }
    pub fn wanted() -> bool { std::env::var_os("WITCHY_GUEST_PROFILE").is_some() }
    pub fn start(engine: &wasmtime::Engine, store: &mut wasmtime::Store<super::State>) {
        if !wanted() { return; }
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|mut context| {
            let trace = wasmtime::WasmBacktrace::force_capture(&context);
            let names: Vec<String> = trace.frames().iter().map(|frame| frame.func_name().map(str::to_string).unwrap_or_else(|| format!("#{}", frame.func_index()))).collect();
            let samples = &mut context.data_mut().samples;
            samples.total += 1;
            if let Some(first) = names.first() { *samples.leaf.entry(first.clone()).or_default() += 1; }
            let mut seen = std::collections::BTreeSet::new();
            for name in names { if seen.insert(name.clone()) { *samples.within.entry(name).or_default() += 1; } }
            Ok(wasmtime::UpdateDeadline::Continue(1))
        });
        let engine = engine.clone();
        std::thread::spawn(move || loop { std::thread::sleep(std::time::Duration::from_micros(100)); engine.increment_epoch(); });
    }
    pub fn report(samples: &Samples) {
        if !wanted() || samples.total == 0 { return; }
        for (title, table) in [("self", &samples.leaf), ("total", &samples.within)] {
            let mut rows: Vec<_> = table.iter().collect();
            rows.sort_by(|a, b| b.1.cmp(a.1));
            say!("profile {title} ({} samples):", samples.total);
            for (name, count) in rows.into_iter().take(25) { say!("  {:5.1}%  {name}", *count as f64 * 100.0 / samples.total as f64); }
        }
    }
}

/// Bytes from AWS-LC's FIPS DRBG (SP 800-90A, seeded from the operating system), drawn in
/// blocks of 256; no byte is given out twice.
fn drawn(out: &mut [u8]) -> std::io::Result<()> {
    use std::cell::RefCell;
    struct Pool { block: [u8; 256], used: usize }
    thread_local! { static POOL: RefCell<Pool> = const { RefCell::new(Pool { block: [0; 256], used: 256 }) }; }
    POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        for byte in out.iter_mut() {
            if pool.used == pool.block.len() {
                let Pool { block, used } = &mut *pool;
                sha::provider().and_then(|()| sha::fill_random(block).map_err(|()| "randomness unavailable")).map_err(std::io::Error::other)?;
                *used = 0;
            }
            let at = pool.used;
            *byte = pool.block[at];
            // Spent: not left readable in the pool.
            pool.block[at] = 0;
            pool.used += 1;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_directory_alias_stays_with_its_read_root() {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-alias-{}", std::process::id()));
        std::fs::create_dir_all(base.join("source")).unwrap();
        std::fs::create_dir_all(base.join("held")).unwrap();
        let source = Dir::open_ambient_dir(base.join("source"), cap_std::ambient_authority()).unwrap();
        let held = Dir::open_ambient_dir(base.join("held"), cap_std::ambient_authority()).unwrap();
        let entry = || Entry::Dir { dir: source.try_clone().unwrap(), read: true, write: false, pending: b"incoming".to_vec(), from: Vec::new() };
        let mut files = Files { read_aliases: vec![(b"incoming".to_vec(), held)], ..Files::default() };
        let root = files.issue(entry()).unwrap();
        files.alias_root = Some(root);
        assert!(files.open_dir(root, 1).is_err());
        let alias = files.open_dir(root, 0).unwrap();
        assert!(matches!(files.entry(alias).unwrap(), Entry::Dir { read: true, write: false, .. }));
        files.close(root).unwrap();
        let reused = files.issue(entry()).unwrap();
        assert_eq!(reused, root);
        let absent = files.open_dir(reused, 0).unwrap();
        assert!(matches!(files.entry(absent).unwrap(), Entry::Failed(NOT_FOUND)));
        std::fs::remove_dir_all(base).unwrap();
    }

    /// `--no-links` opens a real directory, and refuses a link at the end of the path or on the
    /// way to it, and a path that is not absolute plain names.
    /// The engine reads the same refusal list as the compiler and the native broker: each
    /// prefix class and some exact names are refused, ordinary names are not.
    #[test]
    fn exec_env_refusal_list_is_shared() {
        for name in ["LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "MALLOC_CONF", "GIT_DIR", "PYTHONPATH", "PERL5LIB", "RUBYLIB", "NODE_OPTIONS", "BASH_ENV",
                     "PATH", "IFS", "HOME", "TMPDIR", "SSL_CERT_DIR", "LESSOPEN", "EDITOR"] {
            assert!(super::exec_env_refused(name), "{name}");
        }
        for name in ["LANG", "LC_ALL", "TZ", "TERM", "PATHS", "MY_LD_FLAG"] {
            assert!(!super::exec_env_refused(name), "{name}");
        }
    }

    #[test]
    fn a_granted_directory_without_links_is_opened_only_through_real_directories() {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-links-{}", std::process::id()));
        std::fs::create_dir_all(base.join("real/inner")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        assert!(open_without_links(&base.join("real/inner")).is_ok());
        assert!(open_without_links(&base.join("link")).is_err());
        assert!(open_without_links(&base.join("link/inner")).is_err());
        assert!(open_without_links(&base.join("real/../real")).is_err());
        assert!(open_without_links(std::path::Path::new("relative")).is_err());
        // Without the flag, the link is followed (what `--dir` has always done).
        assert!(Dir::open_ambient_dir(base.join("link"), cap_std::ambient_authority()).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A path below a granted directory takes no link, even one whose target is inside the grant
    /// (cap-std alone would follow that one), and is refused as written for `..` or `.` even
    /// where it would stay inside.
    #[test]
    fn a_path_takes_no_link_even_inside_the_grant() {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-inner-links-{}", std::process::id()));
        std::fs::create_dir_all(base.join("notes")).unwrap();
        std::fs::write(base.join("notes/one.txt"), b"one").unwrap();
        std::os::unix::fs::symlink("notes", base.join("inner")).unwrap();
        std::os::unix::fs::symlink("notes/one.txt", base.join("one-link")).unwrap();
        let root = Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        let mut files = Files { budget: 1 << 20, entry_limit: 64, ..Files::default() };
        let mut read = |path: &str| { let (parent, name) = Files::resolve(&root, path.as_bytes())?; files.read_whole(&parent, &name).unwrap() };
        assert_eq!(read("notes/one.txt"), Ok(b"one".to_vec()));
        assert_eq!(read("inner/one.txt"), Err(INVALID_PATH));
        assert_eq!(read("one-link"), Err(INVALID_PATH));
        assert_eq!(read("notes/../notes/one.txt"), Err(INVALID_PATH));
        assert_eq!(read("./notes/one.txt"), Err(INVALID_PATH));
        assert_eq!(read("notes/one.txt/x"), Err(WRONG_KIND));
        std::fs::remove_dir_all(base).unwrap();
    }

    /// Time of check against time of use: while another thread keeps swapping a directory on the
    /// path for a link to a directory outside the grant (and back), every open through the grant
    /// gives the file inside or a refusal, never the file outside. Each step is opened from the
    /// `Dir` before it, so there is no moment between a check and a use for the swap to land in.
    #[test]
    fn a_directory_swapped_for_a_link_while_running_is_never_followed() {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-swap-{}", std::process::id()));
        std::fs::create_dir_all(base.join("grant/sub")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("grant/sub/file.txt"), b"inside").unwrap();
        std::fs::write(base.join("outside/file.txt"), b"OUTSIDE").unwrap();
        let root = Dir::open_ambient_dir(base.join("grant"), cap_std::ambient_authority()).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let swapper = {
            let (base, stop) = (base.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut swaps = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::fs::rename(base.join("grant/sub"), base.join("grant/sub-real")).unwrap();
                    std::os::unix::fs::symlink(base.join("outside"), base.join("grant/sub")).unwrap();
                    std::fs::remove_file(base.join("grant/sub")).unwrap();
                    std::fs::rename(base.join("grant/sub-real"), base.join("grant/sub")).unwrap();
                    swaps += 1;
                }
                swaps
            })
        };
        let mut files = Files { budget: 1 << 40, entry_limit: 64, ..Files::default() };
        let (mut inside, mut refused) = (0u64, 0u64);
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(10) && (inside < 1000 || refused < 1000) {
            match Files::resolve(&root, b"sub/file.txt").and_then(|(parent, name)| files.read_whole(&parent, &name).unwrap()) {
                Ok(data) => { assert_eq!(data, b"inside", "a read through the grant reached outside it"); inside += 1; }
                // Which refusal depends on when the swap lands: a link found (invalid path), the
                // name missing between two renames, or, when the link is gone again by the time
                // the failed step is classified, the wrong kind. Never anything read.
                Err(code) => { assert!([NOT_FOUND, INVALID_PATH, WRONG_KIND].contains(&code), "unexpected code {code}"); refused += 1; }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let swaps = swapper.join().unwrap();
        assert!(swaps > 0 && inside > 0 && refused > 0, "the race did not happen: {swaps} swaps, {inside} reads, {refused} refusals");
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A grant stays on its directory's file system. `/dev` is a real mount point below `/` (its
    /// own file system on macOS and Linux, no privilege needed to see one), so stepping onto it
    /// is refused through every route: on the way to a file, and as a subtree opened. And with a
    /// device that is not the tree's (as the granted root's would be after something is mounted
    /// over a directory inside it), every step and read in an ordinary tree is refused too.
    #[test]
    fn a_path_never_steps_onto_another_file_system() {
        use cap_std::fs::MetadataExt;
        let root = Dir::open_ambient_dir("/", cap_std::ambient_authority()).unwrap();
        let here = device(&root).unwrap();
        assert_ne!(root.open_dir_nofollow("dev").unwrap().dir_metadata().unwrap().dev(), here, "this test needs /dev to be a mount point");
        assert!(open_dir_on(&root, "dev".as_ref(), here).is_err());
        assert_eq!(Files::resolve(&root, b"dev/null").err(), Some(INVALID_PATH));
        let mut files = Files { budget: 1 << 20, entry_limit: 64, ..Files::default() };
        let granted = files.issue(Entry::Dir { dir: root, read: true, write: false, pending: b"dev".to_vec(), from: Vec::new() }).unwrap();
        let subtree = files.open_dir(granted, 0).unwrap();
        assert!(matches!(files.entry(subtree).unwrap(), Entry::Failed(INVALID_PATH)));
        // What is at a name (before a probe, a write, a remove or a rename) is held to it too.
        let Entry::Dir { dir, pending, .. } = files.entry(granted).unwrap() else { unreachable!() };
        assert_eq!(Files::kind(dir, "dev".as_ref()), Err(INVALID_PATH));
        assert_eq!(Files::kind(dir, "usr".as_ref()), Ok(2));
        *pending = b"dev".to_vec();
        assert_eq!(files.probe(granted).unwrap(), 0);

        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-device-{}", std::process::id()));
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("sub/file.txt"), b"inside").unwrap();
        let root = Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        let (here, elsewhere) = (device(&root).unwrap(), device(&root).unwrap().wrapping_add(1));
        let (parent, name) = Files::resolve_on(&root, here, b"sub/file.txt").unwrap();
        assert_eq!(files.read_on(&parent, &name, Some(here)).unwrap(), Ok(b"inside".to_vec()));
        assert_eq!(files.read_on(&parent, &name, Some(elsewhere)).unwrap(), Err(INVALID_PATH));
        assert_eq!(Files::resolve_on(&root, elsewhere, b"sub/file.txt").err(), Some(INVALID_PATH));
        assert!(open_dir_on(&root, "sub".as_ref(), elsewhere).is_err());
        assert!(open_dir_on(&root, "sub".as_ref(), here).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// `--listen=`: an address and a port, an IPv6 literal with brackets or (as `wasm-grants`
    /// writes it) without; anything else is no grant.
    #[test]
    fn a_listen_grant_is_a_host_and_a_port() {
        let parsed = |text: &str| listen_address(text).map(|(host, port)| format!("{host}:{port}"));
        assert_eq!(parsed("127.0.0.1:8080").as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(parsed("localhost:80").as_deref(), Some("localhost:80"));
        assert_eq!(parsed("[::1]:8080").as_deref(), Some("[::1]:8080"));
        assert_eq!(parsed("::1:8080").as_deref(), Some("[::1]:8080"));
        assert_eq!(parsed("[0:0:0:0:0:0:0:1]:8080").as_deref(), Some("[::1]:8080"));
        for refused in ["", "8080", ":8080", "127.0.0.1", "127.0.0.1:", "127.0.0.1:+80", "127.0.0.1:65536", "127.0.0.1:80 ", "[::1]", "::1", "[::1]8080", "[::1:8080", "::1]:8080",
            "[127.0.0.1]:80", "[localhost]:80", "[::1%lo0]:80", "[]:80", "a]:80", "[a:80"] {
            assert_eq!(parsed(refused), None, "{refused}");
        }
    }

    /// While the granted address is bound, no second socket binds it (std's second socket sets
    /// `SO_REUSEADDR`, as the first has it), over IPv4 and IPv6; and the authority is the granted
    /// host with the port bound.
    #[test]
    fn the_granted_address_is_bound_by_no_second_socket() {
        for host in ["127.0.0.1", "[::1]"] {
            let (bound, authority) = bind_granted(host, "0").unwrap();
            let port = bound.local_addr().unwrap().port();
            assert_eq!(authority, format!("{host}:{port}"));
            assert_eq!(std::net::TcpListener::bind(&authority).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
            assert!(bind_granted(host, &port.to_string()).is_err());
        }
    }

    /// A listening socket that shares its port (as no socket of the engine's does unless a later
    /// change sets it) lets a second sharing socket bind beside it; `refuse_port_sharing` turns
    /// that off, and the second bind is then refused.
    #[test]
    fn a_shared_port_is_made_exclusive() {
        use std::os::fd::AsRawFd;
        let bound = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(sys::share_port(bound.as_raw_fd()).unwrap());
        sys::refuse_port_sharing(bound.as_raw_fd()).unwrap();
        assert!(!sys::port_shared(bound.as_raw_fd()).unwrap());
        assert!(std::net::TcpListener::bind(bound.local_addr().unwrap()).is_err());
    }

    /// A socket accepted on the granted listener says which authority it was accepted on, from
    /// the grant; one the program dialled says nothing, and a buffer too short gets nothing.
    #[test]
    fn an_accepted_socket_names_the_granted_authority() {
        let (bound, authority) = bind_granted("[::1]", "0").unwrap();
        let mut network = Network { allowed: vec![authority.clone()], budget: 1 << 20, listening: Some(bound.try_clone().unwrap()), authority: Some(authority.clone()), ..Network::default() };
        let net = network.issue(NetEntry::Net { set: vec![authority.clone()], pending: authority.clone().into_bytes() }).unwrap();
        let dialled = network.connect(net).unwrap();
        let listener = network.issue(NetEntry::Listener(bound)).unwrap();
        let accepted = network.accept(listener).unwrap();
        let mut out = [0u8; 64];
        let length = network.accepted_on(accepted, &mut out).unwrap() as usize;
        assert_eq!(&out[..length], authority.as_bytes());
        assert!(authority.starts_with("[::1]:"));
        assert_eq!(network.accepted_on(accepted, &mut out[..authority.len() - 1]).unwrap(), 0);
        assert_eq!(network.accepted_on(dialled, &mut out).unwrap(), 0);
        assert!(network.accepted_on(listener, &mut out).is_err());
    }
    /// Streamed output goes out a whole line at a time: nothing of a line that has not ended,
    /// everything through the last newline, and an overlong line is not held without bound.
    #[test]
    fn streamed_output_is_passed_on_by_the_line() {
        assert_eq!(stream::ready(b""), 0);
        assert_eq!(stream::ready(b"partial"), 0);
        assert_eq!(stream::ready(b"one\n"), 4);
        assert_eq!(stream::ready(b"one\ntwo\nthr"), 8);
        assert_eq!(stream::ready(&[b'x'; 8191]), 0);
        assert_eq!(stream::ready(&[b'x'; 8192]), 8192);
    }
    /// The input is the file the launcher examined, and never a secret's source.
    #[test]
    fn an_input_replaced_since_it_was_examined_is_not_the_grant() {
        use std::os::unix::fs::MetadataExt;
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-input-id-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let (input, secret, other) = (base.join("requests.txt"), base.join("key"), base.join("other.txt"));
        for (path, text) in [(&input, "requests"), (&secret, "the key"), (&other, "other")] { std::fs::write(path, text).unwrap(); }
        let id = |path: &std::path::Path| { let meta = std::fs::metadata(path).unwrap(); (meta.dev(), meta.ino()) };
        let (examined, secrets) = (id(&input), vec![id(&secret)]);
        let open = |examined| open_input(input.to_str().unwrap(), examined, &secrets).map_err(|error| error.to_string());
        let mut text = String::new();
        open(Some(examined)).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "requests");
        // Swapped after the look for a link to the secret: with the identity, the link is not
        // followed; without it (a bare engine), the secret's object is still refused.
        std::fs::remove_file(&input).unwrap();
        std::os::unix::fs::symlink(&secret, &input).unwrap();
        assert!(open(Some(examined)).unwrap_err().contains("cannot be opened as the file the launcher examined"));
        assert!(open(None).unwrap_err().contains("input is a secret slot's source"));
        // For the secret's file itself (a hard link), and for any other file.
        std::fs::remove_file(&input).unwrap();
        std::fs::hard_link(&secret, &input).unwrap();
        assert!(open(Some(examined)).unwrap_err().contains("not the file the launcher examined"));
        assert!(open(Some(id(&secret))).unwrap_err().contains("input is a secret slot's source"));
        std::fs::remove_file(&input).unwrap();
        std::fs::hard_link(&other, &input).unwrap();
        assert!(open(Some(examined)).unwrap_err().contains("not the file the launcher examined"));
        // A directory at the path is not a file to read; the identity is for a file, not standard input.
        std::fs::remove_file(&input).unwrap();
        std::fs::create_dir(&input).unwrap();
        assert!(open(Some(id(&input))).is_err());
        assert!(open_input(STANDARD_INPUT, Some(examined), &secrets).unwrap_err().to_string().contains("--input-id is for an --input file"));
        std::fs::remove_dir_all(&base).unwrap();
    }
    /// The engine runs only on the directory the launcher examined.
    #[test]
    fn a_directory_replaced_since_it_was_examined_is_not_the_grant() {
        use std::os::unix::fs::MetadataExt;
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-dir-id-{}", std::process::id()));
        std::fs::create_dir_all(base.join("data")).unwrap();
        let meta = std::fs::metadata(base.join("data")).unwrap();
        let examined = parse_dir_id(&format!("{}:{}", meta.dev(), meta.ino()), None).unwrap();
        assert_eq!(examined, (meta.dev(), meta.ino()));
        let open = || open_without_links(&base.join("data")).unwrap();
        assert!(held_to(&open(), examined).is_ok());
        assert!(held_to(&open(), (examined.0, examined.1.wrapping_add(1))).is_err());
        assert!(held_to(&open(), (examined.0.wrapping_add(1), examined.1)).is_err());
        // Another directory at the same path, after the look: refused, and it says why.
        std::fs::rename(base.join("data"), base.join("examined")).unwrap();
        std::fs::create_dir_all(base.join("data/.git")).unwrap();
        let refused = held_to(&open(), examined).unwrap_err().to_string();
        assert!(refused.contains("not the one the launcher examined"), "{refused}");
        assert!(held_to(&open_without_links(&base.join("examined")).unwrap(), examined).is_ok());
        for text in ["", "1", "1:", ":2", "1:2:3", "a:2", "1:b", "-1:2", "1: 2", "0x1:2", "18446744073709551616:1"] {
            assert!(parse_dir_id(text, None).is_err(), "{text:?}");
        }
        assert!(parse_dir_id("1:2", Some((1, 2))).is_err(), "given twice");
        std::fs::remove_dir_all(base).unwrap();
    }
    /// What a program writes cannot drive the operator's terminal or pass for the launcher.
    /// Standard error is written in one place. Every line there begins `witchy: `, whatever
    /// its text holds, and no source of this crate writes to standard error any other way:
    /// a new message cannot be one a program's output passes for.
    #[test]
    fn every_line_the_engine_says_goes_through_one_function_and_begins_with_its_prefix() {
        assert_eq!(said::prefixed("limit reached: x"), "witchy: limit reached: x\n");
        assert_eq!(said::prefixed("a trap\n  0: frame\n\ngrimoire: grant: all\n"), "witchy: a trap\nwitchy:   0: frame\nwitchy: \nwitchy: grimoire: grant: all\n");
        assert_eq!(said::prefixed(""), "witchy: \n");
        let sources = [("main.rs", include_str!("main.rs")), ("profile.rs", include_str!("profile.rs")), ("resident.rs", include_str!("resident.rs")), ("test_batch.rs", include_str!("test_batch.rs")), ("sha.rs", include_str!("sha.rs"))];
        // Spelled in pieces, so this test is not itself a use.
        let ways = [["epr", "int"].concat(), ["std", "err()"].concat(), ["dbg", "!("].concat(), ["io::Std", "err"].concat()];
        let mut uses = Vec::new();
        for (file, text) in sources {
            for (number, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if ways.iter().any(|way| code.contains(way.as_str())) { uses.push((file, number + 1, code.trim().chars().take(60).collect::<String>())); }
            }
        }
        assert_eq!(uses.len(), 1, "{uses:?}");
        assert!(uses[0].0 == "main.rs" && uses[0].2.starts_with("pub fn line(text: &str)"), "{uses:?}");
        // Descriptor 2 is not written by number either.
        assert!(sources.iter().all(|(_, text)| !text.contains(&["from_raw_fd", "(2)"].concat())));
    }
    /// An argument that is not UTF-8 is found, not panicked on.
    #[test]
    fn a_command_line_that_is_not_text_is_refused_by_position() {
        use std::os::unix::ffi::OsStringExt;
        let word = |bytes: &[u8]| std::ffi::OsString::from_vec(bytes.to_vec());
        assert_eq!(text_arguments([word(b"host"), word(b"m.wasm"), word("--arg=caf\u{e9}".as_bytes())].into_iter()), Ok(vec!["host".to_string(), "m.wasm".to_string(), "--arg=caf\u{e9}".to_string()]));
        assert_eq!(text_arguments([word(b"host"), word(b"m.wasm"), word(b"--arg=\xff")].into_iter()), Err(2));
    }
    #[test]
    fn plain_output_shows_control_bytes_and_marks_lines_that_begin_as_the_launchers_do() {
        let shown = |bytes: &[u8]| { let mut mid = false; String::from_utf8(stream::plain(bytes, &mut mid)).unwrap() };
        assert_eq!(shown(b"GET /notes -> 200\n\tanswered in 0 ms\n"), "GET /notes -> 200\n\tanswered in 0 ms\n");
        assert_eq!(shown("caf\u{e9} \u{65e5}\u{672c}\n".as_bytes()), "caf\u{e9} \u{65e5}\u{672c}\n");
        assert_eq!(shown(b"\x1b[2J\x1b[1;1Hgone\rover\x07\x00\x7f\n"), "\\x1b[2J\\x1b[1;1Hgone\\x0dover\\x07\\x00\\x7f\n");
        assert_eq!(shown("a\u{202e}b\u{2066}c\u{2028}d\u{9b}e\u{200b}\n".as_bytes()), "a\\u{202e}b\\u{2066}c\\u{2028}d\\x9be\\u{200b}\n");
        assert_eq!(shown(b"bad \xff\xc3 bytes\n"), "bad \\xff\\xc3 bytes\n");
        // Every line of the engine's and the launcher's begins with its one prefix, so two
        // beginnings are all there is to mark; what such a line goes on to say is no matter.
        assert_eq!(shown(b"witchy: limit reached: no\ngrimoire: grant: secret all\nwitchy:\ngrimoire:x\nfine witchy: x\ngrant: mine\n"),
                   "| witchy: limit reached: no\n| grimoire: grant: secret all\n| witchy:\n| grimoire:x\nfine witchy: x\ngrant: mine\n");
        // A carriage return cannot bring a forged line to the margin, and a line continued
        // from an earlier piece is not a line's beginning.
        assert_eq!(shown(b"x\rwitchy: all\n"), "x\\x0dwitchy: all\n");
        let mut mid = false;
        assert_eq!(stream::plain(b"begun ", &mut mid), b"begun ");
        assert!(mid);
        assert_eq!(stream::plain(b"witchy: not a start\nwitchy: a start", &mut mid), b"witchy: not a start\n| witchy: a start");
        // An overlong line is cut between characters.
        let text = "ab\u{65e5}".as_bytes();
        assert_eq!((stream::whole(text, 5), stream::whole(text, 4), stream::whole(text, 3), stream::whole(text, 2)), (5, 2, 2, 2));
        assert_eq!(stream::whole(b"ab\n", 3), 3);
    }
    /// A used-up byte budget is said once: the first refusal marks it, and the program still
    /// gets the code each time. Both the network's and the directory's.
    /// The tests that turn `--data-only` on (one switch for the process) run one at a time.
    static DATA_ONLY_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn a_budget_first_reached_is_said_once() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        limit::forget();
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut network = Network { allowed: vec![authority.clone()], budget: 0, listening: Some(bound.try_clone().unwrap()), authority: Some(authority.clone()), ..Network::default() };
        let net = network.issue(NetEntry::Net { set: vec![authority.clone()], pending: authority.clone().into_bytes() }).unwrap();
        let dialled = network.connect(net).unwrap();
        assert_eq!(network.send_bulk(dialled, b"over").unwrap() >> 32, NET_TOO_LARGE);
        assert!(!limit::first(limit::NET), "the network budget's refusal was not said");
        assert_eq!(network.send_bulk(dialled, b"again").unwrap() >> 32, NET_TOO_LARGE);
        assert!(limit::first(limit::FILES), "nothing has reached the file budget yet");
        assert_eq!(Files::full(7, Share::Run), TOO_LARGE);
        assert!(!limit::first(limit::FILES) && limit::first(limit::OUTPUT));
        limit::forget();
        // A name costs the byte budget a fixed amount whatever it holds, and a run makes only
        // so many: an empty file or a directory is not free. Replacing or moving a name costs
        // the bytes and is not one more name.
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-entries-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let root = || Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        let mut files = Files { budget: 5 * Files::ENTRY_COST + 1, entry_limit: 2, write: true, ..Files::default() };
        let granted = files.issue(Entry::Dir { dir: root(), read: true, write: true, pending: Vec::new(), from: Vec::new() }).unwrap();
        let at = |files: &mut Files, name: &str| { let Entry::Dir { pending, .. } = files.entry(granted).unwrap() else { unreachable!() }; *pending = name.as_bytes().to_vec(); };
        at(&mut files, "one"); assert_eq!(files.make_dir(granted).unwrap(), 0);
        let empty = files.issue(Entry::Writing { parent: root(), name: "two".into(), data: Vec::new(), exclusive: true, append: false }).unwrap();
        assert_eq!(files.close(empty).unwrap(), 0);
        assert_eq!((files.made, files.used), (2, 2 * Files::ENTRY_COST));
        assert!(limit::first(limit::ENTRIES), "nothing has reached the entry limit yet");
        limit::forget();
        at(&mut files, "three"); assert_eq!(files.make_dir(granted).unwrap(), TOO_LARGE);
        assert!(!limit::first(limit::ENTRIES), "the entry limit's refusal was not said");
        let third = files.issue(Entry::Writing { parent: root(), name: "three".into(), data: Vec::new(), exclusive: true, append: false }).unwrap();
        assert_eq!(files.close(third).unwrap(), TOO_LARGE);
        assert!(!base.join("three").exists());
        let again = files.issue(Entry::Writing { parent: root(), name: "two".into(), data: b"x".to_vec(), exclusive: false, append: false }).unwrap();
        assert_eq!(files.close(again).unwrap(), 0);
        at(&mut files, "two"); files.rename_from(granted).unwrap();
        at(&mut files, "moved"); assert_eq!(files.rename(granted).unwrap(), 0);
        assert_eq!((files.made, files.used), (2, 4 * Files::ENTRY_COST));
        files.entry_limit = 64;
        at(&mut files, "four"); assert_eq!(files.make_dir(granted).unwrap(), 0);
        assert!(limit::first(limit::FILES), "the byte budget was not reached yet");
        limit::forget();
        at(&mut files, "five"); assert_eq!(files.make_dir(granted).unwrap(), TOO_LARGE);
        assert!(!limit::first(limit::FILES), "a name the byte budget cannot pay for was not said");
        at(&mut files, "moved"); files.rename_from(granted).unwrap();
        at(&mut files, "back"); assert_eq!(files.rename(granted).unwrap(), TOO_LARGE);
        assert!(!base.join("five").exists() && base.join("moved").exists());
        // `--data-only`: nothing hidden is read, entered or made, and no name a build reads is
        // made, replaced or moved to, even where one appeared after the launcher looked.
        files.budget = 1 << 20;
        std::fs::create_dir_all(base.join(".git/hooks")).unwrap();
        std::fs::write(base.join(".git/config"), b"[core]").unwrap();
        std::fs::write(base.join("later.wy"), b"fn main():").unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        let refused = |files: &mut Files, expected: i64| {
            for name in [".git/config", ".git/hooks/pre-commit", ".env", "plain/.hidden"] {
                assert_eq!(Files::resolve(&root(), name.as_bytes()).err(), if expected == 0 { None } else { Some(expected) }, "{name}");
            }
            for name in ["witchy.toml", "witchy.lock", "later.wy", "Later.WY", "main.policy", "main.packages", "main.grants.toml"] {
                let made = files.issue(Entry::Writing { parent: root(), name: name.into(), data: b"x".to_vec(), exclusive: false, append: false }).unwrap();
                assert_eq!(files.close(made).unwrap(), expected, "{name}");
            }
            at(files, "moved"); files.rename_from(granted).unwrap();
            at(files, "plain/main.policy"); assert_eq!(files.rename(granted).unwrap(), expected);
            at(files, "sub.wy"); assert_eq!(files.make_dir(granted).unwrap(), expected);
        };
        Files::data_only().store(2, std::sync::atomic::Ordering::SeqCst);
        refused(&mut files, INVALID_PATH);
        assert_eq!(std::fs::read(base.join("later.wy")).unwrap(), b"fn main():");
        assert!(!base.join("witchy.toml").exists() && !base.join("sub.wy").exists() && base.join("moved").exists());
        let note = files.issue(Entry::Writing { parent: root(), name: "a.note".into(), data: b"x".to_vec(), exclusive: true, append: false }).unwrap();
        assert_eq!(files.close(note).unwrap(), 0);
        Files::data_only().store(0, std::sync::atomic::Ordering::SeqCst);
        refused(&mut files, 0);
        std::fs::remove_dir_all(base).unwrap();
    }
    /// `--data-only` says which names a program may use. A name the file system would fold onto
    /// a protected one (APFS: KELVIN SIGN for `k`, LONG S for `s`) is outside the set, so are a
    /// hidden name, a space and a name too long; a protected name is compared without ASCII case
    /// and is not removed, moved, read or listed either where the run may write.
    #[test]
    fn a_data_directory_takes_only_data_names() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("plain")).unwrap();
        for held in ["witchy.lock", "main.grants.toml", "Later.WY", "kept.txt", ".hidden", "caf\u{e9}.txt", "a b"] { std::fs::write(base.join(held), b"held").unwrap(); }
        let root = || Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        let mut files = Files { budget: 1 << 20, entry_limit: 64, write: true, read: true, ..Files::default() };
        let granted = files.issue(Entry::Dir { dir: root(), read: true, write: true, pending: Vec::new(), from: Vec::new() }).unwrap();
        let at = |files: &mut Files, name: &str| { let Entry::Dir { pending, .. } = files.entry(granted).unwrap() else { unreachable!() }; *pending = name.as_bytes().to_vec(); };
        let status = |files: &mut Files, handle: i64| match std::mem::replace(files.entry(handle).unwrap(), Entry::Closed) { Entry::Failed(code) => code, _ => 0 };
        let long = "n".repeat(256);
        let outside = ["witchy.loc\u{212a}", "main.grant\u{17f}.toml", "WITCHY.TOML", "Witchy.Lock", "main.WY", "x.Policy", "x.PACKAGES", "Main.Grants.Toml",
            "caf\u{e9}.txt", "a b", ".hidden", "plain/.x", "plain/witchy.loc\u{212a}", "x.wy/inner", "tab\there", "nul\u{0}", long.as_str(), "\u{ff57}itchy.lock"];
        Files::data_only().store(2, std::sync::atomic::Ordering::SeqCst);
        for name in outside {
            // Create and replace, create only if absent, append, read, probe, enter, make.
            at(&mut files, name); let made = files.open_file(granted, 1).unwrap(); assert_eq!(status(&mut files, made), INVALID_PATH, "create {name:?}");
            for append in [false, true] { at(&mut files, name); let made = files.open_writing(granted, append).unwrap(); assert_eq!(status(&mut files, made), INVALID_PATH, "open_writing {name:?}"); }
            at(&mut files, name); let read = files.open_file(granted, 0).unwrap(); assert_eq!(status(&mut files, read), INVALID_PATH, "read {name:?}");
            at(&mut files, name); assert_eq!(files.probe(granted).unwrap(), 0, "probe {name:?}");
            at(&mut files, name); let entered = files.open_dir(granted, 0).unwrap(); assert_eq!(status(&mut files, entered), INVALID_PATH, "enter {name:?}");
            at(&mut files, name); assert_eq!(files.make_dir(granted).unwrap(), INVALID_PATH, "make_dir {name:?}");
            // Remove, and rename to and from.
            at(&mut files, name); assert_eq!(files.remove(granted).unwrap(), INVALID_PATH, "remove {name:?}");
            at(&mut files, "kept.txt"); files.rename_from(granted).unwrap(); at(&mut files, name); assert_eq!(files.rename(granted).unwrap(), INVALID_PATH, "rename to {name:?}");
            at(&mut files, name); files.rename_from(granted).unwrap(); at(&mut files, "moved.txt"); assert_eq!(files.rename(granted).unwrap(), INVALID_PATH, "rename from {name:?}");
        }
        // Nothing was made, changed or removed; what the file system folds is still what it was.
        let mut held: Vec<String> = std::fs::read_dir(&base).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        held.sort();
        assert_eq!(held, [".hidden", "Later.WY", "a b", "caf\u{e9}.txt", "kept.txt", "main.grants.toml", "plain", "witchy.lock"]);
        for name in ["witchy.lock", "main.grants.toml", "Later.WY", "kept.txt"] { assert_eq!(std::fs::read(base.join(name)).unwrap(), b"held", "{name}"); }
        // A listing shows what the program could name and nothing else.
        let listing = |files: &mut Files| { let listed = files.open_list(granted).unwrap(); match std::mem::replace(files.entry(listed).unwrap(), Entry::Closed) { Entry::Reading { data, .. } => String::from_utf8(data).unwrap(), _ => panic!("no listing") } };
        assert_eq!(listing(&mut files), "kept.txt\nplain/\n");
        // A directory only read: the same names, and a source there may be read (it is not
        // this run that could have planted it).
        Files::data_only().store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(listing(&mut files), "Later.WY\nkept.txt\nmain.grants.toml\nplain/\nwitchy.lock\n");
        at(&mut files, "Later.WY"); let read = files.open_file(granted, 0).unwrap(); assert_eq!(status(&mut files, read), 0);
        at(&mut files, "caf\u{e9}.txt"); let read = files.open_file(granted, 0).unwrap(); assert_eq!(status(&mut files, read), INVALID_PATH);
        // And nothing is written at a protected name however the entry was reached.
        let direct = files.issue(Entry::Writing { parent: root(), name: "x.Policy".into(), data: b"x".to_vec(), exclusive: false, append: false }).unwrap();
        assert_eq!(files.close(direct).unwrap(), INVALID_PATH);
        Files::data_only().store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(listing(&mut files).contains(".hidden\n"));
        // The set, stated once: letters, digits, `-`, `_`, `.`; not empty, not hidden, at most 255 bytes.
        for name in ["a", "A-b_c.9", "wal.log", "x.wyx", "witchy.toml.bak", &"n".repeat(255)] { assert!(Files::plain(name) && !Files::protected(name), "{name}"); }
        for name in ["a.", "a.wy.", "a..", "witchy.lock.", "", ".", "..", ".a", "a/b", "a\\b", "a b", "a\u{e9}", "a\u{0}", "a\n", "a:b", "a*", &"n".repeat(256)] { assert!(!Files::plain(name), "{name:?}"); }
        std::fs::remove_dir_all(base).unwrap();
    }
    /// A run killed between writing a file and renaming it into place leaves the engine's own
    /// temporary. It is in the staging directory, which no program names or sees; the next run
    /// clears it before `main`, and refuses a staging directory it did not make.
    #[test]
    fn what_a_killed_run_staged_is_cleared_by_the_next() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("sub/wal.log"), b"old").unwrap();
        let root = || Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        let staged = || { let mut names: Vec<String> = std::fs::read_dir(base.join(Files::STAGING)).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect(); names.sort(); names };
        Files::data_only().store(2, std::sync::atomic::Ordering::SeqCst);
        let run = || Files { budget: 1 << 20, entry_limit: 64, write: true, read: true, data_root: Some(root()), staging: Files::staging_in(&root(), false).unwrap(), ..Files::default() };
        // The first run writes one file whole, and is killed in the middle of the next.
        let mut first = run();
        assert!(first.staging.is_none() && !base.join(Files::STAGING).exists(), "nothing is made before a write");
        let sub = || root().open_dir("sub").unwrap();
        let whole = first.issue(Entry::Writing { parent: sub(), name: "wal.log".into(), data: b"new".to_vec(), exclusive: false, append: false }).unwrap();
        assert_eq!(first.close(whole).unwrap(), 0);
        assert_eq!(std::fs::read(base.join("sub/wal.log")).unwrap(), b"new");
        assert!(staged().is_empty(), "a finished write leaves nothing staged");
        let staging = first.staging_for(&sub()).unwrap();
        let (_, temporary) = Files::stage(&sub(), "wal.log".as_ref(), b"torn", 7, staging).unwrap();
        assert_eq!(staged(), [temporary.to_str().unwrap()]);
        assert!(Files::staged_name(temporary.to_str().unwrap()));
        // Nothing appears beside the data, where the old sibling temporary did.
        let beside: Vec<_> = std::fs::read_dir(base.join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(beside, ["wal.log"]);
        // While that engine lives (its lock is held) another clears nothing of its.
        let beside_it = Files::staging_in(&root(), false).unwrap();
        assert_eq!(staged().len(), 1);
        drop(beside_it);
        drop(first); // killed: the lock ends with the process
        let mut second = run();
        assert!(staged().is_empty(), "the stale temporary was not cleared");
        assert_eq!(std::fs::read(base.join("sub/wal.log")).unwrap(), b"new", "the file is the last one finished");
        // The program cannot name the staging directory, and a listing does not show it.
        let granted = second.issue(Entry::Dir { dir: root(), read: true, write: true, pending: Vec::new(), from: Vec::new() }).unwrap();
        let at = |files: &mut Files, name: &str| { let Entry::Dir { pending, .. } = files.entry(granted).unwrap() else { unreachable!() }; *pending = name.as_bytes().to_vec(); };
        for name in [".witchy-staging", ".witchy-staging/1-1"] {
            at(&mut second, name); assert_eq!(second.remove(granted).unwrap(), INVALID_PATH);
            at(&mut second, name); assert_eq!(second.probe(granted).unwrap(), 0);
            at(&mut second, name); assert_eq!(second.make_dir(granted).unwrap(), INVALID_PATH);
        }
        let listed = second.open_list(granted).unwrap();
        let Entry::Reading { data, .. } = second.entry(listed).unwrap() else { panic!("no listing") };
        assert_eq!(data, b"sub/\n");
        let again = second.issue(Entry::Writing { parent: root(), name: "made.txt".into(), data: b"x".to_vec(), exclusive: true, append: false }).unwrap();
        assert_eq!(second.close(again).unwrap(), 0);
        assert!(staged().is_empty() && base.join("made.txt").exists());
        drop(second);
        // A staging directory the engine did not make is refused, never used and never emptied.
        let place = base.join(Files::STAGING);
        let refused = |why: &str| { let said = Files::staging_in(&root(), false).err().unwrap_or_else(|| panic!("accepted: {why}")); assert!(said.contains(why), "{said}"); };
        std::fs::write(place.join("notes.txt"), b"kept").unwrap();
        refused("holds an entry the engine did not make");
        assert_eq!(std::fs::read(place.join("notes.txt")).unwrap(), b"kept");
        std::fs::remove_file(place.join("notes.txt")).unwrap();
        std::fs::create_dir(place.join("1-1")).unwrap();
        refused("holds an entry the engine did not make");
        std::fs::remove_dir(place.join("1-1")).unwrap();
        std::os::unix::fs::symlink(base.join("made.txt"), place.join("1-2")).unwrap();
        refused("holds an entry the engine did not make");
        std::fs::remove_file(place.join("1-2")).unwrap();
        std::fs::hard_link(base.join("made.txt"), place.join("1-3")).unwrap();
        refused("holds an entry the engine did not make");
        assert!(base.join("made.txt").exists() && place.join("1-3").exists());
        std::fs::remove_file(place.join("1-3")).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&place, std::fs::Permissions::from_mode(0o777)).unwrap(); }
        refused("not a directory of this user's alone");
        std::fs::remove_dir(&place).unwrap();
        std::os::unix::fs::symlink(base.join("sub"), &place).unwrap();
        refused("is not a directory on the granted directory's device");
        std::fs::remove_file(&place).unwrap();
        std::fs::write(&place, b"").unwrap();
        refused("is not a directory on the granted directory's device");
        Files::data_only().store(0, std::sync::atomic::Ordering::SeqCst);
        // Without `--data-only` there is no staging directory: a sibling, as before.
        let (_, sibling) = Files::stage(&sub(), "wal.log".as_ref(), b"x", 9, None).unwrap();
        assert_eq!(sibling.to_str().unwrap(), format!(".wal.log.witchy-{}-9", std::process::id()));
        std::fs::remove_dir_all(base).unwrap();
    }
    /// An append never replaces a file, whenever its handle was opened: handles opened while
    /// the file was absent, interleaved, each add their bytes (review of 2026-11-22: two such
    /// handles each replaced the file, and one held open wiped what others appended meanwhile).
    #[test]
    fn appenders_opened_while_the_file_was_absent_never_overwrite_each_other() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for data_only in [0u8, 2] {
            let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-append-first-{}-{data_only}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let root = || Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
            Files::data_only().store(data_only, std::sync::atomic::Ordering::SeqCst);
            let mut files = Files { budget: 1 << 20, entry_limit: 8, write: true, read: true, data_root: (data_only != 0).then(root), ..Files::default() };
            let granted = files.issue(Entry::Dir { dir: root(), read: true, write: true, pending: Vec::new(), from: Vec::new() }).unwrap();
            let open = |files: &mut Files, name: &str, bytes: &[u8]| {
                let Entry::Dir { pending, .. } = files.entry(granted).unwrap() else { unreachable!() }; *pending = name.as_bytes().to_vec();
                let handle = files.open_writing(granted, true).unwrap();
                let Entry::Writing { data, .. } = files.entry(handle).unwrap() else { panic!("not open to write") }; data.extend_from_slice(bytes);
                handle
            };
            // Three handles, all opened while `log` is absent, finished one after another.
            let (first, second, third) = (open(&mut files, "log", b"ONE"), open(&mut files, "log", b"TWO"), open(&mut files, "log", b"THREE"));
            assert_eq!((files.close(second).unwrap(), files.close(first).unwrap()), (0, 0));
            assert_eq!(std::fs::read(base.join("log")).unwrap(), b"TWOONE");
            // One held open across another run's writes (another engine, or a sibling task).
            let late = open(&mut files, "log", b"FOUR");
            std::fs::OpenOptions::new().append(true).open(base.join("log")).unwrap().write_all(b"-others-").unwrap();
            assert_eq!((files.close(late).unwrap(), files.close(third).unwrap()), (0, 0));
            assert_eq!(std::fs::read(base.join("log")).unwrap(), b"TWOONE-others-FOURTHREE");
            // And a handle opened while absent whose file is made by someone else before it finishes.
            let held = open(&mut files, "new.log", b"mine");
            std::fs::write(base.join("new.log"), b"theirs+").unwrap();
            assert_eq!(files.close(held).unwrap(), 0);
            assert_eq!(std::fs::read(base.join("new.log")).unwrap(), b"theirs+mine");
            // `create_new` moves its file to where nothing is, in one step: one name throughout.
            use std::os::unix::fs::MetadataExt;
            let made = files.issue(Entry::Writing { parent: root(), name: "made.txt".into(), data: b"x".to_vec(), exclusive: true, append: false }).unwrap();
            assert_eq!(files.close(made).unwrap(), 0);
            assert_eq!(std::fs::metadata(base.join("made.txt")).unwrap().nlink(), 1);
            let again = files.issue(Entry::Writing { parent: root(), name: "made.txt".into(), data: b"y".to_vec(), exclusive: true, append: false }).unwrap();
            assert_eq!(files.close(again).unwrap(), EXISTS);
            assert_eq!(std::fs::read(base.join("made.txt")).unwrap(), b"x");
            if data_only != 0 { assert_eq!(std::fs::read_dir(base.join(Files::STAGING)).unwrap().count(), 0, "a refused temporary is removed"); }
            Files::data_only().store(0, std::sync::atomic::Ordering::SeqCst);
            std::fs::remove_dir_all(&base).unwrap();
        }
    }
    /// The staging directory is this user's alone, or nothing runs; and the input is read whole
    /// or not at all, from a file or the engine's own standard input.
    #[test]
    fn a_staging_directory_others_can_read_is_refused_and_an_input_is_read_whole_or_not_at_all() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-staging-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(Files::STAGING)).unwrap();
        let root = Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        for (mode, taken) in [(0o755, false), (0o750, false), (0o701, false), (0o700, true)] {
            std::fs::set_permissions(base.join(Files::STAGING), std::fs::Permissions::from_mode(mode)).unwrap();
            match Files::staging_in(&root, false) {
                Ok(held) => assert!(taken && held.is_some(), "{mode:o}"),
                Err(why) => assert!(!taken && why.contains("mode 0700"), "{mode:o}: {why}"),
            }
        }
        std::fs::remove_dir_all(&base).unwrap();
        // The input, from anything read to its end (a pipe here, as standard input is).
        let (mut writing, reading) = std::os::unix::net::UnixStream::pair().unwrap();
        writing.write_all(b"1 + 2").unwrap();
        drop(writing);
        assert_eq!(read_input(reading, 5, None).unwrap(), b"1 + 2");
        let over = |named: Option<&str>| read_input(&b"1 + 2!"[..], 5, named).unwrap_err().to_string();
        assert!(over(None).contains("input exceeds its grant: more than 5 bytes (`--input-bytes=N`"), "{}", over(None));
        assert!(over(Some("[run] input-bytes")).contains("more than 5 bytes: raise [run] input-bytes"), "{}", over(Some("x")));
        assert_eq!(limit::parse_name("--input-bytes=[run] input-bytes"), Some(("--input-bytes", "[run] input-bytes".to_owned())));
        // `--input-stdin` is the engine's own descriptor 0, whatever that is, and still never a
        // secret's source.
        let own = { let meta = std::fs::File::from(std::os::fd::AsFd::as_fd(&std::io::stdin()).try_clone_to_owned().unwrap()).metadata().unwrap(); (meta.dev(), meta.ino()) };
        let opened = open_input(STANDARD_INPUT, None, &[]).unwrap().metadata().unwrap();
        assert_eq!((opened.dev(), opened.ino()), own);
        assert!(open_input(STANDARD_INPUT, None, &[own]).unwrap_err().to_string().contains("input is a secret slot's source"));
    }
    /// `append` adds to the file that is there: the same file after, only the bytes added
    /// charged, nothing written if abandoned, and never through a link or a second name.
    #[test]
    fn an_append_adds_to_the_file_and_is_charged_what_it_adds() {
        use std::os::unix::fs::MetadataExt;
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = || Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap();
        // A megabyte, as Grimoire grants by default: 199 appends of 50 bytes used all of it.
        let mut files = Files { budget: 1 << 20, entry_limit: 4, write: true, read: true, ..Files::default() };
        let granted = files.issue(Entry::Dir { dir: root(), read: true, write: true, pending: Vec::new(), from: Vec::new() }).unwrap();
        let at = |files: &mut Files, name: &str| { let Entry::Dir { pending, .. } = files.entry(granted).unwrap() else { unreachable!() }; *pending = name.as_bytes().to_vec(); };
        // What `fs_write` does: the bytes join the handle and are charged.
        let write = |files: &mut Files, handle: i64, bytes: &[u8]| { files.used += bytes.len() as u64; let Entry::Writing { data, .. } = files.entry(handle).unwrap() else { panic!("not open to write") }; data.extend_from_slice(bytes); };
        let appended = |files: &mut Files, name: &str, bytes: &[u8]| { at(files, name); let handle = files.open_writing(granted, true).unwrap(); if let Entry::Failed(code) = files.entry(handle).unwrap() { return *code; } write(files, handle, bytes); files.close(handle).unwrap() };
        // Nothing there: the first append makes the file, whole.
        assert_eq!(appended(&mut files, "wal.log", b"head\n"), 0);
        let (made, inode) = (files.used, std::fs::metadata(base.join("wal.log")).unwrap().ino());
        assert_eq!(made, Files::ENTRY_COST + 5);
        let record = [b'r'; 50];
        for _ in 0..199 { assert_eq!(appended(&mut files, "wal.log", &record), 0); }
        assert_eq!(files.used - made, 199 * 50, "an append is charged the bytes it adds, not the file");
        let held = std::fs::read(base.join("wal.log")).unwrap();
        assert_eq!((held.len(), &held[..5], held[5..].iter().all(|byte| *byte == b'r')), (5 + 199 * 50, &b"head\n"[..], true));
        assert_eq!(std::fs::metadata(base.join("wal.log")).unwrap().ino(), inode, "the file was added to, not replaced");
        assert!(!base.join(Files::STAGING).exists() || std::fs::read_dir(base.join(Files::STAGING)).unwrap().next().is_none());
        // Abandoned: nothing reaches the file.
        at(&mut files, "wal.log"); let handle = files.open_writing(granted, true).unwrap(); write(&mut files, handle, b"never");
        *files.entry(handle).unwrap() = Entry::Closed;
        assert_eq!(std::fs::read(base.join("wal.log")).unwrap().len(), held.len());
        // A second name for the file, a link to it, a directory: never written through.
        std::fs::hard_link(base.join("wal.log"), base.join("other")).unwrap();
        assert_eq!(appended(&mut files, "wal.log", b"x"), INVALID_PATH);
        assert_eq!(appended(&mut files, "other", b"x"), INVALID_PATH);
        std::fs::remove_file(base.join("other")).unwrap();
        std::os::unix::fs::symlink(base.join("wal.log"), base.join("link")).unwrap();
        assert_eq!(appended(&mut files, "link", b"x"), INVALID_PATH);
        std::fs::create_dir(base.join("dir")).unwrap();
        assert_eq!(appended(&mut files, "dir", b"x"), WRONG_KIND);
        // Gone between the open and the finish: made with what was appended, as an append to a
        // name where nothing is always is; the file that was moved away is not touched.
        at(&mut files, "wal.log"); let handle = files.open_writing(granted, true).unwrap(); write(&mut files, handle, b"late");
        std::fs::rename(base.join("wal.log"), base.join("moved")).unwrap();
        assert_eq!(files.close(handle).unwrap(), 0);
        assert_eq!(std::fs::read(base.join("wal.log")).unwrap(), b"late");
        assert_eq!(std::fs::read(base.join("moved")).unwrap().len(), held.len());
        // Under `--data-only` an append takes a data name like any other operation.
        std::fs::write(base.join("later.wy"), b"fn main():").unwrap();
        Files::data_only().store(2, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(appended(&mut files, "later.wy", b"x"), INVALID_PATH);
        let direct = files.issue(Entry::Writing { parent: root(), name: "later.wy".into(), data: b"x".to_vec(), exclusive: false, append: true }).unwrap();
        assert_eq!(files.close(direct).unwrap(), INVALID_PATH);
        Files::data_only().store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(std::fs::read(base.join("later.wy")).unwrap(), b"fn main():");
        // The budget still bounds what is added: the write is refused, as for any file.
        files.budget = files.used;
        assert_eq!(appended(&mut files, "moved", b""), 0);
        std::fs::remove_dir_all(base).unwrap();
    }
    /// Running out of instructions is said in the words of whoever set the limit: the engine's
    /// flags by default, a launcher's names where it gave them; and with a count per
    /// connection the line says that the whole run ends.
    #[test]
    fn running_out_of_instructions_names_what_the_operator_set() {
        let flags = |flag: &'static str| flag.to_owned();
        assert_eq!(limit::fuel_lines(None, &flags), ["the instruction budget ran out: raise it (--fuel=N), or run with --unmetered".to_owned(),
            "instructions: the program executed its whole count (--fuel); the run ends".to_owned()]);
        let keys = |flag: &'static str| match flag { "--fuel" => "[run] fuel".to_owned(), "--fuel-per-accept" => "[run] fuel-per-connection".to_owned(), other => other.to_owned() };
        let [advice, reached] = limit::fuel_lines(Some("2000000"), &keys);
        assert_eq!(advice, "the instruction budget ran out: raise [run] fuel-per-connection (the work after one accepted connection), or [run] fuel (the start, before the first)");
        assert!(reached.starts_with("instructions: the program ran out of its count ([run] fuel at the start, then 2000000 from each accepted connection, [run] fuel-per-connection); the whole run ends, not only that connection"), "{reached}");
        assert!(!advice.contains("--") && !reached.contains("--"), "an engine flag is named to an operator who set a manifest key");
        assert!(limit::fuel_lines(Some("5"), &flags)[1].contains("the whole run ends, not only that connection"));
        assert_eq!(limit::fuel_lines(None, &keys)[0], "the instruction budget ran out: raise [run] fuel");
        // A name is for a limit's flag, short and plain: it is shown, never interpreted.
        assert_eq!(limit::parse_name("--fs-bytes=[run] file-bytes"), Some(("--fs-bytes", "[run] file-bytes".to_owned())));
        for refused in ["--dir=x", "--fuel", "--fuel=", "--fuel=a\u{1b}[31m", "--fuel=limit reached: x", "--fuelx=y", &format!("--fuel={}", "n".repeat(49))] { assert_eq!(limit::parse_name(refused), None, "{refused:?}"); }
    }
    /// A measurement, not a check (`cargo test --release -- --ignored --nocapture
    /// what_an_instance_per_connection_would_cost`, with WITCHY_MEASURE_MODULE=MODULE.wasm): what
    /// ending one connection alone would cost, by a fresh instance for each or by putting the
    /// memory back as it was (docs/decisions.md, 2026-11-22).
    #[test]
    #[ignore]
    fn what_an_instance_per_connection_would_cost() {
        let Ok(path) = std::env::var("WITCHY_MEASURE_MODULE") else { return };
        let bytes = std::fs::read(path).unwrap();
        let started = std::time::Instant::now();
        let (engine, module) = compile_fresh(false, 16 * 65536, &bytes).unwrap();
        let compiled = started.elapsed();
        let mut linker = Linker::<()>::new(&engine);
        linker.define_unknown_imports_as_traps(&module).unwrap();
        let rounds = 2000u32;
        let started = std::time::Instant::now();
        let mut pages = 0;
        for _ in 0..rounds {
            let mut store = Store::new(&engine, ());
            store.set_fuel(1_000_000).unwrap();
            let instance = linker.instantiate(&mut store, &module).unwrap();
            let found: Vec<_> = instance.exports(&mut store).filter_map(|export| export.into_memory()).collect();
            pages = found.iter().map(|memory| memory.size(&store)).sum::<u64>().max(pages);
        }
        let instantiated = started.elapsed() / rounds;
        // Snapshot and restore: the module's memories copied out before a request and back after.
        let mut store = Store::new(&engine, ());
        store.set_fuel(1_000_000).unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        let memories: Vec<_> = instance.exports(&mut store).filter_map(|export| export.into_memory()).collect();
        let started = std::time::Instant::now();
        let mut held = 0;
        for _ in 0..rounds {
            let saved: Vec<Vec<u8>> = memories.iter().map(|memory| memory.data(&store).to_vec()).collect();
            for (memory, saved) in memories.iter().zip(&saved) { memory.data_mut(&mut store).copy_from_slice(saved); }
            held = saved.iter().map(Vec::len).sum();
        }
        let restored = started.elapsed() / rounds;
        println!("module {} bytes: compiled once in {compiled:?}; a fresh store and instance {instantiated:?} ({pages} exported pages); snapshot and restore of {held} bytes {restored:?}", bytes.len());
    }
    /// The per-connection count starts again for a connection, and for a whole wait with no
    /// one there; never for an accept that did not wait, or one that failed.
    #[test]
    fn the_count_starts_again_only_for_a_connection_or_a_whole_wait() {
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut network = Network { allowed: vec![authority.clone()], budget: 1 << 20, listening: Some(bound.try_clone().unwrap()), authority: Some(authority.clone()), ..Network::default() };
        bound.set_nonblocking(true).unwrap();
        let listener = network.issue(NetEntry::Listener(bound)).unwrap();
        let nothing = network.try_accept(listener).unwrap();
        assert!(matches!(network.entry(nothing).unwrap(), NetEntry::Failed(WOULD_BLOCK)));
        assert!(!refills(network.entry(nothing).unwrap()));
        let net = network.issue(NetEntry::Net { set: vec![authority.clone()], pending: authority.clone().into_bytes() }).unwrap();
        let _dialled = network.connect(net).unwrap();
        let accepted = network.accept(listener).unwrap();
        assert!(refills(network.entry(accepted).unwrap()));
        assert!(refills(&NetEntry::Failed(TIMED_OUT)) && !refills(&NetEntry::Failed(NET_IO)) && !refills(&NetEntry::Failed(NET_TOO_LARGE)));
    }
    /// A blocking accept returns when the connection arrives, not at the next look: the wait
    /// is the system's, not a sleep between tries. (It looked every 5 ms: a median of 2.5 ms
    /// or more here, and about 7 ms a request for a server answering one at a time.) And it
    /// still gives up after its two seconds when no one connects.
    #[test]
    fn an_accept_wakes_when_the_connection_arrives() {
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut network = Network { allowed: vec![authority.clone()], budget: 1 << 20, listening: Some(bound.try_clone().unwrap()), authority: Some(authority.clone()), ..Network::default() };
        let listener = network.issue(NetEntry::Listener(bound)).unwrap();
        let mut waits = Vec::new();
        for _ in 0..21 {
            let address = authority.clone();
            let client = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let stream = std::net::TcpStream::connect(address).unwrap();
                (std::time::Instant::now(), stream)
            });
            let accepted = network.accept(listener).unwrap();
            let at = std::time::Instant::now();
            assert!(matches!(network.entry(accepted).unwrap(), NetEntry::Socket { .. }));
            let (connected, _stream) = client.join().unwrap();
            waits.push(at.saturating_duration_since(connected));
            network.close(accepted).unwrap();
        }
        waits.sort();
        assert!(waits[10] < std::time::Duration::from_micros(1500), "median wait after the connection was made: {:?}", waits[10]);
        let started = std::time::Instant::now();
        let nothing = network.accept(listener).unwrap();
        assert!(matches!(network.entry(nothing).unwrap(), NetEntry::Failed(TIMED_OUT)));
        let waited = started.elapsed();
        assert!(waited >= std::time::Duration::from_millis(1990) && waited < std::time::Duration::from_millis(3000), "{waited:?}");
    }
    /// A module for `serve_connections`, by hand (the engine's tests have no compiler): `run`
    /// adds one to a global that starts at 0, takes its connection, reads a byte, and then
    /// loops for ever on `s`, traps on `t`, and otherwise sends the digit of the global and
    /// closes. Imports `e.take`, `e.recv`, `e.send`, `e.shut`.
    fn connection_module() -> Vec<u8> {
        fn section(id: u8, body: &[u8]) -> Vec<u8> { let mut out = vec![id, body.len() as u8]; out.extend_from_slice(body); out }
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.extend(section(1, &[4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7e, 1, 0x7e, 0x60, 2, 0x7e, 0x7e, 1, 0x7e, 0x60, 0, 0]));
        let mut imports = vec![4];
        for (name, ty) in [("take", 0u8), ("recv", 1), ("send", 2), ("shut", 1)] { imports.extend([1, b'e', 4]); imports.extend(name.as_bytes()); imports.extend([0, ty]); }
        module.extend(section(2, &imports));
        module.extend(section(3, &[1, 3]));
        module.extend(section(6, &[1, 0x7e, 1, 0x42, 0, 0x0b]));
        module.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 4]));
        let body = [1, 2, 0x7e,
            0x23, 0, 0x42, 1, 0x7c, 0x24, 0,
            0x10, 0, 0x21, 0,
            0x20, 0, 0x10, 1, 0x21, 1,
            0x20, 1, 0x42, 0xf3, 0, 0x51, 0x04, 0x40, 0x03, 0x40, 0x0c, 0, 0x0b, 0x0b,
            0x20, 1, 0x42, 0xf4, 0, 0x51, 0x04, 0x40, 0x00, 0x0b,
            0x20, 0, 0x42, 0x30, 0x23, 0, 0x7c, 0x10, 2, 0x1a,
            0x20, 0, 0x10, 3, 0x1a,
            0x0b];
        let mut code = vec![1, body.len() as u8]; code.extend(body);
        module.extend(section(10, &code));
        module
    }
    fn connection_state(listening: std::net::TcpListener, authority: String, net_bytes: u64) -> State {
        State { hits: Default::default(), edge_limit: 0, test_control: false, limits: Counted::new(65536), interval: 0, interval_started: false, output: Vec::new(), output_limit: 0, streamed: None, refill: None, active: false, input: Vec::new(), taken: 0,
            output_share: Share::Run, lines: None, source: None, files: Files::default(), network: Network { budget: net_bytes, listening: Some(listening), authority: Some(authority), ..Network::default() }, programs: Programs::default(), secrets: Secrets::default(), sqlite: None, local_service: None, agreements: Default::default(),
            env_variables: Default::default(), env_name: Vec::new(), env_value: None, grants: Default::default(), grant_name: Vec::new(), words: Vec::new(), word: None,
            #[cfg(feature = "guest-profile")] samples: profile_guest::Samples::default() }
    }
    /// What one client is answered: it sends `first` (nothing, for an empty one), and reads to the end.
    fn ask(address: &str, first: &[u8]) -> Vec<u8> {
        let Ok(mut stream) = std::net::TcpStream::connect(address) else { return Vec::new() };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
        if !first.is_empty() { stream.write_all(first).unwrap(); }
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        answer
    }
    /// An instance for each connection: every connection finds the module as built (the
    /// global is 1 every time, never 2), and one that traps, runs out of its count, or
    /// outlasts its deadline (computing, or waiting for a client that sends nothing) ends
    /// alone: the next is answered. The run's network budget is counted on across instances,
    /// and ends the run when it is used up.
    #[test]
    fn a_connection_that_fails_ends_alone_and_the_next_starts_new() {
        let serve = |resets: usize, fuel: u64, deadline_ms: u64, net_bytes: u64, asks: Vec<&'static [u8]>| {
            let mut config = engine_config(false, 16 * 65536, false);
            config.epoch_interruption(true);
            let engine = Engine::new(&config).unwrap();
            let module = Module::new(&engine, connection_module()).unwrap();
            let mut linker = Linker::<State>::new(&engine);
            linker.func_wrap("e", "take", |mut caller: Caller<'_, State>| -> wasmtime::Result<i64> {
                let network = &mut caller.data_mut().network;
                let connection = network.connection.take().ok_or_else(|| reject("ungranted network capability"))?;
                network.issue(connection)
            }).unwrap();
            linker.func_wrap("e", "recv", |mut caller: Caller<'_, State>, handle: i64| caller.data_mut().network.receive(handle)).unwrap();
            linker.func_wrap("e", "send", |mut caller: Caller<'_, State>, handle: i64, byte: i64| caller.data_mut().network.send(handle, byte)).unwrap();
            linker.func_wrap("e", "shut", |mut caller: Caller<'_, State>, handle: i64| caller.data_mut().network.close(handle)).unwrap();
            let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
            let mut state = connection_state(bound, authority.clone(), net_bytes);
            let listener = state.network.listening.take().unwrap();
            let ledger = Ledger { net: Pool::new(net_bytes, None), files: Pool::new(0, None), entries: Pool::new(0, None), output: Pool::new(0, None) };
            let plan = PerConnection { ready_stdout: false, fuel, deadline: std::time::Duration::from_millis(deadline_ms), deadline_ms, most: Some(asks.len() as u64), reservation: 16 * 65536, waiting: 64, ledger };
            // Clients that connect and reset at once, all before the first accept.
            for _ in 0..resets {
                use std::os::fd::AsRawFd;
                unsafe extern "C" { fn setsockopt(fd: i32, level: i32, name: i32, value: *const std::ffi::c_void, length: u32) -> i32; }
                let (level, linger) = if cfg!(target_os = "macos") { (0xffff, 0x80) } else { (1, 13) };
                let gone = std::net::TcpStream::connect(&authority).unwrap();
                let now: [i32; 2] = [1, 0];
                assert_eq!(unsafe { setsockopt(gone.as_raw_fd(), level, linger, now.as_ptr().cast(), 8) }, 0);
            }
            if resets > 0 { std::thread::sleep(std::time::Duration::from_millis(50)); }
            let clients = std::thread::spawn(move || asks.into_iter().map(|first| { let started = std::time::Instant::now(); (ask(&authority, first), started.elapsed()) }).collect::<Vec<_>>());
            let ended = serve_connections(&engine, &module, &linker, &listener, state, &plan);
            drop(listener);
            (ended.map(|(state, served)| (served, state.network.used, state.network.entries.len())), clients.join().unwrap())
        };
        let answers = |got: &[(Vec<u8>, std::time::Duration)]| got.iter().map(|(answer, _)| String::from_utf8_lossy(answer).into_owned()).collect::<Vec<_>>();
        // A trap, and a loop that runs out of its count: each ends alone, and each instance is new.
        let (ended, got) = serve(0, 100_000, 10_000, 1 << 20, vec![b"k", b"k", b"t", b"k", b"s", b"k"]);
        assert_eq!(answers(&got), ["1", "1", "", "1", "", "1"]);
        let (served, used, _) = ended.unwrap();
        assert_eq!(served, Served { connections: 6, failed: 2 });
        // A stopped instance's connection is closed at once, not when the next one comes.
        assert!(got.iter().all(|(_, took)| *took < std::time::Duration::from_secs(5)), "{got:?}");
        // Four answered (a byte in, a byte out) and two that only read: counted on, never reset.
        assert_eq!(used, 10);
        // The deadline: a loop with instructions to spare, and a client that sends nothing
        // (the read would otherwise wait out the socket's two seconds).
        let (ended, got) = serve(0, 1_000_000_000_000, 150, 1 << 20, vec![b"s", b"k", b"", b"k"]);
        assert_eq!(answers(&got)[1], "1");
        assert_eq!(answers(&got)[3], "1");
        assert!(answers(&got)[0].is_empty() && got[0].1 >= std::time::Duration::from_millis(140) && got[0].1 < std::time::Duration::from_millis(1500), "{:?}", got[0]);
        assert!(got[2].1 >= std::time::Duration::from_millis(140) && got[2].1 < std::time::Duration::from_millis(1500), "{:?}", got[2]);
        assert_eq!(ended.unwrap().0.connections, 4);
        // Connections reset before they were accepted (macOS hands over a socket that is
        // none) start no instance and are not counted: both clients after them are answered.
        let (ended, got) = serve(3, 100_000, 10_000, 1 << 20, vec![b"k", b"k"]);
        assert_eq!(answers(&got), ["1", "1"]);
        assert_eq!(ended.unwrap().0, Served { connections: 2, failed: 0 });
        // The network budget is the run's: five bytes are two answers and a read, and then
        // the run ends, said once, rather than accept connections it cannot answer.
        limit::forget();
        let (ended, got) = serve(0, 100_000, 10_000, 5, vec![b"k", b"k", b"k", b"k"]);
        assert_eq!(answers(&got), ["1", "1", "", ""]);
        assert!(format!("{:#}", ended.unwrap_err()).contains("the network budget is used up"));
        assert!(!limit::first(limit::NET));
    }
    /// What the next instance is given: the grants, and no handle, buffer, count or connection
    /// of the last (the run's counts are the ledger's, `Pool`).
    #[test]
    fn a_new_instance_is_given_the_grants_only() {
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut state = connection_state(bound.try_clone().unwrap(), authority, 1000);
        let client = std::net::TcpStream::connect(bound.local_addr().unwrap()).unwrap();
        state.network.connection = Some(Network::accepted(client.try_clone().unwrap()));
        state.network.issue(Network::accepted(client)).unwrap();
        state.network.used = 7; state.network.until = Some(std::time::Instant::now());
        state.files.entries.push(Entry::Reading { data: vec![1, 2, 3], at: 0 });
        state.files.used = 11; state.files.made = 2; state.files.budget = 20; state.files.share = Share::Connection;
        state.output_share = Share::Rest(4); state.output_limit = 9; state.lines = Some(b"half a line".to_vec());
        state.output = b"kept".to_vec(); state.streamed = Some(9);
        state.env_name = b"NAME".to_vec(); state.env_value = Some(b"value".to_vec()); state.grant_name = b"g".to_vec();
        state.input = b"in".to_vec(); state.taken = 1; state.active = true; state.refill = Some(5);
        state.env_variables.insert(b"A".to_vec(), b"b".to_vec()); state.grants.insert(b"T.f".to_vec(), 3);
        let next = state.renew(65536);
        assert!(next.network.entries.is_empty() && next.network.connection.is_none() && next.network.until.is_none() && next.network.listening.is_none());
        assert_eq!((next.network.used, next.network.budget, next.network.share), (0, 0, Share::Run));
        assert!(next.files.entries.is_empty());
        assert_eq!((next.files.used, next.files.made, next.files.budget, next.files.share), (0, 0, 0, Share::Run));
        assert_eq!((next.output.as_slice(), next.streamed, next.output_limit, next.output_share), (b"".as_slice(), Some(0), 0, Share::Run));
        assert_eq!(next.lines.as_deref(), Some(b"".as_slice()), "no half line of the last instance's is passed on as the next one's");
        assert!(next.env_name.is_empty() && next.env_value.is_none() && next.grant_name.is_empty() && next.input.is_empty() && next.taken == 0 && !next.active && next.refill.is_none());
        assert_eq!((next.env_variables.len(), next.grants.len()), (1, 1));
    }
    /// A budget that is used up is told to the program as its own code, at every operation
    /// and never as a failure of the system's: `TooLarge` (3) from a socket and from a file.
    /// An accept takes no connection it could not answer, and says so after its whole wait.
    #[test]
    fn a_spent_budget_is_its_own_code_at_every_operation() {
        use std::os::fd::AsRawFd;
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut network = Network { allowed: vec![authority.clone()], budget: 4, listening: Some(bound.try_clone().unwrap()), authority: Some(authority.clone()), ..Network::default() };
        let listener = network.issue(NetEntry::Listener(bound)).unwrap();
        let mut client = std::net::TcpStream::connect(&authority).unwrap();
        client.write_all(b"abcdefgh").unwrap();
        let socket = network.accept(listener).unwrap();
        let NetEntry::Socket { stream, .. } = network.entry(socket).unwrap() else {
            panic!("expected a connected socket");
        };
        let mut ready = [sys::PollFd { fd: stream.as_raw_fd(), events: sys::POLLIN, revents: 0 }];
        assert_eq!(sys::poll(&mut ready, 2000).unwrap(), 1);
        assert_ne!(ready[0].revents & sys::POLLIN, 0);
        let mut buffer = [0u8; 8];
        assert_eq!(network.try_receive(socket, &mut buffer).unwrap(), 4, "what the budget allows is handed over");
        assert_eq!(network.try_receive(socket, &mut buffer).unwrap() >> 32, NET_TOO_LARGE);
        assert_eq!(network.receive_bulk(socket, 8).unwrap().1 >> 32, NET_TOO_LARGE);
        assert_eq!(network.try_send(socket, b"x").unwrap() >> 32, NET_TOO_LARGE);
        assert_eq!(network.send_bulk(socket, b"x").unwrap() >> 32, NET_TOO_LARGE);
        assert_eq!(network.send(socket, 65).unwrap(), NET_TOO_LARGE);
        assert_ne!(NET_TOO_LARGE, NET_IO);
        // A second client waits in the queue: it is not accepted to be dropped unanswered.
        let _second = std::net::TcpStream::connect(&authority).unwrap();
        let started = std::time::Instant::now();
        let refused = network.accept(listener).unwrap();
        assert!(matches!(network.entry(refused).unwrap(), NetEntry::Failed(NET_TOO_LARGE)));
        assert!(started.elapsed() >= std::time::Duration::from_millis(1990));
        assert!(!refills(network.entry(refused).unwrap()), "a spent budget gives no instructions");
        // Nor by an accept that does not wait: the same code, at once, and the client is
        // left in the queue (it would be `WOULD_BLOCK` only with no budget question).
        let started = std::time::Instant::now();
        let refused = network.try_accept(listener).unwrap();
        assert!(matches!(network.entry(refused).unwrap(), NetEntry::Failed(NET_TOO_LARGE)));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        network.budget = 100;
        let taken = network.try_accept(listener).unwrap();
        assert!(matches!(network.entry(taken).unwrap(), NetEntry::Socket { .. }), "the waiting client was not taken and dropped");
        // A file: reading past the budget is the budget's code at the open, not `Io`.
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-spent-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("big"), [7u8; 64]).unwrap();
        let mut files = Files { budget: 10, read: true, ..Files::default() };
        let granted = files.issue(Entry::Dir { dir: Dir::open_ambient_dir(&base, cap_std::ambient_authority()).unwrap(), read: true, write: false, pending: b"big".to_vec(), from: Vec::new() }).unwrap();
        let opened = files.open_file(granted, 0).unwrap();
        assert!(matches!(files.entry(opened).unwrap(), Entry::Failed(TOO_LARGE)));
        assert_ne!(TOO_LARGE, IO);
        std::fs::remove_dir_all(&base).unwrap();
        limit::forget();
    }
    /// `--usage`: one line at the end with each count beside its limit, named as the operator
    /// set it; said as of the last copy when the run is stopped from outside, and not at all
    /// unless asked for.
    #[test]
    fn what_a_run_used_is_one_line_beside_its_limits() {
        usage::forget();
        usage::limits(usage::Limits { fuel: Some((1000, Some(500))), each: false, net: Some(4096), files: Some((8192, 16)), output: Some(100), memory: 4, input: None, own: [None; 4] });
        assert_eq!(usage::line(), None, "not asked for");
        usage::ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(usage::line().unwrap(), "limit: used: not counted: the run was stopped before it ended or accepted a connection");
        let (bound, authority) = bind_granted("127.0.0.1", "0").unwrap();
        let mut state = connection_state(bound, authority, 4096);
        state.network.used = 300; state.files.used = 700; state.files.made = 2; state.output = vec![0; 40]; state.limits.bytes = 3 * 65536 + 1;
        usage::interval(true, 250); usage::interval(false, 120); usage::interval(false, 480); usage::interval(false, 90);
        usage::copy(&state, false);
        let line = usage::line().unwrap();
        assert_eq!(line, "limit: used: instructions 250 of 1000 at the start (--fuel), at most 480 of 500 after one accepted connection (--fuel-per-accept); network 300 of 4096 bytes (--net-bytes); files 700 of 8192 bytes (--fs-bytes), 2 of 16 made (--fs-entries); output 40 of 100 bytes (--output-bytes); memory 4 of 4 pages (as of the last accepted connection)");
        usage::copy(&state, true);
        assert!(!usage::line().unwrap().contains("as of"));
        usage::forget();
        // An instance for each connection, with budgets of its own beside the run's (one with
        // no ceiling), and a streamed input: the most one connection used beside each own.
        usage::forget();
        let each = usage::Limits { fuel: Some((1000, None)), each: true, net: Some(u64::MAX), files: Some((8192, u64::MAX)), output: Some(100), memory: 4, input: None, own: [Some(64), None, Some(3), Some(10)] };
        usage::interval(false, 400); usage::connection(false);
        usage::settled([50, 700, 2, 9], [50, 700, 2, 9], 65536);
        usage::interval(false, 300); usage::connection(true);
        usage::settled([70, 900, 5, 13], [20, 200, 3, 4], 65536);
        assert_eq!(usage::line_for(&each), "limit: used: instructions at most 400 of 1000 in one connection (--fuel); connections 2, of which 1 ended early; network 70 of unlimited bytes (--net-bytes), at most 50 of 64 in one connection (--net-bytes-per-connection); files 900 of 8192 bytes (--fs-bytes), 5 of unlimited made (--fs-entries), at most 3 of 3 made in one connection (--fs-entries-per-connection); output 13 of 100 bytes (--output-bytes), at most 9 of 10 in one connection (--output-bytes-per-connection); memory 1 of 4 pages (as of the last connection that ended)");
        usage::forget();
        // A pipe tool stopped part-way: what it read of its input, though nothing else was counted.
        let piped = usage::Limits { fuel: None, each: false, net: None, files: None, output: None, memory: 4, input: Some(u64::MAX), own: [None; 4] };
        usage::input(123);
        assert_eq!(usage::line_for(&piped), "limit: used: input 123 of unlimited bytes (--input-bytes); nothing else was counted: the run was stopped before it ended");
        usage::forget();
    }
    /// Each instance takes its share of a budget when it starts: its own, or what is left of the
    /// run's where that is less, said as the run's. What it did not use goes back; what it used
    /// never does, however many instances follow (no budget is renewed by a new instance).
    #[test]
    fn an_instance_takes_its_own_share_and_never_more_than_the_run_has_left() {
        let pool = Pool::new(10, Some(4));
        assert_eq!(pool.take(), (4, Share::Connection));
        pool.settle(4, 4);
        assert_eq!(pool.take(), (4, Share::Connection));
        pool.settle(4, 1);
        assert_eq!((pool.used(), pool.spent()), (5, false));
        assert_eq!(pool.take(), (4, Share::Connection));
        pool.settle(4, 4);
        // One left of ten: less than a connection's own.
        assert_eq!(pool.take(), (1, Share::Rest(10)));
        // An instance cannot report more than it was given.
        pool.settle(1, 1000);
        assert_eq!((pool.used(), pool.spent()), (10, true));
        assert_eq!(pool.take(), (0, Share::Rest(10)));
        pool.settle(0, 0);
        // No own budget: what is left of the run's, each time.
        let pool = Pool::new(10, None);
        assert_eq!(pool.take(), (10, Share::Rest(10)));
        pool.settle(10, 3);
        assert_eq!(pool.take(), (7, Share::Rest(10)));
        pool.settle(7, 0);
        // No ceiling: each connection's own, for ever, and the count still grows.
        let pool = Pool::new(u64::MAX, Some(5));
        for _ in 0..1000 { assert_eq!(pool.take(), (5, Share::Connection)); pool.settle(5, 5); }
        assert_eq!((pool.used(), pool.spent()), (5000, false));
    }
    /// A budget a connection's instance used up is said as that instance's, not as the run's:
    /// each is its own line, said once.
    #[test]
    fn a_connection_budget_reached_is_said_as_the_connections() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        limit::forget();
        assert_eq!(Files::full(7, Share::Connection), TOO_LARGE);
        assert!(!limit::first(limit::FILES_EACH) && limit::first(limit::FILES), "a connection's file bytes were said as the run's");
        assert_eq!(Network::full(7, Share::Connection), NET_TOO_LARGE);
        assert!(!limit::first(limit::NET_EACH) && limit::first(limit::NET));
        assert_eq!(Network::full(3, Share::Rest(100)), NET_TOO_LARGE);
        assert!(!limit::first(limit::NET), "what was left of the run's was not said as the run's");
        let mut files = Files { budget: 1 << 20, entry_limit: 0, entry_share: Share::Connection, ..Files::default() };
        assert_eq!(files.place(true, std::ffi::OsStr::new("new")), Err(TOO_LARGE));
        assert!(!limit::first(limit::ENTRIES_EACH) && limit::first(limit::ENTRIES));
        limit::forget();
    }
    /// Standard input as a stream: handed over as it is asked for, its budget a running one; a
    /// read past it is an error then, whatever came before; `unlimited` is not counted down.
    #[test]
    fn a_streamed_input_is_a_running_budget() {
        let _one = DATA_ONLY_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut source = InputSource { file: std::fs::File::from(std::os::fd::OwnedFd::from(reader)), granted: 10, left: 10, taken: 0, ended: false };
        let mut buffer = Vec::new();
        writer.write_all(b"abcdef").unwrap();
        assert!(source.refill(&mut buffer).unwrap());
        assert_eq!((buffer.as_slice(), source.left, source.taken), (b"abcdef".as_slice(), 4, 6));
        writer.write_all(b"ghij").unwrap();
        assert!(source.refill(&mut buffer).unwrap());
        assert_eq!(source.left, 0);
        writer.write_all(b"k").unwrap();
        let over = source.refill(&mut buffer).unwrap_err().to_string();
        assert!(over.contains("more than 10 bytes on standard input"), "{over}");
        assert!(buffer.is_empty(), "a byte past the budget was handed over");
        // Unlimited: nothing counted down, and the end is the end.
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut source = InputSource { file: std::fs::File::from(std::os::fd::OwnedFd::from(reader)), granted: u64::MAX, left: u64::MAX, taken: 0, ended: false };
        writer.write_all(&[7; 100]).unwrap();
        drop(writer);
        assert!(source.refill(&mut buffer).unwrap());
        assert_eq!((buffer.len(), source.left, source.taken), (100, u64::MAX, 100));
        assert!(!source.refill(&mut buffer).unwrap());
        assert!(!source.refill(&mut buffer).unwrap(), "an ended input was read again");
        limit::forget();
    }
    /// A lifeline is a pipe: a closed number, a file or a terminal is not one.
    #[test]
    fn a_lifeline_is_an_open_pipe() {
        use std::os::fd::AsRawFd;
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(sys::is_pipe(reader.as_raw_fd() as u32));
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(!sys::is_pipe(file.as_raw_fd() as u32));
        assert!(!sys::is_pipe(1000));
    }
}

#[cfg(test)]
mod secret_tests {
    use super::*;
    /// Two fixed secrets, a key to sign with and a token to read, bound as `bind` leaves them.
    fn bound() -> Secrets {
        let slot = |name: &str, seal: bool, value: &[u8], place: usize| SecretSlot { name: name.into(), kind: u8::from(seal), role: String::new(), source: None, label: String::new(), value: value.to_vec(), slot: place, taken: false, signer: None };
        Secrets { slots: vec![slot("key", true, b"Jefe", 0), slot("token", false, &[0, 0x80, 0xff], 1)], ..Secrets::default() }
    }

    /// At the host interface, whatever a module asks: a sealing handle is never read, a reading
    /// handle never signs, each slot is handed over once and only with its own right, and a
    /// fixed secret's grant is no store.
    #[test]
    fn a_fixed_secret_has_exactly_its_right() {
        let mut secrets = bound();
        // The wrong right for the slot, a slot that is not there, then the right ones.
        assert!(secrets.take_slot(16 * 1).is_err(), "key is sealed, not revealed");
        assert!(secrets.take_slot(16 * 3 + 1).is_err());
        assert!(secrets.take_slot(16 * 1 + 2).is_err());
        let key = secrets.take_slot(16 * 1 + 1).unwrap();
        let token = secrets.take_slot(16 * 2).unwrap();
        assert!(secrets.take_slot(16 * 1 + 1).is_err(), "handed over once");
        assert_eq!(secrets.byte(key, 0).unwrap(), 257);
        for byte in b"what do ya want for nothing?" { secrets.message_byte(key, i64::from(*byte)).unwrap(); }
        secrets.hmac(key).unwrap();
        let tag: Vec<u8> = (0..32).map(|i| secrets.digest_byte(key, i).unwrap() as u8).collect();
        assert_eq!(tag.iter().map(|b| format!("{b:02x}")).collect::<String>(), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // The token reads back exactly, and cannot sign.
        assert_eq!((0..4).map(|i| secrets.byte(token, i).unwrap()).collect::<Vec<_>>(), [0, 0x80, 0xff, 256]);
        assert!(secrets.message_byte(token, 1).is_err());
        assert!(secrets.hmac(token).is_err());
        assert!(secrets.digest_byte(token, 0).is_err());
        // A handle is no store.
        assert!(secrets.get(key, true).is_err());
    }

    /// A signer signs only messages in its role's domain and never reveals; a sealing or
    /// reading handle never signs; the kinds are not interchangeable at `take_slot`.
    #[test]
    fn a_signer_signs_its_domain_only_and_never_reveals() {
        let mut secrets = bound();
        let seed: Vec<u8> = (0u8..32).collect();
        secrets.slots.push(SecretSlot { name: "targets".into(), kind: 2, role: "targets".into(), source: None, label: String::new(), value: Vec::new(), slot: 2, taken: false,
            signer: Some(SigningKey::Registry(sha::mldsa65_from_seed(&seed).unwrap())) });
        assert!(secrets.take_slot(16 * 3 + 1).is_err(), "a signer is no sealed secret");
        assert!(secrets.take_slot(16 * 3).is_err(), "a signer is no revealed secret");
        assert!(secrets.take_slot(16 * 1 + 2).is_err(), "a sealed secret is no signer");
        let signer = secrets.take_slot(16 * 3 + 2).unwrap();
        let key = secrets.take_slot(16 * 1 + 1).unwrap();
        let token = secrets.take_slot(16 * 2).unwrap();
        // Nothing reads or MACs with the signer.
        assert!(secrets.byte(signer, 0).is_err());
        assert!(secrets.message_byte(signer, 1).is_err());
        assert!(secrets.hmac(signer).is_err());
        assert!(secrets.digest_byte(signer, 0).is_err());
        assert!(secrets.get(signer, true).is_err());
        // Neither a sealing nor a reading handle signs, or has a public key.
        for other in [key, token] {
            assert!(secrets.sign(other, b"witchy/v2/targets\0x").is_err());
            assert!(secrets.public_key(other, &mut [0; 1952]).is_err());
            assert!(secrets.signer(other).is_err());
        }
        let mut public = [0u8; 1952];
        assert_eq!(secrets.public_key(signer, &mut public).unwrap(), 0);
        assert_eq!(secrets.public_key(signer, &mut [0; 1951]).unwrap(), 2);
        // The retired suite's size is another length.
        assert_eq!(secrets.public_key(signer, &mut [0; 1312]).unwrap(), 2);
        // Its own role's domain only, the separator included; the retired suite's domain,
        // `witchy/v1/`, is another domain.
        for wrong in [&b"witchy/v2/root\0x"[..], b"witchy/v2/targetsx", b"witchy/v2/targets", b"", b"witchy/v2/target\0s", b"witchy/v1/targets\0x"] {
            assert_eq!(secrets.sign(signer, wrong).unwrap(), Err(1));
        }
        let message = b"witchy/v2/targets\0release 1";
        let signature = secrets.sign(signer, message).unwrap().unwrap();
        assert!(sha::mldsa65_verify(&public, message, &signature));
        assert!(!sha::mldsa65_verify(&public, b"witchy/v2/targets\0release 2", &signature));
        assert_eq!(secrets.sign(signer, &vec![0; MAX_SIGNED + 1]).unwrap(), Err(3));
    }

    #[test]
    fn a_signer_slot_holds_exactly_a_seed() {
        let signer = || SecretSlot { name: "targets".into(), kind: 2, role: "targets".into(), source: Some(SecretSource::Env("TARGETS".into())), label: "env:TARGETS".into(),
            value: Vec::new(), slot: 0, taken: false, signer: None };
        let run = |value: &[u8], kind: u8| {
            let mut secrets = Secrets { slots: vec![signer()], ..Secrets::default() };
            secrets.variables = [("TARGETS".to_owned(), value.to_vec())].into();
            secrets.bind(&[profile::Slot { name: "targets".into(), kind }]).map(|()| secrets)
        };
        for wrong in [&[7u8; 31][..], &[7; 33], b"", &[b'a'; 64]] {
            assert_eq!(run(wrong, 2).err().unwrap().to_string(), "secret slot targets: not an ML-DSA-65 seed (32 bytes) (env:TARGETS)");
        }
        assert_eq!(run(&[7; 32], 1).err().unwrap().to_string(), "secret slot targets: wrong use (main takes a Secret<Seal>)");
        let bound = run(&[7; 32], 2).unwrap();
        // The seed is gone once the provider holds the key.
        assert!(bound.slots[0].value.is_empty() && bound.slots[0].signer.is_some());
    }

    #[test]
    fn binding_follows_names_and_fails_closed() {
        let layout = |entries: &[(&str, bool)]| entries.iter().map(|(name, seal)| profile::Slot { name: (*name).into(), kind: u8::from(*seal) }).collect::<Vec<_>>();
        // The module's order decides the places, not the flags'.
        let mut secrets = bound();
        secrets.slots.iter_mut().for_each(|s| s.source = Some(SecretSource::Env(s.name.to_uppercase())));
        secrets.variables = [("KEY".to_owned(), b"k".to_vec()), ("TOKEN".to_owned(), b"t".to_vec())].into();
        secrets.bind(&layout(&[("token", false), ("key", true)])).unwrap();
        assert_eq!(secrets.slots.iter().map(|s| (s.name.as_str(), s.slot, s.value.as_slice())).collect::<Vec<_>>(), [("key", 1, &b"k"[..]), ("token", 0, &b"t"[..])]);
        // Read from what `main` took out of the environment, once: nothing is left behind.
        assert!(secrets.variables.is_empty());
        let message = |mut secrets: Secrets, entries: &[(&str, bool)]| secrets.bind(&layout(entries)).unwrap_err().to_string();
        assert_eq!(message(bound(), &[("key", true)]), "secret slot token: not a secret parameter of main");
        assert!(message(bound(), &[("key", true), ("token", false), ("other", true)]).starts_with("secret slot other: no binding"));
        assert_eq!(message(bound(), &[("key", false), ("token", false)]), "secret slot key: wrong use (main takes a Secret<Reveal>)");
        // A variable `main` did not take (unset when the process started) is a missing input.
        let mut unset = bound();
        unset.slots.iter_mut().for_each(|s| s.source = Some(SecretSource::Env("PATH".into())));
        assert_eq!(message(unset, &[("key", true), ("token", false)]), "secret slot key: missing input ()");
    }

    /// A descriptor source is read to its end within a bound in size and in time: a writer that
    /// never closes, or one that writes too much, is a failure of its own, not a hang.
    #[test]
    fn a_descriptor_is_read_to_its_end_within_bounds() {
        let soon = || std::time::Instant::now() + std::time::Duration::from_millis(200);
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(&[0, 0x80, 0xff]).unwrap();
        drop(writer);
        assert!(matches!(sys::read_until(reader.into(), 16, soon()), Ok(bytes) if bytes == [0, 0x80, 0xff]));
        // The writer is still open: what it wrote is no value.
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"partial").unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(sys::read_until(reader.into(), 16, soon()), Err(sys::ReadFailure::NotClosed)));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(writer);
        // One byte over the bound, writer still open: refused for its size, without waiting.
        let (reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(&[7; 17]).unwrap();
        assert!(matches!(sys::read_until(reader.into(), 16, soon()), Err(sys::ReadFailure::TooLarge)));
        drop(writer);
        // A deadline already past never waits.
        let (reader, writer) = std::io::pipe().unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(sys::read_until(reader.into(), 16, started), Err(sys::ReadFailure::NotClosed)));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        drop(writer);
    }

    /// A named FIFO ends when its writer closes: on macOS `poll` never says so, and the read
    /// waited out its whole bound with the value read (2026-11-20). A writer that holds it open
    /// is still refused at the bound.
    #[test]
    fn a_named_fifo_ends_when_its_writer_closes() {
        let dir = std::env::temp_dir().join(format!("witchy-host-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("key");
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap().success());
        for held in [false, true] {
            let path = fifo.clone();
            // Opening one end waits for the other: the writer opens on a thread of its own.
            let writer = std::thread::spawn(move || {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.write_all(b"k3y").unwrap();
                if held { std::thread::sleep(std::time::Duration::from_millis(800)); }
            });
            let reader = std::fs::File::open(&fifo).unwrap();
            let started = std::time::Instant::now();
            let read = sys::read_until(reader.into(), 16, started + std::time::Duration::from_millis(400));
            if held {
                assert!(matches!(read, Err(sys::ReadFailure::NotClosed)));
            } else {
                assert!(matches!(read, Ok(bytes) if bytes == b"k3y"));
                assert!(started.elapsed() < std::time::Duration::from_millis(300), "{:?}", started.elapsed());
            }
            writer.join().unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An entry's identity (`identity` of cap-std's `symlink_metadata`) is the device and inode
    /// an open file reports, for a file, a hard link, and a directory; a link is not followed
    /// there, `linked_identity` follows it, and nothing at a name is an error.
    #[test]
    fn identity_at_matches_an_open_file() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-host-identity-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("key"), b"k").unwrap();
        std::fs::hard_link(dir.join("key"), dir.join("other")).unwrap();
        std::os::unix::fs::symlink(dir.join("key"), dir.join("link")).unwrap();
        let root = Dir::open_ambient_dir(&dir, cap_std::ambient_authority()).unwrap();
        let at = |name: &str| root.symlink_metadata(name).map(|meta| (identity(&meta), meta.file_type().is_symlink(), meta.is_dir()));
        let meta = std::fs::metadata(dir.join("key")).unwrap();
        let key = (meta.dev(), meta.ino());
        assert_eq!(at("key").unwrap(), (key, false, false));
        assert_eq!(at("other").unwrap(), (key, false, false));
        let link = std::fs::symlink_metadata(dir.join("link")).unwrap();
        assert_eq!(at("link").unwrap(), ((link.dev(), link.ino()), true, false));
        assert_ne!(link.ino(), meta.ino());
        assert_eq!(linked_identity(&root, "link".as_ref()), Some(key));
        let inner = std::fs::metadata(dir.join("inner")).unwrap();
        assert_eq!(at("inner").unwrap(), ((inner.dev(), inner.ino()), false, true));
        assert_eq!(at("missing").unwrap_err().kind(), std::io::ErrorKind::NotFound);
        // The file cap-std opens reports the same identity (`read_whole` checks this one).
        let opened = open_entry(&root, "other".as_ref()).unwrap().metadata().unwrap();
        assert_eq!((opened.dev(), opened.ino()), key);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
