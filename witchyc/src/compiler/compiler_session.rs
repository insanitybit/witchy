//! Operation-scoped compiler. A private Unix socket serves length-prefixed UTF-8 arguments.
//! The creating process owns stdin. EOF closes the listener and drops all snapshots.
use super::{Heap, allocator, caps, check, files, logical_body, proof_session, ready};
use crate::{ast, diagnostic::Diagnostic};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    fmt, fs,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

pub(super) struct CheckedSource {
    pub syntax: Arc<ast::Program>,
    pub logical: logical_body::Program,
    prepared: std::sync::OnceLock<Result<super::proof_body::Prepared, Diagnostic>>,
}
impl CheckedSource {
    pub(super) fn from_syntax(syntax: ast::Program) -> Result<Arc<Self>, Diagnostic> {
        let logical = check::program(&syntax)?;
        Ok(Arc::new(Self { syntax: Arc::new(syntax), logical, prepared: Default::default() }))
    }
    pub(super) fn into_parts(self: Arc<Self>) -> (Arc<ast::Program>, logical_body::Program) {
        match Arc::try_unwrap(self) {
            Ok(owned) => (owned.syntax, owned.logical),
            Err(shared) => (shared.syntax.clone(), shared.logical.clone()),
        }
    }
    pub(super) fn prepared(&self) -> Result<&super::proof_body::Prepared, Diagnostic> {
        self.prepared
            .get_or_init(|| super::source_vc::prepare_with_sites(&self.logical))
            .as_ref()
            .map_err(Clone::clone)
    }
    fn proof_free(&self) -> bool {
        !self
            .syntax
            .declarations
            .iter()
            .any(|d| matches!(d, ast::Declaration::Lemma(_)))
            && self.prepared().is_ok_and(|p| p.goals.is_empty())
    }
}
struct Analysis {
    image: Option<Arc<Image>>,
    source: String,
    starts: Vec<usize>,
    checked: Arc<CheckedSource>,
}
#[derive(Default)]
struct Cache {
    entries: VecDeque<Analysis>,
    hits: u64,
    misses: u64,
    reused_images: u64,
    reviewed_builds: u64,
    proof_free_builds: u64,
}
thread_local! {
    static CACHE: RefCell<Option<Arc<Mutex<Cache>>>> = const { RefCell::new(None) };
    static STOP: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static PROOF_TIME: std::cell::Cell<(u128,usize)> = const { std::cell::Cell::new((0,0)) };
    static MESSAGES: RefCell<Option<String>> = const { RefCell::new(None) };
}
/// On the session's worker lanes, observe shutdown requested by the owner.
pub(super) fn cancelled() -> bool {
    STOP.with_borrow(|stop| stop.as_ref().is_some_and(|stop| stop.load(Ordering::Acquire)))
}

pub(super) fn diagnostic(args: fmt::Arguments<'_>) {
    use fmt::Write;
    MESSAGES.with_borrow_mut(|messages| match messages {
        Some(text) => {
            let _ = writeln!(text, "{args}");
        }
        None => eprintln!("{args}"),
    });
}

/// Paths and authority decisions are not part of this value. Check rules read the parsed
/// source and module boundaries. All path-based rules use the current request's Part table.
pub(super) fn analyze(
    source: &str,
    starts: &[usize],
) -> Result<(Arc<CheckedSource>, u128), Diagnostic> {
    analyze_target(source, starts, crate::parser::Target::Wasm)
}

pub(super) fn analyze_target(
    source: &str,
    starts: &[usize],
    target: crate::parser::Target,
) -> Result<(Arc<CheckedSource>, u128), Diagnostic> {
    // The existing shared session caches Wasm analyses.
    let shared = if target == crate::parser::Target::Wasm && !crate::parser::debug_build() { CACHE.with_borrow(Clone::clone) } else { None };
    let hit = shared.as_ref().and_then(|shared| {
        let mut cache = shared.lock().unwrap();
        let pos = cache
            .entries
            .iter()
            .position(|a| a.starts == starts && a.source == source)?;
        let item = cache.entries.remove(pos)?;
        let value = (item.checked.clone(), 0);
        cache.entries.push_back(item);
        cache.hits += 1;
        Some(value)
    });
    if let Some(value) = hit {
        return Ok(value);
    }
    let start = Instant::now();
    let syntax = super::methods::lift(crate::parser::parse_target(
        source, starts, &Default::default(), None, None, target,
    )?)?;
    let parse_ns = start.elapsed().as_nanos();
    let logical = check::program(&syntax)?;
    let checked = Arc::new(CheckedSource {
        syntax: Arc::new(syntax),
        logical,
        prepared: Default::default(),
    });
    if let Some(shared) = shared {
        let mut cache = shared.lock().unwrap();
        cache.misses += 1;
        if !cache
            .entries
            .iter()
            .any(|a| a.starts == starts && a.source == source)
        {
            if cache.entries.len() == 12 {
                cache.entries.pop_front();
            }
            cache.entries.push_back(Analysis {
                image: None,
                source: source.into(),
                starts: starts.into(),
                checked: checked.clone(),
            });
        }
    }
    Ok((checked, parse_ns))
}

pub(super) struct Image {
    pub heap: Heap,
    pub wasm: Vec<u8>,
    pub prefix: String,
    pub suffix: String,
    /// The library functions this image's build did not prove: the same source reaches the same.
    pub unreached: super::proof_reach::Unreached,
}
pub(super) fn receipt(prefix: &str, suffix: &str, obligations: &str, fresh: &str) -> String {
    format!("{prefix},\"obligations\":[{obligations}],\"proof_admission\":false{fresh}{suffix}")
}
impl Image {
    pub fn report(&self, proof: &super::proof::Checked) -> String {
        receipt(&self.prefix, &self.suffix, &proof.inventory, &proof.fresh)
    }
}
pub(super) fn active() -> bool {
    CACHE.with_borrow(|c| c.is_some())
}
pub(super) fn image(source: &str, starts: &[usize]) -> Option<Arc<Image>> {
    CACHE.with_borrow(|slot| {
        let mut c = slot.as_ref()?.lock().unwrap();
        let image = c
            .entries
            .iter()
            .rev()
            .find(|a| a.source == source && a.starts == starts)?
            .image
            .as_ref()?
            .clone();
        if image.heap != allocator::heap() {
            return None;
        }
        c.reused_images += 1;
        Some(image)
    })
}
pub(super) fn remember(source: &str, starts: &[usize], image: Image) {
    CACHE.with_borrow(|slot| {
        if let Some(shared) = slot
            && let Some(a) = shared
                .lock()
                .unwrap()
                .entries
                .iter_mut()
                .find(|a| a.source == source && a.starts == starts)
        {
            a.image = Some(Arc::new(image));
        }
    });
}

pub(super) fn binding() -> Result<Option<String>, String> {
    ready::Binding::configured()
        .map(|b| b.map(|v| v.identity))
        .map_err(|e| format!("{}: {}", e.code, e.message))
}
pub(super) fn optional_text(p: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}
struct Approval {
    proof_free: bool,
    input: PathBuf,
    snapshot: files::BuildInput,
    doc: PathBuf,
    grant: super::GrantDocumentInput,
    checked: Arc<CheckedSource>,
    checker: Option<String>,
    /// The memory sizes the review was asked for: the reviewed build is emitted with these.
    heap: Heap,
}
impl Approval {
    fn current(&self) -> Result<(), String> {
        if self.snapshot.policy != optional_text(&self.input.with_extension("policy"))?
            || self.checker != binding()?
            || super::read_grant_document(&self.doc).ok().as_ref() != Some(&self.grant)
        {
            return Err(
                "grant document, policy or proof toolchain changed after grant review".into(),
            );
        }
        Ok(())
    }
}
#[derive(Default)]
struct Session {
    approvals: Mutex<BTreeMap<String, Approval>>,
}
fn proof_error(e: Diagnostic) -> String {
    format!("{}: {}", e.code, e.message)
}
#[derive(Default)]
struct Reply {
    code: u32,
    stdout: String,
    stderr: String,
    ticket: String,
}
fn lines(values: Vec<String>) -> String {
    values.into_iter().map(|s| s + "\n").collect()
}
fn grant_error(errors: Vec<String>) -> String {
    errors
        .into_iter()
        .map(|s| format!("error: {s}"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn grant_reply((flags, review, warnings): (Vec<String>, Vec<String>, Vec<String>)) -> Reply {
    let stderr = review
        .into_iter()
        .map(|s| format!("grant: {s}\n"))
        .chain(warnings.into_iter().map(|s| format!("warning: {s}\n")))
        .collect();
    Reply {
        stdout: lines(flags),
        stderr,
        ..Reply::default()
    }
}

impl Session {
    // The ordinary build protocol includes checked STOP before publication.
    // A resident compiler keeps its typed inputs and code, never an unfinished
    // checker's authority over a later output.
    fn publish(
        build: impl FnOnce(
            Option<&mut proof_session::Session>,
        ) -> Result<(String, super::Measurements), String>,
    ) -> Result<(), String> {
        let mut checker = proof_session::Session::publishing().map_err(proof_error)?;
        match build(checker.as_mut()) {
            Ok((_, measurements)) => {
                PROOF_TIME.set((measurements.proof_check_ns, measurements.lean_invocations));
                Ok(())
            }
            Err(e) => {
                if let Some(checker) = checker.as_mut() {
                    checker.refuse();
                }
                Err(e)
            }
        }
    }
    fn dispatch(&self, args: &[String]) -> Result<Reply, String> {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let mut reply = Reply::default();
        match args.as_slice() {
            ["input-key", input] => {
                let input = Path::new(input);
                let (_, parts) = files::read_bundle(input)?;
                let policy = optional_text(&input.with_extension("policy"))?;
                reply.stdout = files::input_key(&parts, policy.as_deref(), binding()?.as_deref())?;
            }
            ["wasm-grants", input, doc, expected, document] => {
                let input = Path::new(input);
                let doc = Path::new(doc);
                let (source, parts) = files::read_bundle(input)?;
                let policy = optional_text(&input.with_extension("policy"))?;
                let grant = super::read_grant_document(doc)?;
                if files::input_key(&parts, policy.as_deref(), binding()?.as_deref())? != *expected
                    || grant.text != *document
                {
                    return Err("compiler inputs changed before checking grants".into());
                }
                let (checked, _) = analyze(&source, &files::starts(&parts))
                    .map_err(|e| grant_error(vec![files::render(&source, &parts, &e)]))?;
                return super::grants_checked(
                    &checked.syntax,
                    doc,
                    &grant.text,
                    &grant.base,
                )
                .map(grant_reply)
                .map_err(grant_error);
            }
            ["wasm-grants", input, doc] => {
                return super::grants_file(input.as_ref(), doc.as_ref())
                    .map(grant_reply)
                    .map_err(grant_error);
            }
            // `prepare-grants INPUT DOCUMENT TEXT [--document-base=DIR] [PAGES SLOTS STACK]`.
            // The base and memory sizes go with the approval used by `build-checked`.
            ["prepare-grants", input, doc, expected, options @ ..] => {
                let (base, sizes) = match options {
                    [flag, sizes @ ..] if let Some(base) = flag.strip_prefix("--document-base=") => (Some(Path::new(base)), sizes),
                    _ => (None, options),
                };
                let heap = match sizes {
                    [pages, slots, stack] => Heap {
                        pages: pages.parse().map_err(|_| "invalid heap pages")?,
                        slots: slots.parse().map_err(|_| "invalid slot count")?,
                        stack: stack.parse().map_err(|_| "invalid stack size")?,
                    }
                    .checked()?,
                    [] => Heap::DEFAULT,
                    _ => return Err("grant review requires all three memory sizes or none".into()),
                };
                let input = PathBuf::from(input);
                let doc = PathBuf::from(doc);
                let (source, parts) =
                    files::read_bundle(&input).map_err(|s| grant_error(vec![s]))?;
                let (checked, _) = analyze(&source, &files::starts(&parts))
                    .map_err(|e| grant_error(vec![files::render(&source, &parts, &e)]))?;
                let grant = super::read_grant_document(&doc).map_err(|e| format!("error: {e}"))?;
                if let Some(base) = base && base != grant.base {
                    return Err("grant document directory changed before review".into());
                }
                if grant.text != *expected {
                    return Err("grant document changed before review".into());
                }
                reply = grant_reply(
                    super::grants_checked(&checked.syntax, &doc, &grant.text, &grant.base)
                        .map_err(grant_error)?,
                );
                let policy = optional_text(&input.with_extension("policy"))?;
                let checker = binding()?;
                let proof_free = checked.proof_free();
                let mut approvals = self.approvals.lock().unwrap();
                if approvals.len() >= 64 {
                    return Err("too many outstanding grant reviews".into());
                }
                let mut random = [0u8; 16];
                fs::File::open("/dev/urandom")
                    .and_then(|mut f| f.read_exact(&mut random))
                    .map_err(|e| e.to_string())?;
                let ticket: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
                approvals.insert(
                    ticket.clone(),
                    Approval {
                        proof_free,
                        checked,
                        input,
                        snapshot: files::BuildInput {
                            source,
                            parts,
                            policy,
                        },
                        doc,
                        grant,
                        checker,
                        heap,
                    },
                );
                reply.ticket = ticket;
            }
            ["release", token] => {
                self.approvals.lock().unwrap().remove(*token);
            }
            ["build-checked", token, input, output] => {
                let approved = self
                    .approvals
                    .lock()
                    .unwrap()
                    .remove(*token)
                    .ok_or("grant ticket expired or already consumed")?;
                let start = Instant::now();
                if approved.input != Path::new(input) {
                    return Err("grant ticket names another input".into());
                }
                approved.current()?;
                CACHE.with_borrow(|slot| {
                    if let Some(shared) = slot {
                        let mut cache = shared.lock().unwrap();
                        cache.reviewed_builds += 1;
                        cache.proof_free_builds += u64::from(approved.proof_free);
                    }
                });
                Self::publish(|checker| {
                    allocator::with_heap(approved.heap, || {
                        files::build_loaded(
                            &approved.input,
                            output.as_ref(),
                            None,
                            checker,
                            true,
                            start,
                            &approved.snapshot,
                            Some(approved.checked.clone()),
                            &|| approved.current(),
                        )
                    })
                })?;
            }
            // A manifest-bound native executable (docs/native-trusted-executables.md): the
            // request names the compiler inputs' key and the manifest's digest, and both are
            // checked against the snapshot the build reads and again before publication.
            ["build-native", input, output, manifest, key, manifest_id] => {
                #[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
                {
                    let input = Path::new(input);
                    let selected = binding()?;
                    let current = |snapshot: &files::BuildInput| -> Result<(), String> {
                        if files::input_key(&snapshot.parts, snapshot.policy.as_deref(), selected.as_deref())? != *key {
                            return Err("compiler inputs changed before build".into());
                        }
                        if binding()? != selected {
                            return Err("proof toolchain changed during build".into());
                        }
                        Ok(())
                    };
                    let mut checker = proof_session::Session::publishing().map_err(proof_error)?;
                    let result = files::build_native_checked(input, output.as_ref(), checker.as_mut(), Path::new(manifest), manifest_id, &current);
                    if result.is_err() && let Some(checker) = checker.as_mut() {
                        checker.refuse();
                    }
                    let times = result?;
                    PROOF_TIME.set((times.proof_ns, 0));
                    reply.stdout = format!("{}\n", times.json());
                }
                #[cfg(not(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))))]
                {
                    let _ = (input, output, manifest, key, manifest_id);
                    return Err("W0401: a manifest-bound native build requires witchyc built for macos-arm64 or linux-arm64".into());
                }
            }
            ["build", input, output, pages, slots, stack, expected @ ..] if expected.len() <= 1 => {
                let heap = Heap {
                    pages: pages.parse().map_err(|_| "invalid heap pages")?,
                    slots: slots.parse().map_err(|_| "invalid slot count")?,
                    stack: stack.parse().map_err(|_| "invalid stack size")?,
                }
                .checked()?;
                let input = Path::new(input);
                Self::publish(|checker| {
                    allocator::with_heap(heap, || {
                        let start = Instant::now();
                        let (source, parts) = files::read_bundle(input)?;
                        let policy = optional_text(&input.with_extension("policy"))?;
                        let selected = checker
                            .as_deref()
                            .and_then(|s| s.cache_identity())
                            .map(str::to_owned);
                        if let [key] = expected
                            && files::input_key(&parts, policy.as_deref(), selected.as_deref())?
                                != *key
                        {
                            return Err("compiler inputs changed before build".into());
                        }
                        files::build_loaded(
                            input,
                            output.as_ref(),
                            None,
                            checker,
                            true,
                            start,
                            &files::BuildInput {
                                source,
                                parts,
                                policy,
                            },
                            None,
                            &|| {
                                if binding()? != selected {
                                    return Err("proof toolchain changed during build".into());
                                }
                                Ok(())
                            },
                        )
                    })
                })?;
            }
            ["wasm-check-package", input] => super::check_package(input.as_ref())?,
            ["wasm-caps", "--policy", policy, input] => {
                match super::caps_policy(policy.as_ref(), input.as_ref()) {
                    Ok(v) if v.is_empty() => reply.stdout = "fits the policy\n".into(),
                    Ok(v) => {
                        reply.code = 1;
                        reply.stdout = lines(v);
                    }
                    Err(e) => {
                        reply.code = 2;
                        reply.stderr = e + "\n";
                    }
                }
            }
            ["wasm-caps", "--repin", policy, input] => {
                match super::caps_repin(policy.as_ref(), input.as_ref()) {
                    Ok(v) if v.is_empty() => reply.stdout = "every digest is current\n".into(),
                    Ok(v) => {
                        reply.stdout =
                            lines(v.into_iter().map(|s| format!("repinned {s}")).collect())
                    }
                    Err(e) => {
                        reply.code = 2;
                        reply.stderr = e + "\n";
                    }
                }
            }
            ["wasm-caps", "--by-file", input, cwd] => {
                let (source, parts) = files::read_bundle(input.as_ref())?;
                let (logical, params) = super::checked_bundle(&source, &parts)?;
                reply.stdout = caps::by_file(&logical, params, &files::names(&parts, cwd.as_ref()));
            }
            ["wasm-caps", "--draft-policy", input] => {
                reply.stdout = super::caps_draft_policy(input.as_ref())?
            }
            ["wasm-caps", "--json", input] => reply.stdout = super::caps_json_file(input.as_ref())?,
            ["wasm-caps", input] => reply.stdout = super::caps_file(input.as_ref())?,
            _ => return Err("unknown compiler session request".into()),
        }
        Ok(reply)
    }
}

fn number(stream: &mut UnixStream) -> io::Result<u32> {
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}
fn request(s: &mut UnixStream) -> io::Result<Vec<String>> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let count = number(s)?;
    if !(1..=12).contains(&count) {
        return Err(io::Error::other("invalid request size"));
    }
    let mut args = Vec::new();
    for index in 0..count {
        let n = number(s)? as usize;
        let limit = if (index == 3 && args.first().is_some_and(|op| op == "prepare-grants"))
            || (index == 4 && args.first().is_some_and(|op| op == "wasm-grants"))
        {
            super::MAX_GRANT_DOCUMENT_BYTES
        } else {
            65536
        };
        if n > limit {
            return Err(io::Error::other("compiler request exceeds field limit"));
        }
        let mut bytes = vec![0; n];
        s.read_exact(&mut bytes)?;
        args.push(String::from_utf8(bytes).map_err(io::Error::other)?);
    }
    Ok(args)
}

fn exchange(s: &mut UnixStream, state: &Session, args: Vec<String>) -> io::Result<()> {
    let started = Instant::now();
    PROOF_TIME.set((0, 0));
    MESSAGES.with_borrow_mut(|m| *m = Some(String::new()));
    let _ = check::later_refusals();
    let r = state.dispatch(&args).unwrap_or_else(|e| Reply {
        code: 1,
        stderr: e + "\n",
        ..Reply::default()
    });
    let messages = MESSAGES.with_borrow_mut(Option::take).unwrap_or_default() + &r.stderr;
    let counters = CACHE.with_borrow(|c| {
        let c = c.as_ref().unwrap().lock().unwrap();
        format!(
            "{} {} {} {} {}",
            c.hits, c.misses, c.reused_images, c.reviewed_builds, c.proof_free_builds
        )
    });
    let (proof_ns, invocations) = PROOF_TIME.get();
    let counters = format!(
        "{counters} | {} {proof_ns} {invocations}",
        started.elapsed().as_nanos()
    );
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(&r.code.to_be_bytes())?;
    for value in [r.stdout, messages, r.ticket, counters] {
        s.write_all(&(value.len() as u32).to_be_bytes())?;
        s.write_all(value.as_bytes())?;
    }
    Ok(())
}

pub fn serve(path: &Path) -> io::Result<()> {
    // The owner creates a mode-0700 directory. Refuse a public listener even
    // when invoked directly, before accepting filesystem operations.
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "compiler socket requires a private directory",
        ));
    }
    let listener = UnixListener::bind(path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let socket = path.to_owned();
    std::thread::spawn(move || {
        let mut control = [0u8; 1];
        let _ = io::stdin().read(&mut control);
        done.store(true, Ordering::Release);
        let _ = UnixStream::connect(socket);
    });
    let cache = Arc::new(Mutex::new(Cache::default()));
    let state = Session::default();
    std::thread::scope(|scope| -> io::Result<()> {
        // Proof diagnostics and the bounded native checker belong to one build
        // lane. Independent authority queries may finish while that lane proves.
        let mut lanes = Vec::new();
        for name in ["compiler-build", "compiler-query"] {
            let (tx, rx) = mpsc::sync_channel::<(UnixStream, Vec<String>)>(64);
            lanes.push(tx);
            let cache = cache.clone();
            let stop = stop.clone();
            let state = &state;
            std::thread::Builder::new()
                .name(name.into())
                // The checker walks expressions by recursion: the same stack as the command line.
                .stack_size(super::parallel::STACK)
                .spawn_scoped(scope, move || {
                    CACHE.with_borrow_mut(|s| *s = Some(cache));
                    STOP.with_borrow_mut(|s| *s = Some(stop));
                    for (mut stream, args) in rx {
                        if cancelled() {
                            break;
                        }
                        let _ = exchange(&mut stream, state, args);
                    }
                })?;
        }
        println!("WITCHY_COMPILER_SESSION_1");
        io::stdout().flush()?;
        for stream in listener.incoming() {
            let mut stream = stream?;
            if stop.load(Ordering::Acquire) {
                break;
            }
            if let Ok(args) = request(&mut stream) {
                let index = match args[0].as_str() {
                    "build" | "build-native" => 0,
                    "build-checked" => usize::from(
                        args.get(1)
                            .and_then(|key| {
                                state
                                    .approvals
                                    .lock()
                                    .unwrap()
                                    .get(key)
                                    .map(|a| a.proof_free)
                            })
                            .unwrap_or(false),
                    ),
                    _ => 1,
                };
                if lanes[index].send((stream, args)).is_err() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    return Err(io::Error::other("compiler worker stopped"));
                }
            }
        }
        Ok(())
    })?;
    fs::remove_file(path)
}

/// The qualification runner starts a separate worker for each build mode.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn qualification_worker(root: &Path) -> io::Result<()> {
    super::native::configure_qualification(root)?;
    use std::io::BufRead;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Some((input, output)) = line.split_once('\t') else {
            return Err(io::Error::other("qualification request missing output"));
        };
        let _ = check::later_refusals();
        PROOF_TIME.set((0, 0));
        MESSAGES.with_borrow_mut(|messages| *messages = Some(String::new()));
        let result = super::build_native(input.as_ref(), output.as_ref(), super::native::TARGET, true, None);
        let mut messages = MESSAGES.with_borrow_mut(Option::take).unwrap_or_default();
        let status = if let Err(error) = result {
            messages.push_str(&error);
            messages.push('\n');
            1
        } else {
            0
        };
        let encoded: String = messages.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        println!("qualification-response\t{status}\t{encoded}");
        std::io::stdout().flush()?;
    }
    Ok(())
}
