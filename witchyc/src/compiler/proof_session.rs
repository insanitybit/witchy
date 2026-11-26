//! Bounded native checker; production use requires an explicit trusted builder record.
use super::{proof_digest as digest, proof_measure::Measurements};
use crate::diagnostic::Diagnostic;
use std::{collections::BTreeMap, fs, io::{Read, Write}, os::unix::process::CommandExt,
    path::{Path, PathBuf}, process::{Child, ChildStdin, Command, Stdio}, sync::mpsc, thread::JoinHandle, time::{Duration, Instant}};
type Result<T> = std::result::Result<T, Diagnostic>;
fn error(text: &'static str) -> Diagnostic { Diagnostic::new("W0402", 0..0, text) }
thread_local! {
    // Current request's evidence and explanation probes stay with its worker.
    static LAST_FAILURE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // Verified idle production helpers, kept only while a `Spares` scope is open.
    static SPARES: std::cell::RefCell<Option<Pool>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn take_last_failure() -> Option<PathBuf> { LAST_FAILURE.with_borrow_mut(Option::take) }
pub(super) fn quiet(on: bool) { QUIET.set(on); }
/// Helpers started beside one, for a refused build's explanation probes. A refusal ends its
/// helper (the helper treats a failed check as terminal), and each probe that fails ends another.
const SPARE_HELPERS: usize = 2;
#[derive(Default)]
struct Pool { kept: Vec<Native>, refused: bool }
/// While one is open on this thread, production helpers are started `SPARE_HELPERS` at a time
/// once a request has been refused (the explanation's probes follow), and a probe session takes
/// a kept one instead of launching its own. Every launch keeps its bracket: a full SDK snapshot
/// matching the expected identity just before it and another after its LOAD, before anything
/// it replies is used; one snapshot serves every helper launched or loaded on its side of it.
/// Closing the scope stops every helper still kept.
pub(super) struct Spares { owner: bool }
impl Spares {
    pub(super) fn open() -> Self {
        Self { owner: SPARES.with_borrow_mut(|pool| pool.is_none().then(|| *pool = Some(Pool::default())).is_some()) }
    }
}
impl Drop for Spares {
    fn drop(&mut self) {
        if !self.owner { return; }
        let Some(Pool { kept, .. }) = SPARES.with_borrow_mut(Option::take) else { return };
        // Stopping a kept helper is not part of any request: its evidence is not the last failure.
        let (last, _quiet) = (LAST_FAILURE.with_borrow_mut(Option::take), Quiet::on());
        for native in kept { let _ = Session { native: Some(native), ..Session::default() }.finish(); }
        LAST_FAILURE.set(last);
    }
}
/// Quiet until dropped, also on unwind: a resident session reuses this thread.
struct Quiet(bool);
impl Quiet { fn on() -> Self { Self(QUIET.replace(true)) } }
impl Drop for Quiet { fn drop(&mut self) { QUIET.set(self.0); } }
fn spares_open() -> bool { SPARES.with_borrow(Option::is_some) }
fn spares_refused() -> bool { SPARES.with_borrow(|pool| pool.as_ref().is_some_and(|pool| pool.refused && pool.kept.is_empty())) }
fn keep_spares(natives: Vec<Native>) { SPARES.with_borrow_mut(|pool| if let Some(pool) = pool { pool.kept.extend(natives) }); }
fn io<T>(r: std::io::Result<T>) -> Result<T> { r.map_err(|_| error("native diagnostic session I/O refused")) }
// An uncertain writer remains owned here; it is never silently detached or reused.
static OUTSTANDING_WRITERS: std::sync::Mutex<Vec<(u32, PathBuf, JoinHandle<()>)>> = std::sync::Mutex::new(Vec::new());
unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; fn getpgid(pid: i32) -> i32; }
pub(super) struct Session {
    native: Option<Native>,
    ready: Option<super::ready::Binding>,
    poisoned: bool,
    request: String,
    expected_generation: Option<String>,
    body_mode: bool,
    source: String,
    installed_body: Option<String>,
    pending_body: Option<(String, String, String)>,
    independent: Vec<(String, String)>,
    body_install_ns: u128,
    selected_body: Option<String>,
    used_cached_proofs: bool,
}
struct Native {
    child: Child,
    input: Option<ChildStdin>,
    path: PathBuf,
    root: PathBuf,
    root_setting: PathBuf,
    helper: PathBuf,
    keep: bool,
    private_group: bool,
    group_id: i32,
    stopped: bool,
    cleanup_started: bool,
    leader_reaped: bool,
    writer: Option<JoinHandle<()>>,
    sdk: String,
    helper_id: String,
    binding: String,
    context: String,
    nonce: String,
    seq: usize,
    offset: usize,
    v2: bool,
    source: String,
    body: String,
    /// A full SDK snapshot taken after this helper's LOAD matched `sdk` (see `verify`).
    verified: bool,
}
impl Default for Session { fn default() -> Self { Self { native: None, ready: None, poisoned: false, request: String::new(), expected_generation: None, body_mode: false, source: String::new(), installed_body: None, pending_body: None, independent: Vec::new(), body_install_ns: 0, selected_body: None, used_cached_proofs: false } } }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
use super::proof_wire2::BODY_OPERATIONS;

fn known_reply(line: &str, binding: &str, nonce: &str, seq: usize, phase: &str, status: &str, claim: &str) -> bool {
    let Some(json) = line.strip_prefix("WITCHY_RESIDENT_V1:").and_then(|s| s.strip_prefix('{')).and_then(|s| s.strip_suffix('}')) else { return false; };
    let mut fields = BTreeMap::new();
    for field in json.split(',') {
        let Some((name, value)) = field.split_once(':') else { return false; };
        let Some(name) = name.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return false; };
        let value = value.trim();
        let value = if name == "seq" { value } else {
            let Some(v) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return false; };
            if v.bytes().any(|b| b == b'"' || b == b'\\' || b < 32) { return false; } v
        };
        if fields.insert(name, value.to_owned()).is_some() { return false; }
    }
    let expected = [("binding", binding.to_owned()), ("nonce", nonce.to_owned()), ("seq", seq.to_string()),
        ("phase", phase.to_owned()), ("status", status.to_owned()), ("mode", "UNSEALED_DIAGNOSTIC".into()), ("claim", claim.to_owned())];
    fields == expected.into_iter().collect()
}
fn bounded_output(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let file = io(fs::File::open(path))?;
    io(file.take((limit + 1) as u64).read_to_end(&mut bytes))?;
    if bytes.len() > limit { return Err(error("native diagnostic output bound/trailing data")); }
    Ok(bytes)
}
fn check_owner() -> Result<()> {
    if super::compiler_session::cancelled() {
        return Err(error("prepared Lean proof check cancelled by compiler session shutdown"));
    }
    Ok(())
}

impl Native {
    fn configured_helper() -> Result<PathBuf> {
        let helper = PathBuf::from(std::env::var_os("WITCHY_WASM_NATIVE_HELPER").ok_or_else(|| error("diagnostic session requires operator native helper"))?);
        if !helper.is_absolute() || io(fs::canonicalize(&helper))? != helper { return Err(error("native helper must be absolute without symlink")); }
        Ok(helper)
    }
    fn launch(sdk: String, root: PathBuf, root_setting: PathBuf, helper: PathBuf, context: &str, v2: bool, keep: bool) -> Result<Self> {
        check_owner()?;
        let helper_id = io(digest::file(&helper))?;
        let binding = io(digest::bytes(format!("{sdk}:{helper_id}:{context}").as_bytes()))?;
        let mut random = [0u8; 16];
        io(fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)))?;
        let nonce = hex(&random);
        let path = std::env::temp_dir().join(format!("witchy-native-diagnostic-{}-{nonce}", std::process::id()));
        use std::os::unix::fs::DirBuilderExt;
        let mut dir = fs::DirBuilder::new();
        dir.mode(0o700);
        io(dir.create(&path))?;
        let out = io(fs::File::create(path.join("stdout")))?;
        let err = io(fs::File::create(path.join("stderr")))?;
        // Production inherits the launcher's group so an artifact timeout kills both
        // compiler and helper. The fixed helper creates threads, never subprocesses.
        // Diagnostics retain their independently owned process group.
        let private_group = keep;
        let mut command = Command::new(&helper);
        command.arg(&root).arg(&binding).arg(&nonce)
            .env_clear().env("LEAN_PATH", root.join("lib/lean")).current_dir(&path)
            .stdin(Stdio::piped()).stdout(out).stderr(err);
        if private_group { command.process_group(0); }
        let mut child = match command.spawn() {
            Ok(child) => child, Err(_) => { let _ = fs::remove_dir_all(&path); return Err(error("native diagnostic launch refused")); }
        };
        let pid = child.id() as i32;
        let group_id = if private_group { pid } else { unsafe { getpgid(0) } };
        if pid <= 1 || group_id <= 0 || unsafe { getpgid(pid) } != group_id {
            let _ = child.kill(); let _ = child.wait(); let _ = fs::remove_dir_all(&path);
            return Err(error("native diagnostic child group ownership unknown"));
        }
        let input = child.stdin.take();
        let mut native = Self {
            child, input, path, root, root_setting, helper, sdk, helper_id, binding, nonce,
            keep, private_group, group_id, stopped: false, cleanup_started: false, leader_reaped: false, writer: None, context: context.into(), seq: 0, offset: 0, v2, source: super::proof_wire2::ZERO.into(), body: super::proof_wire2::ZERO.into(),
            verified: false,
        };
        // The snapshot after LOAD is `verify`, which runs before anything the helper
        // replies to a request is used: this helper's own first request, or a spare's pool entry.
        let load = native.exchange("LOAD", "", "", "loaded-untrusted", "").and_then(|_| native.idle(false));
        if let Err(mut e) = load {
            native.retain_failure();
            // Unverified: not a proof's deadline (`proof::DEADLINES`), which names a stalled request.
            if e.message == "native diagnostic deadline" { e.message = "native diagnostic deadline at helper startup".into(); }
            return Err(e);
        }
        if keep { super::compiler_diagnostic!("native diagnostic raw evidence: {}", native.path.display()); }
        Ok(native)
    }
    /// `SPARE_HELPERS` more helpers launched like `like`, as far as they start. Their own
    /// failures are not a request's: they neither refuse it nor replace its evidence.
    fn spares(like: &Native) -> Vec<Native> {
        let mut spares = Vec::new();
        if super::compiler_session::cancelled() { return spares; }
        let _quiet = Quiet::on();
        for _ in 0..SPARE_HELPERS {
            match Native::launch(like.sdk.clone(), like.root.clone(), like.root_setting.clone(), like.helper.clone(), &like.context, like.v2, like.keep) {
                Ok(spare) => spares.push(spare),
                Err(_) => break,
            }
        }
        spares
    }
    /// `verify` the spares (and `first`, launched just before them), then keep the spares.
    /// On a refusal every one's evidence is retained, unread (`retain_failure`).
    fn verify_kept(first: Option<&mut Native>, mut spares: Vec<Native>, m: &mut Measurements) -> Result<()> {
        let mut all: Vec<&mut Native> = first.into_iter().collect();
        all.extend(spares.iter_mut());
        if let Err(e) = Native::verify(&mut all, m) {
            for spare in &mut spares { spare.retain_failure(); }
            return Err(e);
        }
        keep_spares(spares);
        Ok(())
    }
    /// The second half of every launch's bracket: one full SDK snapshot, taken after each of
    /// `natives` received LOAD, must equal the expected identity each was launched with (whose
    /// own snapshot preceded its launch), and the helper file must be unchanged. A snapshot
    /// is of content, so it serves every helper whose LOAD came before it.
    fn verify(natives: &mut [&mut Native], m: &mut Measurements) -> Result<()> {
        let Some(first) = natives.first() else { return Ok(()) };
        let phase = Instant::now();
        let sdk_after = io(digest::installation(&first.root))?;
        m.installation_hash_b_ns += phase.elapsed().as_nanos();
        m.installation_snapshots += 1;
        let helper_after = io(digest::file(&first.helper))?;
        for native in natives.iter() {
            if native.root != first.root || native.helper != first.helper || sdk_after != native.sdk || helper_after != native.helper_id {
                return Err(error("native diagnostic startup identity changed"));
            }
        }
        for native in natives.iter_mut() { native.verified = true; }
        Ok(())
    }
    fn exchange(&mut self, op: &str, module: &str, marker: &str, expected: &str, claim: &str) -> Result<String> {
        check_owner()?;
        let extra = if op != "CHECK_DIAG" { String::new() } else {
            format!(",\"bytes\":{},\"marker\":\"{marker}\",\"claim\":\"{claim}\"", module.len())
        };
        let v2 = self.v2;
        let v2context = if v2 { io(digest::bytes(self.context.as_bytes()))? } else { super::proof_wire2::ZERO.into() };
        let v2module = if !v2 || module.is_empty() { super::proof_wire2::ZERO.into() } else { io(digest::bytes(module.as_bytes()))? };
        let empty = matches!(op, "LOAD" | "STOP");
        let v2source = if empty { super::proof_wire2::ZERO.to_string() } else { self.source.clone() };
        let v2body = if empty { super::proof_wire2::ZERO.to_string() } else { self.body.clone() };
        let binding = self.binding.clone(); let nonce = self.nonce.clone();
        let request = super::proof_wire2::Request { binding: &binding, nonce: &nonce, seq: self.seq, op, context: &v2context, source: &v2source, module: &v2module, body: &v2body, bytes: module.len(), marker };
        let reply = |line: &str, status: &str| if v2 { request.reply(line, status) } else { known_reply(line, &binding, &nonce, request.seq, op, status, claim) };
        let header = if self.v2 { request.header().ok_or_else(|| error("body diagnostic request identity refused"))? } else { format!("{{\"binding\":\"{}\",\"nonce\":\"{}\",\"op\":\"{op}\",\"seq\":{}{extra}}}\n", self.binding, self.nonce, self.seq) };
        if header.len() > 4096 || module.len() > 65536 { return Err(error("native diagnostic input bound")); }
        // Retain exact bounded frames/modules and raw replies for Root's independent evidence.
        let record = if op == "QUEUE" { "queue" } else { "request" };
        io(fs::write(self.path.join(format!("{record}-{}.json", self.seq)), &header))?;
        if !module.is_empty() { io(fs::write(self.path.join(format!("{record}-{}.lean", self.seq)), module))?; }
        let mut bytes = header.into_bytes();
        bytes.extend_from_slice(module.as_bytes());
        let mut input = self.input.take().ok_or_else(|| error("native diagnostic input unavailable"))?;
        let (send, receive) = mpsc::channel();
        // A blocked writer cannot suppress the parent deadline; killing our child closes its pipe.
        self.writer = Some(std::thread::spawn(move || {
            let result = input.write_all(&bytes).and_then(|_| input.flush());
            let _ = send.send((input, result));
        }));
        let start = Instant::now();
        let base = self.offset;
        let mut output = String::new();
        let payload = matches!(op, "CHECK_DIAG" | "CHECK" | "INSTALL_BODY");
        let mut began = !payload;
        let mut complete = false;
        loop {
            check_owner()?;
            if self.input.is_none() {
                match receive.try_recv() {
                    Ok((input, result)) => {
                        io(result)?;
                        self.input = Some(input);
                        if self.writer.take().unwrap().join().is_err() { return Err(error("native diagnostic writer panicked")); }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return Err(error("native diagnostic writer disconnected")),
                    Err(mpsc::TryRecvError::Empty) => (),
                }
            }
            if start.elapsed() > Duration::from_secs(10) { return Err(error("native diagnostic deadline")); }
            if io(fs::metadata(self.path.join("stderr")))?.len() != 0 { return Err(error("native diagnostic stderr refused")); }
            let length = io(fs::metadata(self.path.join("stdout")))?.len() as usize;
            if length < self.offset || length.saturating_sub(base) > 65536 { return Err(error("native diagnostic output bound")); }
            let bytes = bounded_output(&self.path.join("stdout"), base + 65536)?;
            if bytes.len() < self.offset || bytes.len().saturating_sub(base) > 65536 { return Err(error("native diagnostic output bound")); }
            if complete {
                if self.offset != bytes.len() { return Err(error("native diagnostic trailing data")); }
                if self.input.is_some() { return Ok(output); }
            }
            while let Some(end) = bytes[self.offset..].iter().position(|b| *b == b'\n') {
                let line = std::str::from_utf8(&bytes[self.offset..self.offset + end]).map_err(|_| error("native diagnostic UTF8 refused"))?;
                self.offset += end + 1;
                if payload && !began && reply(line, "begin") { began = true; continue; }
                if began && reply(line, expected) {
                    complete = true;
                    if self.offset != bytes.len() { return Err(error("native diagnostic trailing data")); }
                    if self.input.is_some() { return Ok(output); }
                    break;
                }
                if began && payload {
                    if let Some(message) = line.strip_prefix(if v2 { "BODY_INTERFACE_REUSE_DIAGNOSTIC:" } else { "UNSEALED_DIAGNOSTIC:" }) { output.push_str(message); output.push('\n'); continue; }
                }
                return Err(error("native diagnostic protocol or proof refusal"));
            }
            if !complete && io(self.child.try_wait())?.is_some() {
                self.leader_reaped = true;
                return Err(error("native diagnostic child exited"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn idle(&mut self, allow_clean_exit: bool) -> Result<()> {
        match io(self.child.try_wait())? {
            Some(status) if allow_clean_exit && status.success() => self.leader_reaped = true,
            Some(_) => { self.leader_reaped = true; return Err(error("native diagnostic late child exit")); }
            None if unsafe { getpgid(self.child.id() as i32) } == self.group_id => (),
            None => return Err(error("native diagnostic late group ownership unknown")),
        }
        if bounded_output(&self.path.join("stdout"), self.offset)?.len() != self.offset {
            return Err(error("native diagnostic completed output was truncated"));
        }
        bounded_output(&self.path.join("stderr"), 0)?;
        Ok(())
    }
    fn join_writer(&mut self) -> Result<()> {
        let start = Instant::now();
        if let Some(writer) = self.writer.as_ref() {
            while !writer.is_finished() {
                if start.elapsed() > Duration::from_secs(1) {
                    return Err(error("native diagnostic writer completion unknown; ownership retained"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if let Some(writer) = self.writer.take() {
            writer.join().map_err(|_| error("native diagnostic cleanup writer panicked"))?;
        }
        Ok(())
    }
    fn wait_leader(&mut self) -> Result<()> {
        let start = Instant::now();
        loop {
            if io(self.child.try_wait())?.is_some() {
                self.leader_reaped = true;
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(1) {
                return Err(error("native diagnostic child exit remains unknown"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn stop(&mut self) -> Result<()> {
        if self.stopped { return Ok(()); }
        // Never retry a group signal after a prior uncertain cleanup/reaped PID.
        if self.cleanup_started { return Err(error("native diagnostic cleanup remains unknown")); }
        self.cleanup_started = true;
        self.input.take();
        if !self.private_group {
            // Never signal the inherited group: it belongs to the caller. Child
            // retains wait ownership, so kill cannot target a reused/reaped PID.
            if !self.leader_reaped { self.leader_reaped = io(self.child.try_wait())?.is_some(); }
            if !self.leader_reaped {
                // A clean exit can race kill; wait on the owned child in either case.
                let killed = self.child.kill();
                if let Err(error) = self.wait_leader() { io(killed)?; return Err(error); }
            }
            self.join_writer()?;
            self.stopped = true;
            return Ok(());
        }
        let group = -(self.child.id() as i32);
        if !self.leader_reaped { self.leader_reaped = io(self.child.try_wait())?.is_some(); }
        // macOS may remove process metadata before WNOHANG observes our child's
        // terminal status. Wait only on that owned Child; absence alone is not death.
        if !self.leader_reaped && unsafe { getpgid(self.child.id() as i32) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(3) {
            self.wait_leader()?;
        }
        // Once try_wait reaped the leader, its numeric PGID may be reused: observe only.
        if !self.leader_reaped {
            if unsafe { kill(group, 9) } != 0 && std::io::Error::last_os_error().raw_os_error() != Some(3) {
                self.leader_reaped = io(self.child.try_wait())?.is_some();
                if !self.leader_reaped {
                    if self.child.kill().is_ok() && self.wait_leader().is_ok() { let _ = self.join_writer(); }
                    return Err(error("native diagnostic group kill refused; ownership retained unknown"));
                }
            }
            if !self.leader_reaped { self.wait_leader()?; }
        }
        // Only join after kill/wait has broken our writer's potentially blocked pipe.
        self.join_writer()?;
        let start = Instant::now();
        loop {
            if unsafe { kill(group, 0) } != 0 {
                if std::io::Error::last_os_error().raw_os_error() != Some(3) {
                    return Err(error("native diagnostic group release unknown"));
                }
                self.stopped = true;
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(1) { return Err(error("native diagnostic descendant group remains")); }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn retain_failure(&mut self) {
        // Cancelled production work drops normally. Drop still retains evidence
        // if it cannot confirm child or writer cleanup.
        if !self.keep && super::compiler_session::cancelled() {
            return;
        }
        self.keep = true;
        // Stop before retaining a bounded prefix; record original sizes on a limit violation.
        if let Err(e) = self.stop() { super::compiler_diagnostic!("{}: {}", e.code, e.message); }
        for (name, limit) in [("stdout", 9 * 65536), ("stderr", 65536)] {
            let path = self.path.join(name);
            if let Ok(meta) = fs::metadata(&path) {
                if meta.len() > limit {
                    super::compiler_diagnostic!("native diagnostic {name} original bytes: {}", meta.len());
                    if self.stopped { let _ = fs::OpenOptions::new().write(true).open(path).and_then(|f| f.set_len(limit)); }
                }
            }
        }
        // Only a helper a snapshot after its LOAD has verified may have its output read back
        // as a request's refusal; another's stays on disk as raw evidence.
        if self.verified { LAST_FAILURE.set(Some(self.path.clone())); }
        if !QUIET.get() { super::compiler_diagnostic!("native diagnostic raw evidence: {}", self.path.display()); }
    }

}
impl Drop for Native {
    fn drop(&mut self) {
        if let Err(e) = self.stop() { self.keep = true; super::compiler_diagnostic!("{}: {}", e.code, e.message); }
        if let Some(writer) = self.writer.take() {
            super::compiler_diagnostic!("native diagnostic outstanding writer retained: child {}; evidence {}", self.child.id(), self.path.display());
            OUTSTANDING_WRITERS.lock().unwrap_or_else(|e| e.into_inner()).push((self.child.id(), self.path.clone(), writer));
        }
        if !self.keep { let _ = fs::remove_dir_all(&self.path); }
    }
}
impl Session {
    pub(super) fn publishing() -> Result<Option<Self>> {
        super::ready::Binding::configured()?.map(Self::for_binding).transpose()
    }
    pub(super) fn for_binding(binding: super::ready::Binding) -> Result<Self> {
        let mut session = Self::body_generation(&binding.sdk)?;
        session.ready = Some(binding);
        Ok(session)
    }
    pub(super) fn production(&self) -> bool { self.ready.is_some() }
    /// What a cached proof is tied to: the selected checker record (helper, SDK and helper source hashes).
    pub(super) fn note_cached_proofs(&mut self) { self.used_cached_proofs = true; }
    pub(super) fn cache_identity(&self) -> Option<&str> { self.ready.as_ref().map(|binding| binding.identity.as_str()) }
    pub(super) fn poisoned(&self) -> bool { self.poisoned }
    pub(super) fn available(&self) -> bool {
        // Reserve a possible INSTALL before planning the next source, plus STOP.
        // This prevents a late first generic body from turning a clean budget end into a protocol refusal.
        let limit = BODY_OPERATIONS - usize::from(self.installed_body.is_none());
        !self.poisoned && self.native.as_ref().is_none_or(|n| n.seq < limit)
    }
    /// Renew only a healthy, exhausted production helper. The original operator
    /// selection belongs to the whole build loop, not to individual helpers.
    /// Never reread the environment to silently change generation or fall back.
    pub(super) fn renew(&mut self) -> Result<()> {
        if self.poisoned || self.available() { return Err(error("checker renewal requires healthy budget exhaustion")); }
        let binding = self.ready.clone().ok_or_else(|| error("only production checkers may renew"))?;
        binding.validate()?;
        self.validate_publication()?;
        self.finish()?; // Checked STOP and actual child reap precede replacement.
        binding.validate()?;
        // Clear all query/library state. The next proof performs normal full
        // startup SDK A/B checks and kernel-checks the freshly installed body.
        *self = Self::for_binding(binding)?;
        Ok(())
    }
    /// Replace an exhausted helper between the requests of one build, keeping
    /// that build's request and source identity. Startup checks repeat in full.
    pub(super) fn renew_within_build(&mut self) -> Result<()> {
        let (request, source) = (self.request.clone(), self.source.clone());
        self.renew()?;
        self.request = request;
        self.source = source;
        Ok(())
    }
    pub(super) fn validate_publication(&mut self) -> Result<()> {
        if self.poisoned { return Err(error("build session refused; restart required")); }
        if (self.native.is_some() || self.used_cached_proofs) && let Some(binding) = &self.ready {
            binding.validate()?;
        }
        self.validate_idle()
    }
    /// Operator explicitly assumes the selected private copy stays unchanged until exit.
    /// This is not a verified immutable object or a portable proof-publication policy.
    pub(super) fn trusted_generation(expected: &str) -> Result<Self> {
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(error("trusted generation requires a lowercase SHA256 identity"));
        }
        Ok(Self { expected_generation: Some(expected.into()), ..Self::default() })
    }
    pub(super) fn body_generation(expected: &str) -> Result<Self> {
        let mut session = Self::trusted_generation(expected)?;
        session.body_mode = true;
        Ok(session)
    }
    pub(super) fn body_enabled(&self) -> bool { self.body_mode }
    pub(super) fn installed_body(&self) -> Option<&str> { self.installed_body.as_deref() }
    pub(super) fn queue_body(&mut self, key: &str, module: String, marker: String) -> Result<()> {
        if !self.body_mode || self.poisoned || self.installed_body.is_some() || self.pending_body.is_some()
            || !super::proof_wire2::hash(key) || key == super::proof_wire2::ZERO { self.refuse(); return Err(error("body diagnostic install state refused")); }
        self.pending_body = Some((key.into(), module, marker));
        Ok(())
    }
    pub(super) fn select_body(&mut self, key: Option<&str>) -> Result<()> {
        if !self.body_mode || self.poisoned { self.refuse(); return Err(error("body diagnostic selection state refused")); }
        if let Some(key) = key {
            let pending = self.pending_body.as_ref().map(|(key, _, _)| key.as_str());
            if key == super::proof_wire2::ZERO || !super::proof_wire2::hash(key) || (Some(key) != self.installed_body.as_deref() && Some(key) != pending) {
                self.refuse(); return Err(error("body diagnostic selected interface unavailable"));
            }
        }
        self.selected_body = key.map(str::to_owned);
        Ok(())
    }
    pub(super) fn trust_fields(&self) -> String {
        if let Some(binding) = &self.ready {
            return format!(",\"trust_policy\":\"selected_readonly_generation_no_concurrent_mutation\",\"verified_immutable\":false,\"sdk_path_currentness\":false,\"generation_selection\":\"until_session_exit\",\"ready_binding_sha256\":\"{}\",\"body_interface_reuse\":{},\"axiom_allowlist\":[\"propext\",\"Classical.choice\",\"Quot.sound\"]", binding.identity, self.selected_body.is_some());
        }
        let fields = self.expected_generation.as_ref().map(|expected| format!(",\"trust_policy\":\"selected_readonly_generation_no_concurrent_mutation\",\"expected_generation_sha256\":\"{expected}\",\"startup_sdk_checked\":{},\"verified_immutable\":false,\"sdk_path_currentness\":false,\"generation_selection\":\"until_session_exit\"", self.native.is_some())).unwrap_or_default();
        if self.body_mode { format!("{fields},\"body_interface_reuse\":true,\"library_slots\":1,\"library_installed\":{},\"body_install_ns\":{},\"body_application_selected\":{}", self.installed_body.is_some(), self.body_install_ns, self.selected_body.is_some()) } else { fields }
    }
    pub(super) fn diagnostic_identity(&self) -> (&'static str, &'static str) {
        if self.production() { return ("selected_generation_kernel_checks", "witchy.selected-generation.kernel.v1"); }
        if self.body_mode { return ("body_interface_session_diagnostic", "witchy.body-interface.session.diagnostic.v2"); }
        if self.expected_generation.is_some() {
            ("trusted_generation_session_diagnostic", "witchy.trusted-generation.session.diagnostic.v1")
        } else { ("native_session_diagnostic", "witchy.native.session.diagnostic.v1") }
    }
    pub(super) fn mode(&self) -> &'static str {
        if self.production() { return "SELECTED_GENERATION_KERNEL_CHECKS"; }
        if self.body_mode { return super::proof_wire2::MODE; }
        if self.expected_generation.is_some() { "TRUSTED_GENERATION_DIAGNOSTIC" } else { "UNSEALED_DIAGNOSTIC" }
    }
    pub(super) fn refuse(&mut self) {
        self.poisoned = true;
        if let Some(mut native) = self.native.take() { native.retain_failure(); }
    }
    pub(super) fn finish(&mut self) -> Result<()> {
        if let Some(native) = self.native.as_mut() {
            if native.seq < if native.v2 { BODY_OPERATIONS + 1 } else { 8 } {
                native.seq += 1;
                if let Err(e) = native.exchange("STOP", "", "", "closed", "") { self.refuse(); return Err(e); }
            }
            // A verified closed frame permits a bounded graceful exit, not an
            // inference from EPERM or permission to kill a possibly reused PGID.
            if let Err(e) = native.wait_leader().and_then(|_| native.idle(true)) { self.refuse(); return Err(e); }
            if let Err(e) = native.stop() { self.refuse(); return Err(e); }
        }
        self.native.take();
        Ok(())
    }
    pub(super) fn validate_idle(&mut self) -> Result<()> {
        let result = match self.native.as_mut() { Some(native) => native.idle(native.seq == 8), None => Ok(()) };
        if result.is_err() { self.refuse(); }
        result
    }
    pub(super) fn request(&mut self, id: &str, source: &str) { self.request = format!("{id}:{source}"); self.source = source.into(); self.selected_body = None; }
    /// Only the current operator-built helper understands QUEUE. Keep old
    /// bindings, retained-body checks and already active sessions on CHECK.
    pub(super) fn queue_independent(&mut self, requests: &[(String, String, usize)]) {
        if self.native.is_none() && self.pending_body.is_none() && self.selected_body.is_none()
            && self.ready.as_ref().is_some_and(|b| b.fixed_header) && (2..=5).contains(&requests.len()) {
            self.independent = requests.iter().map(|(m, k, _)| (m.clone(), k.clone())).collect();
        }
    }
    pub(super) fn run(&mut self, module: &str, marker: &str, context: &str, m: &mut Measurements) -> Result<(String, String, String)> {
        // Setup can refuse before creating any evidence for this request.
        LAST_FAILURE.set(None);
        if self.poisoned || !OUTSTANDING_WRITERS.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return Err(error("native diagnostic session poisoned or outstanding writer retained"));
        }
        let result = self.run_inner(module, marker, context, m);
        if result.is_err() {
            SPARES.with_borrow_mut(|pool| if let Some(pool) = pool { pool.refused = true });
            self.refuse();
        }
        result
    }
    /// End this session's use of its helper. Inside a `Spares` scope a healthy verified
    /// production helper with budget left is kept for the next probe session instead.
    pub(super) fn park(&mut self) -> Result<()> {
        let keep = spares_open() && !self.poisoned && self.ready.is_some() && self.installed_body.is_none()
            && self.pending_body.is_none() && self.available() && self.native.as_ref().is_some_and(|native| native.verified);
        if !keep { return self.finish(); }
        let mut native = self.native.take().unwrap();
        // Only a helper still running with no late output is kept.
        if let Err(e) = native.idle(false) { self.poisoned = true; native.retain_failure(); return Err(e); }
        keep_spares(vec![native]);
        Ok(())
    }
    fn run_inner(&mut self, module: &str, marker: &str, context: &str, m: &mut Measurements) -> Result<(String, String, String)> {
        let context = self.expected_generation.as_ref().map(|expected|
            format!("{context}:selected_readonly_generation_no_concurrent_mutation:{expected}")).unwrap_or_else(|| context.into());
        let phase = Instant::now();
        if let Some(binding) = &self.ready { binding.validate()?; }
        let configured = match &self.ready { Some(binding) => binding.root.clone(), None =>
            PathBuf::from(std::env::var_os("WITCHY_WASM_LEAN_ROOT").ok_or_else(|| error("native diagnostic requires trusted Lean root"))?) };
        let helper = match &self.ready { Some(binding) => binding.helper.clone(), None => Native::configured_helper()? };
        if !configured.is_absolute() { return Err(error("native diagnostic root must be absolute")); }
        let root = io(fs::canonicalize(&configured))?;
        m.configuration_ns += phase.elapsed().as_nanos();
        let per_request_scans = self.expected_generation.is_none();
        if self.native.is_none() && !per_request_scans && self.ready.is_some() && self.independent.is_empty() && self.pending_body.is_none() {
            // A kept helper was launched for this same selection and verified (`Spares`).
            let (expected, selected) = (self.expected_generation.as_deref(), self.ready.as_ref().map(|binding| binding.helper_id.as_str()));
            let fits = |n: &Native| n.verified && !n.keep && n.v2 == self.body_mode && Some(n.sdk.as_str()) == expected && Some(n.helper_id.as_str()) == selected
                && n.root_setting == configured && n.root == root && n.helper == helper && n.context == context && n.seq + 1 < BODY_OPERATIONS;
            self.native = SPARES.with_borrow_mut(|pool| {
                let kept = &mut pool.as_mut()?.kept;
                let at = kept.iter().position(fits)?;
                Some(kept.swap_remove(at))
            });
            // One that exited or wrote since is discarded (its evidence kept, never this
            // request's), and this session launches its own as if none were kept. A kept helper's
            // evidence holds its earlier probes' requests too; only a refused one prints errors.
            if let Some(mut native) = self.native.take_if(|native| native.idle(false).is_err()) {
                let last = LAST_FAILURE.with_borrow_mut(Option::take);
                native.retain_failure();
                LAST_FAILURE.set(last);
            }
        }
        let phase = Instant::now();
        let sdk = if self.native.is_none() || per_request_scans {
            let sdk = io(digest::installation(&root))?;
            m.installation_hash_a_ns += phase.elapsed().as_nanos();
            m.installation_snapshots += 1;
            sdk
        } else { self.native.as_ref().unwrap().sdk.clone() };
        if self.expected_generation.as_ref().is_some_and(|expected| expected != &sdk) {
            return Err(error("trusted generation differs from expected startup identity"));
        }
        if self.native.is_none() {
            self.native = Some(Native::launch(sdk.clone(), root.clone(), configured.clone(), helper.clone(), &context, self.body_mode, self.ready.is_none())?);
            if self.ready.as_ref().is_some_and(|binding| binding.helper_id != self.native.as_ref().unwrap().helper_id) {
                return Err(error("selected ready binding/root/helper changed; restart with a new generation"));
            }
            if self.ready.is_some() && !per_request_scans && spares_refused() {
                // An explanation probe: start the next probes' helpers now, after the same
                // snapshot, and verify them all with one snapshot after their LOADs.
                let native = self.native.as_mut().unwrap();
                let spares = Native::spares(native);
                Native::verify_kept(Some(native), spares, m)?;
            }
        }
        let native = self.native.as_mut().unwrap();
        if native.root_setting != configured || native.root != root || helper != native.helper
            || native.sdk != sdk || native.context != context || io(digest::file(&native.helper))? != native.helper_id {
            return Err(error("native diagnostic pinned inputs changed"));
        }
        if native.seq >= if self.body_mode { BODY_OPERATIONS } else { 8 } { return Err(error("native diagnostic request cap; restart")); }
        // Every exchange of this request; nothing it replied leaves this call before the checks below.
        let result = (|| {
            if self.body_mode {
                native.source = self.source.clone();
                if let Some((key, install, install_marker)) = self.pending_body.take() {
                    if native.seq >= BODY_OPERATIONS - 1 { return Err(error("body install would exhaust operation budget")); }
                    native.seq += 1; native.body = key.clone();
                    let install_start = Instant::now();
                    let output = native.exchange("INSTALL_BODY", &install, &install_marker, "ok", "")?;
                    super::proof::validate_body_install(&output, &install_marker, &key)?;
                    native.idle(false)?;
                    self.body_install_ns = install_start.elapsed().as_nanos();
                    self.installed_body = Some(key);
                }
                native.body = self.selected_body.clone().unwrap_or_else(|| super::proof_wire2::ZERO.into());
            }
            if !self.independent.is_empty() {
                if native.seq != 0 || native.body != super::proof_wire2::ZERO { return Err(error("independent check state refused")); }
                for (ordinal, (text, key)) in std::mem::take(&mut self.independent).into_iter().enumerate() {
                    native.seq = ordinal + 1;
                    native.exchange("QUEUE", &text, &key, "queued", "")?;
                    native.idle(false)?;
                }
                native.seq = 0;
            }
            native.seq += 1;
            let phase = Instant::now();
            let claim = io(digest::bytes(format!("{}:{context}:{module}", self.request).as_bytes()))?;
            let result = native.exchange(if self.body_mode { "CHECK" } else { "CHECK_DIAG" }, module, marker, "ok", &claim);
            let request = phase.elapsed().as_nanos();
            m.child_spawn_to_exit_ns += request;
            m.slowest_request_ns = m.slowest_request_ns.max(request);
            result
        })();
        // A fresh helper's post-LOAD snapshot: before anything it replied (output, an error,
        // refusal evidence or spares) leaves this call, so it also covers what it read since.
        let fresh = !native.verified;
        if fresh {
            check_owner()?;
            Native::verify(&mut [&mut *native], m)?;
        }
        if per_request_scans && !fresh {
            let phase = Instant::now();
            let after = io(digest::installation(&root))?;
            m.installation_hash_b_ns += phase.elapsed().as_nanos();
            m.installation_snapshots += 1;
            if after != sdk { native.verified = false; return Err(error("native diagnostic SDK changed during check")); }
        }
        if io(digest::file(&native.helper))? != native.helper_id { native.verified = false; return Err(error("native diagnostic helper changed during check")); }
        if result.is_err() && fresh && self.ready.is_some() && !per_request_scans && SPARES.with_borrow(|pool| pool.as_ref().is_some_and(|pool| pool.kept.is_empty())) {
            // A refusal: the explanation's probes follow. The snapshot that just verified this
            // helper is also the one taken just before these launches, as each launch has.
            // The refused helper is stopped first; its evidence is retained as before.
            if native.stop().is_ok() {
                let spares = Native::spares(native);
                // Spares that fail their snapshot are not kept (their evidence stays unread);
                // the refusal stands, and the next probe's own snapshot meets the change.
                let _ = Native::verify_kept(None, spares, m);
            }
        }
        let output = result?; // Preserve the primary protocol/proof error before late idle observations.
        native.idle(native.seq == 8)?;
        Ok((sdk, output, native.helper_id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_early_refusal_cannot_return_evidence_from_an_earlier_request() {
        LAST_FAILURE.set(Some(PathBuf::from("previous checker request")));
        let mut session = Session::default();
        session.refuse();
        let error = session.run("", "", "", &mut Measurements::default()).unwrap_err();
        assert!(error.message.contains("session poisoned"));
        assert!(take_last_failure().is_none());
    }

    #[test]
    fn reply_requires_exact_binding_state_types_and_complete_keyset() {
        let line = "WITCHY_RESIDENT_V1:{\"status\":\"ok\",\"binding\":\"ab\",\"nonce\":\"cd\",\"seq\":1,\"phase\":\"CHECK_DIAG\",\"mode\":\"UNSEALED_DIAGNOSTIC\",\"claim\":\"ef\"}";
        let accepts = |s: &str| known_reply(s, "ab", "cd", 1, "CHECK_DIAG", "ok", "ef");
        assert!(accepts(line));
        for bad in [
            line.replace("\"seq\":1", "\"seq\":\"1\""),
            line.replace("\"seq\":1", "\"seq\":2"),
            line.replace("\"claim\":\"ef\"", "\"claim\":\"other\""),
            line.replace("\"status\":\"ok\"", "\"status\":\"error\""),
            line.replace("\"claim\":\"ef\"", "\"claim\":\"ef\",\"claim\":\"ef\""),
            line.replace("\"claim\":\"ef\"", "\"claim\":\"ef\",\"extra\":0"),
            line.replace("\"claim\":\"ef\"", "\"claim\":\"e\\u0066\""),
            line.replace("UNSEALED_DIAGNOSTIC", "VERIFIED"),
        ] { assert!(!accepts(&bad), "{bad}"); }
    }
    #[test]
    fn actual_proof_free_pipeline_never_initializes_a_native_checker() {
        let mut session = Session::default();
        let (_, report, measurements) = super::super::compile_backend("fn main():\n    return\n", &[], Some(&mut session)).unwrap();
        assert!(session.native.is_none());
        assert_eq!(measurements.lean_invocations, 0);
        assert!(!report.contains("fresh_kernel_checks") && !report.contains("native_session_diagnostic"));
    }
    #[test]
    fn malformed_generation_identity_refuses_before_source_or_stdin() {
        for bad in [String::new(), "0".repeat(63), "A".repeat(64), format!("{}\n", "0".repeat(64))] {
            let error = super::super::diagnostic_trusted_generation_loop(Path::new("missing-source-must-not-be-read"), &bad).unwrap_err();
            assert!(error.contains("lowercase SHA256 identity"));
        }
    }
    #[test]
    fn explicit_generation_proof_free_pipeline_keeps_startup_lazy_and_bytes_identical() {
        let source = "fn main():\n    return\n";
        let expected = "5a".repeat(32);
        let mut session = Session::trusted_generation(&expected).unwrap();
        let (bytes, report, m) = super::super::compile_backend(source, &[], Some(&mut session)).unwrap();
        let (ordinary_bytes, ordinary_report, _) = super::super::compile_measured(source).unwrap();
        assert_eq!(bytes, ordinary_bytes); assert_eq!(report, ordinary_report);
        assert_eq!(m.proof.installation_snapshots, 0); assert!(session.native.is_none());
        assert_eq!(session.expected_generation.as_deref(), Some(expected.as_str()));
        assert!(session.trust_fields().contains("\"startup_sdk_checked\":false"));
    }

    /// A stand-in helper speaking the v2 wire: a check whose marker holds `FAIL` is refused
    /// (and, like the real helper, ends it). Its `load-count` file counts LOADs; when that count
    /// reaches the number in `change-at`, the LOAD also changes the SDK it was given, and at
    /// the number in `forge-at` it answers LOAD with a Lean error line naming the first goal.
    const FAKE_HELPER: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
here=${0%/*}
# The request's own fields, with `op` as `phase` and no `bytes`, plus a status and the mode.
reply() {
    fields=${line%\}}; fields="${fields%%,\"op\":*}${fields#*\"op\":\"$op\"}"; fields="${fields%%,\"bytes\":*}${fields#*,\"bytes\":$bytes}"
    printf 'WITCHY_RESIDENT_V2:%s,"phase":"%s","status":"%s","mode":"BODY_INTERFACE_REUSE_DIAGNOSTIC"}\n' "$fields" "$op" "$1"
}
while IFS= read -r line; do
    op=${line#*\"op\":\"}; op=${op%%\"*}; bytes=${line#*\"bytes\":}; bytes=${bytes%%,*}; marker=${line#*\"marker\":\"}; marker=${marker%%\"*}
    if [ "$bytes" -gt 0 ]; then dd bs=1 count="$bytes" of=/dev/null 2>/dev/null; fi
    case $op in
        LOAD)
            count=$(( $(cat "$here/load-count" 2>/dev/null || echo 0) + 1 )); echo $count > "$here/load-count"
            if [ "$count" = "$(cat "$here/change-at" 2>/dev/null)" ]; then echo changed >> "$1/library"; fi
            if [ "$count" = "$(cat "$here/forge-at" 2>/dev/null)" ]; then echo 'x.lean:1:1: error: forged'; exit 1; fi
            reply loaded-untrusted;;
        CHECK)
            reply begin
            case $marker in *FAIL*) reply error; exit 1;; esac
            reply ok;;
        STOP) reply closed; exit 0;;
        *) exit 2;;
    esac
done
"#;
    struct Toolchain { dir: PathBuf, binding: super::super::ready::Binding }
    impl Toolchain {
        fn new(name: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-spares-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("sdk")).unwrap();
            fs::write(dir.join("sdk/library"), "selected\n").unwrap();
            let helper = dir.join("helper");
            fs::write(&helper, FAKE_HELPER).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            let text = format!("witchy.ready-toolchain.v1\nhelper={}\nhelper_sha256={}\nsdk={}\nsdk_sha256={}\nhelper_mode=body-interface-v2\nsource_sha256=3a03a52e04eff6139819aa3a189794d47418ca7afec1e1320512c07136932ea8\ntrust_policy=selected_readonly_generation_no_concurrent_mutation\n",
                helper.display(), digest::file(&helper).unwrap(), dir.join("sdk").display(), digest::installation(&dir.join("sdk")).unwrap());
            fs::write(dir.join("binding"), text).unwrap();
            let binding = super::super::ready::Binding::load(&dir.join("binding")).unwrap();
            Self { dir, binding }
        }
        fn change_sdk_at_load(&self, load: usize) { fs::write(self.dir.join("change-at"), format!("{load}\n")).unwrap(); }
        fn forge_at_load(&self, load: usize) { fs::write(self.dir.join("forge-at"), format!("{load}\n")).unwrap(); }
        fn loads(&self) -> usize { fs::read_to_string(self.dir.join("load-count")).map_or(0, |n| n.trim().parse().unwrap()) }
        /// One request in a fresh session for the selected binding; its snapshot count.
        fn check(&self, marker: &str) -> (Result<(String, String, String)>, usize) { self.check_in("context", marker) }
        fn check_in(&self, context: &str, marker: &str) -> (Result<(String, String, String)>, usize) {
            let mut session = Session::for_binding(self.binding.clone()).unwrap();
            session.request("test", &"a".repeat(64));
            let mut m = Measurements::default();
            let result = session.run("theorem t : True := trivial\n", marker, context, &mut m);
            if result.is_ok() { session.park().unwrap(); }
            (result, m.installation_snapshots)
        }
    }
    impl Drop for Toolchain { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }
    fn kept() -> usize { SPARES.with_borrow(|pool| pool.as_ref().map_or(0, |pool| pool.kept.len())) }

    #[test]
    fn a_refusal_starts_verified_spares_that_later_probes_take_without_new_snapshots() {
        let toolchain = Toolchain::new("taken");
        let scope = Spares::open();
        // Before LOAD, after LOAD (after the check), after the spares' LOADs.
        let (result, snapshots) = toolchain.check("FAIL:1");
        assert!(result.is_err() && take_last_failure().is_some());
        assert_eq!((snapshots, kept(), toolchain.loads()), (3, SPARE_HELPERS, 1 + SPARE_HELPERS));
        // A passing probe keeps its helper for the next; a refused one ends it.
        let (result, snapshots) = toolchain.check("PASS:1");
        assert!(result.is_ok());
        assert_eq!((snapshots, kept()), (0, SPARE_HELPERS));
        let (result, snapshots) = toolchain.check("FAIL:2");
        assert!(result.is_err() && take_last_failure().is_some());
        assert_eq!((snapshots, kept(), toolchain.loads()), (0, SPARE_HELPERS - 1, 1 + SPARE_HELPERS));
        drop(scope);
        assert_eq!(kept(), 0);
        // Outside a scope nothing is kept: every request launches and snapshots twice.
        let (result, snapshots) = toolchain.check("FAIL:3");
        assert!(result.is_err());
        assert_eq!((snapshots, kept(), toolchain.loads()), (2, 0, 2 + SPARE_HELPERS));
    }

    #[test]
    fn an_sdk_changed_during_load_refuses_the_helper_and_its_evidence() {
        let toolchain = Toolchain::new("changed");
        toolchain.change_sdk_at_load(1);
        let _scope = Spares::open();
        let (result, _) = toolchain.check("FAIL:1");
        assert!(result.unwrap_err().message.contains("identity changed"));
        // What the changed helper wrote is never read back as the request's refusal.
        assert!(take_last_failure().is_none());
        assert_eq!(kept(), 0);
    }

    #[test]
    fn a_probe_with_no_kept_helper_launches_a_verified_group() {
        let toolchain = Toolchain::new("group");
        let _scope = Spares::open();
        let _ = toolchain.check("FAIL:1");
        // Take both spares and end them: the pool is empty and a request was refused.
        for marker in ["FAIL:2", "FAIL:3"] { assert_eq!(toolchain.check(marker).1, 0); }
        assert_eq!((kept(), toolchain.loads()), (0, 1 + SPARE_HELPERS));
        // Before the launches, after all their LOADs.
        let (result, snapshots) = toolchain.check("PASS:1");
        assert!(result.is_ok());
        assert_eq!((snapshots, kept(), toolchain.loads()), (2, SPARE_HELPERS + 1, 2 * (1 + SPARE_HELPERS)));
        // A kept helper serves only its own selection and budget.
        let (result, snapshots) = toolchain.check_in("another context", "PASS:2");
        assert!(result.is_ok());
        assert_eq!((snapshots, toolchain.loads()), (2, 2 * (1 + SPARE_HELPERS) + 1));
        SPARES.with_borrow_mut(|pool| for native in &mut pool.as_mut().unwrap().kept { native.seq = BODY_OPERATIONS - 1; });
        let before = kept();
        let (result, snapshots) = toolchain.check("PASS:3");
        assert!(result.is_ok());
        assert_eq!((snapshots, kept(), toolchain.loads()), (2, before + 1, 2 * (1 + SPARE_HELPERS) + 2));
    }

    #[test]
    fn an_sdk_changed_during_a_groups_load_keeps_no_helper() {
        let toolchain = Toolchain::new("group-changed");
        let _scope = Spares::open();
        let _ = toolchain.check("FAIL:1");
        for marker in ["FAIL:2", "FAIL:3"] { let _ = toolchain.check(marker); }
        toolchain.change_sdk_at_load(2 + SPARE_HELPERS);
        let (result, snapshots) = toolchain.check("PASS:1");
        assert!(result.unwrap_err().message.contains("identity changed"));
        assert_eq!((snapshots, kept()), (2, 0));
        assert!(take_last_failure().is_none());
    }

    #[test]
    fn a_passing_check_is_refused_when_the_sdk_changed_during_its_load() {
        let toolchain = Toolchain::new("admission");
        toolchain.change_sdk_at_load(1);
        let (result, snapshots) = toolchain.check("PASS:1");
        assert!(result.unwrap_err().message.contains("identity changed"));
        assert_eq!(snapshots, 2);
    }

    #[test]
    fn a_helper_failing_its_load_is_never_read_back_as_a_refusal() {
        let toolchain = Toolchain::new("forged");
        toolchain.forge_at_load(1);
        let (result, _) = toolchain.check("PASS:1");
        assert!(result.is_err());
        assert!(take_last_failure().is_none());
    }

    #[test]
    fn an_sdk_changed_during_a_spares_load_keeps_no_spare() {
        let toolchain = Toolchain::new("spare-changed");
        toolchain.change_sdk_at_load(2);
        let _scope = Spares::open();
        // The refusal stands, with its verified helper's evidence; no spare is kept.
        let (result, snapshots) = toolchain.check("FAIL:1");
        assert!(!result.unwrap_err().message.contains("identity"));
        assert!(take_last_failure().is_some());
        assert_eq!((snapshots, kept()), (3, 0));
        // The next probe's own snapshot before launch refuses the changed SDK.
        let (result, _) = toolchain.check("PASS:1");
        assert!(result.unwrap_err().message.contains("differs from expected"));
    }
}
