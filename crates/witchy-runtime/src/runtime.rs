//! Witchy runtime spike.
//!
//! Each VM runs in its own wasmtime `Store` (its own linear memory, its own
//! stack). A VM can only reach the outside world through host functions
//! that the runtime explicitly links into *that VM's* `Linker`. Those host
//! functions ARE the capabilities: if a capability was not granted, the import
//! is simply absent and the VM fails to instantiate. There is no ambient
//! authority anywhere.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wasmtime::{
    bail, Cache, CacheConfig, Caller, Config, Engine, Error, Extern, Linker, Memory, Module,
    Result, Store, StoreLimits, StoreLimitsBuilder,
};

/// An on-disk Cranelift compilation cache so re-running the same program skips
/// recompiling its WAT (the ~3 ms compile cost). Keyed by wasm content +
/// wasmtime version, so it is transparent and self-invalidating. Best-effort:
/// returns `None` if a cache directory can't be set up.
fn compilation_cache() -> Option<Cache> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let mut cfg = CacheConfig::new();
    cfg.with_directory(base.join("witchy").join("wasm"));
    Cache::new(cfg).ok()
}

/// Directory for AOT-serialized compiled modules, keyed by a content hash of the
/// wasm. Distinct from the Cranelift compile cache above: `Module::deserialize`
/// loads an already-compiled artifact directly, skipping wasm parse/validate AND
/// the cache lookup — the cold-start lever from spec/performance.md Phase 3.
fn aot_cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("witchy").join("aot");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Build a `Module`. On the cacheable (non-preempt) engine, a warm AOT artifact
/// is loaded via `Module::deserialize` (skips wasm parse/validate/compile); a
/// cold compile is persisted for next time. `cacheable` is false on the preempt
/// engine, whose differing config must not share artifacts.
fn build_module(engine: &Engine, opt_wasm: &[u8], cacheable: bool) -> Result<Module> {
    if !cacheable {
        return Module::new(engine, opt_wasm);
    }
    let path = aot_cache_dir().map(|dir| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(opt_wasm);
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        dir.join(format!("{hex}.cwasm"))
    });
    if let Some(p) = &path {
        if let Ok(bytes) = std::fs::read(p) {
            // SAFETY: the artifact was produced by this binary's own
            // `Module::serialize` into a content-hash-named file under our cache
            // dir; `deserialize` additionally rejects version/config mismatches
            // (a wasmtime upgrade), in which case we fall through and recompile.
            if let Ok(m) = unsafe { Module::deserialize(engine, &bytes) } {
                return Ok(m);
            }
        }
    }
    let module = Module::new(engine, opt_wasm)?;
    if let Some(p) = &path {
        if let Ok(bytes) = module.serialize() {
            // Write-then-rename so a reader never sees a partial file. The temp
            // name carries a random suffix so two threads/processes compiling the
            // same program don't race on one path (atomic rename publishes it).
            let mut rnd = [0u8; 8];
            getrandom::fill(&mut rnd).ok();
            let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
            let tmp = p.with_extension(format!("{suffix}.tmp"));
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, p);
            }
        }
    }
    Ok(module)
}

pub type VmId = u32;

/// The set of capabilities granted to a VM at spawn time. Each `true` flag
/// causes the corresponding host function to be linked into the VM.
/// Everything defaults to denied.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// May write to the host's stdout via `witchy.print`.
    pub print: bool,
    /// May print an integer via `witchy.print_int` (used by compiled witchy).
    pub print_int: bool,
    /// Capture output without echoing it to stdout (used by `witchy parity`,
    /// which compares the captured lines rather than showing them).
    pub quiet: bool,
    /// May read the wall clock via `witchy.now` (a `Clock` capability).
    pub clock: bool,
    /// May read process environment variables via `witchy.env_*` (an `Env`
    /// capability).
    pub env: bool,
    /// The directory subtree backing the root `Dir` capability (handle 0).
    /// `None` denies the filesystem entirely; the `dir_read`/`dir_write` flags
    /// pick which operation families are linked within it.
    pub dir_root: Option<std::path::PathBuf>,
    /// Additional `Dir` grants beyond handle 0, for a `main` that takes several
    /// `Dir` parameters (positional: the i-th `Dir` param maps to handle `i`).
    /// Handle 0 is `dir_root`; these back handles 1.. in order. They share the
    /// `dir_read`/`dir_write` rights of the grant. See rfcs/0004-self-hosted-cli.md.
    pub dir_roots: Vec<std::path::PathBuf>,
    /// Direct `File` grants (RFC-0012): the i-th `File` parameter of `main` maps to
    /// the i-th path here (handle `i` in the files table). Read/write access is the
    /// param's compile-time right, so these are plain paths. Backed by `--file`.
    pub file_grants: Vec<std::path::PathBuf>,
    /// May read within `dir_root` (read/exists/is_dir/list/subdir).
    pub dir_read: bool,
    /// May write within `dir_root` (write/make_dir).
    pub dir_write: bool,
    /// May spawn a native subprocess via `exec` (an `Exec` capability). The
    /// executable is named + confined through a `Dir[Read]` handle, so `exec`
    /// without filesystem read is useless — but the flag is tracked separately so
    /// `Exec` appears as its own authority. See rfcs/0004-self-hosted-cli.md.
    pub exec: bool,
    /// The `host:port` allowlist backing the root `Net` capability (handle 0).
    /// `None` denies the network entirely; the verb flags below pick which
    /// operation families are linked within it.
    pub net_allow: Option<Vec<String>>,
    /// May dial out (`connect`/`restrict`) to allowlisted addresses.
    pub net_connect: bool,
    /// May bind and accept (`listen`/`accept`) on allowlisted addresses.
    pub net_listen: bool,
    /// The program's command-line arguments (`main(args: List(String))`).
    /// Pure input chosen by the host, not authority.
    pub args: Vec<String>,
    /// The Ed25519 seed backing the root `Secret` capability. The key
    /// material stays host-side; the guest only ever sees signatures and the
    /// public key.
    pub signing_key: Option<[u8; 32]>,
    /// Named secrets backing the `SecretStore` capability, in handle order — a
    /// `Secret` value is an index into this table, so the raw bytes stay host-side
    /// (the guest only ever holds the handle). Index 0 is the `signing` secret
    /// (from `--signing-key`), so a bare `Secret` capability is handle 0.
    pub secrets: Vec<(String, Vec<u8>)>,
    /// The confined output directory backing a build step's `BuildOut`
    /// capability — where `write_out` may write generated source, and nowhere
    /// else. `None` denies build-time writes entirely.
    pub build_out: Option<std::path::PathBuf>,
    /// The confined read roots backing a build step's `BuildRead` capability —
    /// `read_build` resolves a path against the first root that holds it. Empty
    /// denies build-time reads.
    pub build_read_roots: Vec<std::path::PathBuf>,
}

impl Capabilities {
    /// No authority at all.
    pub fn none() -> Self {
        Self::default()
    }
}

/// Host-side state owned by a spawned VM's `Store`: its capability grant, output
/// buffer, and the host-side `Dir`/`Net`/build handle tables. One state type
/// means one set of capability host functions.
pub struct VmState {
    id: VmId,
    caps: Capabilities,
    pub(crate) limits: StoreLimits,
    /// Everything the VM has printed (via the `print`/`print_int`
    /// capabilities), so the host can observe a compiled program's output.
    pub(crate) output: Arc<Mutex<Vec<String>>>,
    /// The `Dir` capability handle table: index 0 is the granted root, and each
    /// `subdir` mints a new confined entry. Guest code only ever holds the i32
    /// index — the paths live host-side, so a module cannot forge a directory.
    pub(crate) dirs: Vec<std::path::PathBuf>,
    /// The `File` capability handle table (RFC-0012): `dir.open`/`dir.create` mint
    /// a confined entry; guest code holds only the i32 index, so a file path can no
    /// more be forged than a directory.
    pub(crate) files: Vec<std::path::PathBuf>,
    /// A host->guest transfer staged by a size-probing call (`dir_read_len`,
    /// `net_recv_*_len`, ...) and consumed by the matching `fill_pending`, so
    /// the data is read once with no time-of-check/time-of-use gap.
    pub(crate) pending: Option<Vec<u8>>,
    /// A staged directory listing (`dir_list_size` -> `dir_list_write`).
    pub(crate) pending_list: Option<Vec<String>>,
    /// The `Net` capability handle table: index 0 is the granted allowlist,
    /// and each `restrict` mints a narrower entry — host-side, unforgeable.
    pub(crate) nets: Vec<Vec<String>>,
    /// Open sockets, indexed by the guest's i32 Socket handles. Each is a plain or
    /// TLS byte stream behind one `dyn Stream` (see `Stream`).
    sockets: Vec<std::io::BufReader<Box<dyn Stream>>>,
    /// Listening server sockets, indexed by the guest's i32 Listener handles.
    listeners: Vec<std::net::TcpListener>,
    /// A build step's confined output directory (`BuildOut`) and read roots
    /// (`BuildRead`) — host-side, so a guest holds only an opaque handle.
    build_out: Option<std::path::PathBuf>,
    build_read_roots: Vec<std::path::PathBuf>,
}

/// A spawned VM plus the entrypoint we can drive.
pub struct Vm {
    store: Store<VmState>,
    instance: wasmtime::Instance,
}

impl Vm {
    /// Call the VM's exported `run` function to completion.
    pub fn run(&mut self) -> Result<()> {
        let run = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, "run")?;
        run.call(&mut self.store, ())?;
        if std::env::var_os("WITCHY_REGION_STATS").is_some_and(|v| v == "1") {
            if let Some(bytes) = self.region_copy_bytes() {
                eprintln!("region copy-out: {bytes} byte(s)");
            }
        }
        Ok(())
    }

    /// The total bytes the `region:` copy-outs moved (the exported
    /// `__region_copy_bytes` counter), or None when the module has no regions.
    /// The exported re-own counter: how many in-place accumulation sites
    /// entered with a zero ownership token (each one copies). None when the
    /// module has no in-place machinery. (A test/diagnostic API.)
    #[allow(dead_code)]
    pub fn reowns(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__witchy_reowns")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    pub fn region_copy_bytes(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__region_copy_bytes")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// Everything this VM has printed so far, in order. (Used by tests to
    /// assert a compiled program's behavior end to end.)
    #[allow(dead_code)]
    pub fn output(&self) -> Vec<String> {
        self.store.data().output.lock().unwrap().clone()
    }
}

/// The runtime owns the wasm engine and the shared mailbox registry, and hands
/// out VM ids.
pub struct Runtime {
    engine: Engine,
    next_id: VmId,
    preempt: bool,
}

impl Runtime {
    /// A runtime whose VMs can be preempted by the scheduler advancing the
    /// engine epoch. Epoch interruption makes the JIT insert a check at
    /// every loop backedge and call, so for run-to-completion single-program
    /// execution prefer [`Runtime::batch`], which omits that per-iteration cost.
    pub fn new() -> Result<Self> {
        Self::with_preemption(true)
    }

    /// A runtime for run-to-completion execution — the `sandbox`/benchmark path
    /// and differential WASM runs. No epoch interruption, so the generated code
    /// runs without per-backedge preemption checks (a measurable speedup on
    /// tight loops and recursion). There is no scheduler to preempt it; the
    /// capability sandbox (only granted host fns, capped linear memory) is still
    /// fully in force, so this is a speed choice, not a security relaxation.
    pub fn batch() -> Result<Self> {
        Self::with_preemption(false)
    }

    fn with_preemption(preempt: bool) -> Result<Self> {
        let mut config = Config::new();
        // Cranelift's Speed tier: the compile-time cost is amortized by the
        // compilation cache below, so generated code quality is free on every
        // run after the first.
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        // Epoch-based interruption lets the scheduler preempt a runaway VM.
        // It is only worth its per-backedge cost when a
        // scheduler will actually advance the epoch.
        if preempt {
            config.epoch_interruption(true);
        } else if let Some(cache) = compilation_cache() {
            // The batch path re-runs the same program across invocations (a CLI
            // re-run, a benchmark loop); caching the compile makes the second
            // run onward skip Cranelift.
            config.cache(Some(cache));
        }
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            next_id: 1,
            preempt,
        })
    }

    /// Spawn a VM from WAT/wasm source, granting it exactly `caps` and
    /// capping its linear memory at `memory_pages_max` 64KiB pages.
    ///
    /// The capability check is structural: a granted host function is linked,
    /// an ungranted one is absent, so a VM that imports an ungranted
    /// capability fails right here at `instantiate`.
    pub fn spawn(
        &mut self,
        wasm: impl AsRef<[u8]>,
        caps: Capabilities,
        memory_pages_max: usize,
    ) -> Result<Vm> {
        let id = self.next_id;
        let module = build_module(&self.engine, &optimize_module(wasm.as_ref()), !self.preempt)?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let dirs = caps
            .dir_root
            .iter()
            .cloned()
            .chain(caps.dir_roots.iter().cloned())
            .collect();
        let nets = caps.net_allow.iter().cloned().collect();
        let state = VmState {
            id,
            caps: caps.clone(),
            limits,
            output: Arc::new(Mutex::new(Vec::new())),
            dirs,
            files: caps.file_grants.clone(),
            pending: None,
            pending_list: None,
            nets,
            sockets: Vec::new(),
            listeners: Vec::new(),
            build_out: caps.build_out.clone(),
            build_read_roots: caps.build_read_roots.clone(),
        };

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        // Deadline of 1 epoch; we never advance the engine epoch during a
        // normal run, so this is never tripped. The scheduler advances it to preempt.
        // Only meaningful when the engine has epoch interruption enabled.
        if self.preempt {
            store.set_epoch_deadline(1);
        }

        let mut linker: Linker<VmState> = Linker::new(&self.engine);
        link_capability_imports(&mut linker, &caps)?;

        // Ungranted capability imports are rejected here.
        let instance = linker.instantiate(&mut store, &module)?;

        // Only commit the id once the VM actually came up.
        self.next_id += 1;
        Ok(Vm { store, instance })
    }

    /// Run a VM, but preempt it if it runs longer than `budget`. A watchdog
    /// advances the engine epoch once the budget elapses; the VM traps at
    /// its next loop back-edge or call. This is how the scheduler reclaims a
    /// runaway or malicious VM that refuses to yield.
    pub fn run_with_budget(&self, vm: &mut Vm, budget: Duration) -> Result<()> {
        let engine = self.engine.clone();
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(budget);
            engine.increment_epoch();
        });
        let result = vm.run();
        watchdog.join().ok();
        result
    }
}

// ---------------------------------------------------------------------------
// Host functions = capabilities. Each reads/writes the *calling* VM's own
// linear memory via `Caller`; none can touch another VM's memory.
// ---------------------------------------------------------------------------

/// Register the capability host imports a grant entitles a VM to. Only granted
/// families are defined; a module compiled against an ungranted operation cannot
/// even instantiate.
pub(crate) fn link_capability_imports(
    linker: &mut Linker<VmState>,
    caps: &Capabilities,
) -> Result<()> {
    // --- capability wiring: only granted host functions are defined ---
    if caps.print {
        linker.func_wrap("witchy", "print", host_print)?;
    }
    if caps.print_int {
        linker.func_wrap("witchy", "print_int", host_print_int)?;
        // Same "print a computed result" facility, for a Float-returning main.
        linker.func_wrap("witchy", "print_float", host_print_float)?;
    }
    if caps.clock {
        linker.func_wrap("witchy", "now", host_now)?;
    }
    if caps.env {
        linker.func_wrap("witchy", "env_len", host_env_len)?;
        linker.func_wrap("witchy", "env_fill", host_env_fill)?;
    }
    // The Dir family is linked per RIGHT, so a module compiled against a
    // write operation cannot even instantiate under a read-only grant.
    if caps.dir_root.is_some() && caps.dir_read {
        linker.func_wrap("witchy", "dir_subdir", host_dir_subdir)?;
        linker.func_wrap("witchy", "dir_read_len", host_dir_read_len)?;
        linker.func_wrap("witchy", "dir_exists", host_dir_exists)?;
        linker.func_wrap("witchy", "dir_is_dir", host_dir_is_dir)?;
        linker.func_wrap("witchy", "dir_list_size", host_dir_list_size)?;
        // RFC-0012: `dir.open` navigates a readable Dir to a File handle.
        linker.func_wrap("witchy", "dir_open", host_dir_open)?;
    }
    if caps.dir_root.is_some() && caps.dir_write {
        linker.func_wrap("witchy", "dir_write", host_dir_write)?;
        linker.func_wrap("witchy", "dir_append", host_dir_append)?;
        linker.func_wrap("witchy", "dir_make_dir", host_dir_make_dir)?;
        // RFC-0012: `dir.create` navigates a writable Dir to a File handle.
        linker.func_wrap("witchy", "dir_create", host_dir_create)?;
    }
    // RFC-0012 File leaf ops: usable on a File obtained by navigation (above) OR
    // granted directly to `main` (`--file`), so they are linked whenever either a
    // Dir grant or a direct File grant is present.
    let has_file_grants = !caps.file_grants.is_empty();
    if (caps.dir_root.is_some() && caps.dir_read) || has_file_grants {
        linker.func_wrap("witchy", "file_read_len", host_file_read_len)?;
    }
    if (caps.dir_root.is_some() && caps.dir_write) || has_file_grants {
        linker.func_wrap("witchy", "file_write", host_file_write)?;
    }
    // The Exec capability: spawn a confined subprocess. The executable is named
    // through a `Dir[Read]` handle, so `exec_run` reuses `dir_base`/`confine`.
    if caps.exec {
        linker.func_wrap("witchy", "exec_run", host_exec_run)?;
    }
    // The Net family, linked per VERB right. Socket I/O carries no authority
    // of its own (a socket is only obtainable through a granted connect or
    // accept), so it is linked under either verb.
    let net = caps.net_allow.is_some();
    if net && caps.net_connect {
        linker.func_wrap("witchy", "net_restrict", host_net_restrict)?;
        linker.func_wrap("witchy", "net_deny", host_net_deny)?;
        linker.func_wrap("witchy", "net_connect", host_net_connect)?;
        linker.func_wrap("witchy", "net_try_connect", host_net_try_connect)?;
    }
    if net && caps.net_listen {
        linker.func_wrap("witchy", "net_listen", host_net_listen)?;
        linker.func_wrap("witchy", "net_accept", host_net_accept)?;
    }
    if net && (caps.net_connect || caps.net_listen) {
        linker.func_wrap("witchy", "net_send_line", host_net_send_line)?;
        linker.func_wrap("witchy", "net_send_bytes", host_net_send_bytes)?;
        linker.func_wrap("witchy", "net_recv_line_len", host_net_recv_line_len)?;
        linker.func_wrap("witchy", "net_recv_all_len", host_net_recv_all_len)?;
        linker.func_wrap("witchy", "net_recv_bytes_len", host_net_recv_bytes_len)?;
        linker.func_wrap("witchy", "net_close", host_net_close)?;
    }
    // The Secret capability: the seed never enters guest memory — the
    // host signs and reports the public key on the guest's behalf.
    if caps.signing_key.is_some() {
        linker.func_wrap("witchy", "crypto.sign", host_crypto_sign)?;
        linker.func_wrap("witchy", "crypto.public_key", host_crypto_public_key)?;
    }
    // The build-time capabilities. Like Dir, each is linked only when granted
    // — a build step compiled against `write_out` cannot even instantiate
    // without a `BuildOut` grant, and `read_build` without a `BuildRead` one.
    if caps.build_out.is_some() {
        linker.func_wrap("witchy", "build_out_write", host_build_out_write)?;
    }
    if !caps.build_read_roots.is_empty() {
        // `read_build` is staged like `dir_read`; `fill_pending` (linked
        // unconditionally below) does the transfer.
        linker.func_wrap("witchy", "build_read_len", host_build_read_len)?;
    }
    // `fill_pending` / `write_pending_list` only write out data already
    // staged by a granted size call — no authority of their own, so they are
    // always available. `args_size` stages the host-chosen argv (pure
    // input, not authority), so it is always available too.
    linker.func_wrap("witchy", "fill_pending", host_fill_pending)?;
    linker.func_wrap("witchy", "write_pending_list", host_write_pending_list)?;
    linker.func_wrap("witchy", "args_size", host_args_size)?;
    // Field-length staging helpers (`[len]` of a host cell's string/list field).
    // They carry no authority — pure reads — and the WIR static prelude declares
    // them unconditionally, so define harmless stubs here. Ordinary programs
    // never call them, so the body is unreachable; returning 0 is a safe default.
    linker.func_wrap("witchy", "field_str_len", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    linker.func_wrap("witchy", "field_intlist_len", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    linker.func_wrap("witchy", "field_strlist_size", |_: Caller<'_, VmState>, _: i32| -> i32 { 0 })?;
    // Native-stdlib functions are pure (no authority), so they're always
    // available — the same `crypto` module the interpreter exposes, here as a
    // host import that bridges to the shared `native` registry.
    linker.func_wrap("witchy", "crypto.ed25519_verify", host_ed25519_verify)?;
    linker.func_wrap("witchy", "crypto.sha256", host_sha256)?;
    linker.func_wrap("witchy", "crypto.ecdsa_p256_verify", host_ecdsa_p256_verify)?;
    linker.func_wrap("witchy", "crypto.ecdsa_p256_verify_hex", host_ecdsa_p256_verify_hex)?;
    linker.func_wrap("witchy", "crypto.rsa_pkcs1_sha256_verify", host_rsa_pkcs1_sha256_verify)?;
    linker.func_wrap("witchy", "crypto.sha512", host_sha512)?;
    linker.func_wrap("witchy", "crypto.sha3_256", host_sha3_256)?;
    linker.func_wrap("witchy", "crypto.hmac_sha256", host_hmac_sha256)?;
    linker.func_wrap("witchy", "crypto.rune_hash", host_crypto_rune_hash)?;
    // Resolving a named secret to its host-table handle reveals nothing (an
    // i32 index, or -1 when absent), so it is always linkable; an ungranted
    // store simply yields -1 for every name.
    linker.func_wrap("witchy", "secretstore_lookup", host_secretstore_lookup)?;
    // Revealing a value secret needs a granted secret at the handle (it errors
    // otherwise), so it is always linkable — like `secretstore_lookup`.
    linker.func_wrap("witchy", "crypto_reveal_len", host_crypto_reveal_len)?;
    // The compiler's footprint analyses are pure functions of their source
    // arguments — the toolchain exposed to witchy, same registry bridge.
    linker.func_wrap("witchy", "compiler_footprint_len", host_compiler_footprint_len)?;
    linker.func_wrap("witchy", "compiler_diff_len", host_compiler_diff_len)?;
    linker.func_wrap("witchy", "compiler_doc_len", host_compiler_doc_len)?;
    linker.func_wrap("witchy", "regex_match_spans_len", host_regex_match_spans_len)?;
    // Float -> string formatting is pure; done in the host so it is byte-
    // identical to the interpreter's `Display` (no float formatter in WAT).
    linker.func_wrap("witchy", "float_to_str", host_float_to_str)?;
    linker.func_wrap("witchy", "string_from_code", host_string_from_code)?;
    // hex/base64 transforms are pure; bridged to the same native registry the
    // interpreter uses (byte-for-byte parity, no byte-level work in WAT).
    linker.func_wrap("witchy", "encoding", host_encoding)?;
    Ok(())
}

fn host_print(mut caller: Caller<'_, VmState>, ptr: i32, len: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let bytes = slice(data, ptr, len)?;
    let text = String::from_utf8_lossy(bytes);
    let id = caller.data().id;
    if !caller.data().caps.quiet {
        print!("[vm {id}] {text}");
    }
    caller
        .data()
        .output
        .lock()
        .unwrap()
        .push(text.trim_end_matches('\n').to_string());
    Ok(())
}

fn host_print_int(caller: Caller<'_, VmState>, n: i64) -> Result<()> {
    if !caller.data().caps.quiet {
        println!("[vm {}] {n}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(n.to_string());
    Ok(())
}

fn host_print_float(caller: Caller<'_, VmState>, x: f64) -> Result<()> {
    // The same canonical float formatting the interpreter uses (Value::Float
    // Display via `render_float`), so the two backends agree on float output —
    // including the trailing `.0` on a whole-valued float.
    let s = witchy_syntax::fmt::render_float(x);
    if !caller.data().caps.quiet {
        println!("[vm {}] {s}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(s);
    Ok(())
}

/// `crypto.ed25519_verify(pk, msg, sig) -> Bool`, bridged to the shared native
/// registry (the same implementation the interpreter uses). Each argument is a
/// pointer to a witchy string header (`[i32 len][bytes]`).
fn host_ed25519_verify(
    mut caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args = [
        Value::Str(read_wstr(data, pk)?),
        Value::Str(read_wstr(data, msg)?),
        Value::Str(read_wstr(data, sig)?),
    ];
    let f = crate::native::lookup("crypto.ed25519_verify")
        .ok_or_else(|| Error::msg("crypto.ed25519_verify is not registered"))?;
    match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Bool(b) => Ok(b as i32),
        _ => Err(Error::msg("crypto.ed25519_verify did not return a Bool")),
    }
}

/// `crypto.sha256(in_header_ptr, out_data_ptr)`: read the input string, compute
/// its SHA-256 (via the shared native registry), and write the 64 hex bytes into
/// guest memory at `out_data_ptr` (the guest pre-allocated the result string).
/// Format an f64 with Rust's `Display` (matching the interpreter's float
/// `to_string`), write the bytes at `out_ptr`, and return the byte length. The
/// guest reserves a generous buffer; an f64's decimal form never exceeds it.
fn host_float_to_str(mut caller: Caller<'_, VmState>, x: f64, out_ptr: i32) -> Result<i32> {
    let s = witchy_syntax::fmt::render_float(x);
    let bytes = s.into_bytes();
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing float string into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

/// `string_from_code(codepoint, out_ptr) -> byte length`: encode a Unicode
/// scalar value as UTF-8 into the guest buffer, via the shared native registry
/// (the SAME `char::from_u32` the interpreter uses). An out-of-range or
/// surrogate value becomes U+FFFD, never an error.
fn host_string_from_code(mut caller: Caller<'_, VmState>, cp: i64, out_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let f = crate::native::lookup("string.from_code")
        .ok_or_else(|| Error::msg("string.from_code is not registered"))?;
    let s = match f(&[Value::Int(cp)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("string.from_code did not return a String")),
    };
    let bytes = s.into_bytes();
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing string.from_code result into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

fn host_sha256(mut caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let input = read_wstr(mem.data(&caller), in_ptr)?;
    let f = crate::native::lookup("crypto.sha256")
        .ok_or_else(|| Error::msg("crypto.sha256 is not registered"))?;
    let hex = match f(&[Value::Str(input)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.sha256 did not return a String")),
    };
    if hex.len() != 64 {
        return Err(Error::msg("crypto.sha256 hex digest is not 64 bytes"));
    }
    mem.write(&mut caller, out_ptr as usize, hex.as_bytes())
        .map_err(|e| Error::msg(format!("writing sha256 result into guest memory: {e}")))
}

/// Shared bridge for the three-string crypto verifies (`ecdsa_p256_verify` and
/// its hex variant): read three string headers, dispatch through the shared
/// native registry (the SAME aws-lc-rs impl the interpreter uses), return an
/// i32 bool. Parameterized by the native name so each verify is a thin wrapper.
fn host_crypto_verify3(
    mut caller: Caller<'_, VmState>,
    native: &str,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args = [
        Value::Str(read_wstr(data, pk)?),
        Value::Str(read_wstr(data, msg)?),
        Value::Str(read_wstr(data, sig)?),
    ];
    let f = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Bool(b) => Ok(b as i32),
        _ => Err(Error::msg(format!("{native} did not return a Bool"))),
    }
}

/// Shared bridge for the fixed-width hex digests (`sha512`, `sha3_256`,
/// `hmac_sha256`): read one or two input string headers, compute via the shared
/// native registry, and write the `expect`-length hex digest into the guest's
/// pre-allocated buffer at `out_ptr`.
fn host_crypto_digest(
    mut caller: Caller<'_, VmState>,
    native: &str,
    inputs: &[i32],
    out_ptr: i32,
    expect: usize,
) -> Result<()> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args: Vec<Value> = inputs
        .iter()
        .map(|&p| read_wstr(data, p).map(Value::Str))
        .collect::<Result<_>>()?;
    let f = crate::native::lookup(native)
        .ok_or_else(|| Error::msg(format!("{native} is not registered")))?;
    let hex = match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg(format!("{native} did not return a String"))),
    };
    if hex.len() != expect {
        return Err(Error::msg(format!("{native} hex digest is not {expect} bytes")));
    }
    mem.write(&mut caller, out_ptr as usize, hex.as_bytes())
        .map_err(|e| Error::msg(format!("writing {native} result into guest memory: {e}")))
}

fn host_ecdsa_p256_verify(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i32> {
    host_crypto_verify3(caller, "crypto.ecdsa_p256_verify", pk, msg, sig)
}

fn host_ecdsa_p256_verify_hex(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i32> {
    host_crypto_verify3(caller, "crypto.ecdsa_p256_verify_hex", pk, msg, sig)
}

fn host_rsa_pkcs1_sha256_verify(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i32> {
    host_crypto_verify3(caller, "crypto.rsa_pkcs1_sha256_verify", pk, msg, sig)
}

fn host_sha512(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    host_crypto_digest(caller, "crypto.sha512", &[in_ptr], out_ptr, 128)
}

fn host_sha3_256(caller: Caller<'_, VmState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    host_crypto_digest(caller, "crypto.sha3_256", &[in_ptr], out_ptr, 64)
}

fn host_hmac_sha256(caller: Caller<'_, VmState>, key_ptr: i32, msg_ptr: i32, out_ptr: i32) -> Result<()> {
    host_crypto_digest(caller, "crypto.hmac_sha256", &[key_ptr, msg_ptr], out_ptr, 64)
}

/// `crypto.rune_hash(paths_ptr, contents_ptr, out_data_ptr)`: read both guest
/// string lists, compute the store's content hash through the shared native
/// registry, and write the fixed 71-byte `sha256:<hex>` result into the
/// guest-allocated string.
fn host_crypto_rune_hash(
    mut caller: Caller<'_, VmState>,
    paths_ptr: i32,
    contents_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let paths = read_wstr_list(mem.data(&caller), paths_ptr)?;
    let contents = read_wstr_list(mem.data(&caller), contents_ptr)?;
    let to_list = |xs: Vec<String>| Value::List(xs.into_iter().map(Value::Str).collect());
    let f = crate::native::lookup("crypto.rune_hash")
        .ok_or_else(|| Error::msg("crypto.rune_hash is not registered"))?;
    let hash = match f(&[to_list(paths), to_list(contents)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.rune_hash did not return a String")),
    };
    if hash.len() != 71 {
        return Err(Error::msg("crypto.rune_hash result is not 71 bytes"));
    }
    mem.write(&mut caller, out_ptr as usize, hash.as_bytes())
        .map_err(|e| Error::msg(format!("writing rune_hash result into guest memory: {e}")))
}

/// `compiler_footprint_len(src_ptr) -> Int`: compute the capability-footprint
/// JSON of the guest's source string through the shared native registry, stage
/// it for `fill_pending`, and report its byte length.
fn host_compiler_footprint_len(mut caller: Caller<'_, VmState>, src_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup("compiler.footprint")
        .ok_or_else(|| Error::msg("compiler.footprint is not registered"))?;
    let json = match f(&[Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.footprint did not return a String")),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_diff_len(old_ptr, new_ptr) -> Int`: compute the footprint-diff
/// JSON of two guest source strings, stage it for `fill_pending`, and report
/// its byte length.
fn host_compiler_diff_len(
    mut caller: Caller<'_, VmState>,
    old_ptr: i32,
    new_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let old_src = read_wstr(mem.data(&caller), old_ptr)?;
    let new_src = read_wstr(mem.data(&caller), new_ptr)?;
    let f = crate::native::lookup("compiler.diff")
        .ok_or_else(|| Error::msg("compiler.diff is not registered"))?;
    let json = match f(&[Value::Str(old_src), Value::Str(new_src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.diff did not return a String")),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_doc_len(name_ptr, src_ptr) -> Int`: render the guest's source string to
/// Markdown API docs (the `compiler.doc` native — `witchy doc` output) under heading
/// `name`, stage it for `fill_pending`, and report its byte length.
fn host_compiler_doc_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup("compiler.doc")
        .ok_or_else(|| Error::msg("compiler.doc is not registered"))?;
    let md = match f(&[Value::Str(name), Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.doc did not return a String")),
    };
    let len = md.len() as i32;
    caller.data_mut().pending = Some(md.into_bytes());
    Ok(len)
}

/// `regex_match_spans_len(pat_ptr, text_ptr) -> Int`: run the regex crate (the
/// same `regex.match_spans` native the interpreter uses) over two guest strings,
/// stage the encoded match spans for `fill_pending`, and report their byte
/// length. An invalid pattern traps — identical to the interpreter's error.
fn host_regex_match_spans_len(
    mut caller: Caller<'_, VmState>,
    pat_ptr: i32,
    text_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let pattern = read_wstr(mem.data(&caller), pat_ptr)?;
    let text = read_wstr(mem.data(&caller), text_ptr)?;
    let f = crate::native::lookup("regex.match_spans")
        .ok_or_else(|| Error::msg("regex.match_spans is not registered"))?;
    let spans = match f(&[Value::Str(pattern), Value::Str(text)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("regex.match_spans did not return a String")),
    };
    let len = spans.len() as i32;
    caller.data_mut().pending = Some(spans.into_bytes());
    Ok(len)
}

/// `encoding.*(op, in_header_ptr, out_data_ptr) -> byte length`: read the input
/// string, run the selected hex/base64 transform through the shared native
/// registry (the same implementation the interpreter uses, so the backends agree
/// byte-for-byte), write the result bytes at `out_data_ptr`, and return their
/// length. The guest reserves a sufficient buffer (`2*len + slack`) beforehand.
/// `op`: 0 = hex_encode, 1 = hex_decode, 2 = base64_encode, 3 = base64_decode.
fn host_encoding(mut caller: Caller<'_, VmState>, op: i32, in_ptr: i32, out_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let name = match op {
        0 => "encoding.hex_encode",
        1 => "encoding.hex_decode",
        2 => "encoding.base64_encode",
        3 => "encoding.base64_decode",
        4 => "encoding.base64url_of_hex",
        5 => "encoding.base64url_decode",
        6 => "encoding.base64url_to_hex",
        _ => return Err(Error::msg(format!("unknown encoding op {op}"))),
    };
    let mem = memory_of(&mut caller)?;
    let input = read_wstr(mem.data(&caller), in_ptr)?;
    let f = crate::native::lookup(name)
        .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
    let out = match f(&[Value::Str(input)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg(format!("{name} did not return a String"))),
    };
    let bytes = out.as_bytes();
    mem.write(&mut caller, out_ptr as usize, bytes)
        .map_err(|e| Error::msg(format!("writing {name} result into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

/// `now() -> Int`: wall-clock milliseconds since the Unix epoch — the same value
/// the interpreter's `now(Clock)` produces. Linked only when the VM was
/// granted a `Clock` capability.
fn host_now(_caller: Caller<'_, VmState>) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `env_len(name_ptr) -> Int`: the byte length of the named environment
/// variable's value, or -1 when unset (or not valid Unicode — matching the
/// interpreter's `std::env::var`, which errors on both). The guest sizes its
/// buffer from this, then calls `env_fill`. Linked only under an `Env` grant.
fn host_env_len(mut caller: Caller<'_, VmState>, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    match std::env::var(&name) {
        Ok(v) => Ok(v.len() as i32),
        Err(_) => Ok(-1),
    }
}

/// `env_fill(name_ptr, out_ptr)`: write the named environment variable's value
/// bytes into guest memory at `out_ptr` (the guest pre-allocated `env_len`
/// bytes). Linked only under an `Env` grant.
fn host_env_fill(mut caller: Caller<'_, VmState>, name_ptr: i32, out_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let value = std::env::var(&name).unwrap_or_default();
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| Error::msg(format!("writing env value into guest memory: {e}")))
}

// --- the Dir capability family ---
//
// A guest `Dir` value is an i32 HANDLE into the VM's host-side path table
// (`VmState::dirs`); the paths never enter guest memory, so a module cannot
// forge or widen one. Handle 0 is the granted root. Every operation resolves
// through the SAME `resolve`/`resolve_write` confinement the interpreter uses
// (lexical `..`/absolute rejection + symlink-aware canonicalization), so the
// two backends agree on exactly which paths are reachable.

/// Look up a Dir handle's base path (trap on a forged/out-of-range handle).
fn dir_base(caller: &Caller<'_, VmState>, h: i32) -> Result<std::path::PathBuf> {
    caller
        .data()
        .dirs
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Dir handle {h}")))
}

fn confine(r: std::result::Result<std::path::PathBuf, crate::confine::ConfineError>) -> Result<std::path::PathBuf> {
    r.map_err(|e| Error::msg(e.0))
}

/// `dir_subdir(h, name) -> handle`: attenuate to a confined subdirectory,
/// minting a new handle.
fn host_dir_subdir(mut caller: Caller<'_, VmState>, h: i32, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let base = dir_base(&caller, h)?;
    let sub = confine(crate::confine::resolve(&base, &name))?;
    let dirs = &mut caller.data_mut().dirs;
    dirs.push(sub);
    Ok((dirs.len() - 1) as i32)
}

/// Look up a `File` handle's confined path (trap on a forged/out-of-range handle).
fn file_path(caller: &Caller<'_, VmState>, f: i32) -> Result<std::path::PathBuf> {
    caller
        .data()
        .files
        .get(f as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid File handle {f}")))
}

/// `dir_open(h, rel) -> file handle` (RFC-0012): open an existing file confined to
/// Dir handle `h`, minting a `File` handle. `dir_create` is the write counterpart
/// (the target need not exist yet).
fn host_dir_open(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve(&base, &rel))?;
    let files = &mut caller.data_mut().files;
    files.push(path);
    Ok((files.len() - 1) as i32)
}

/// `dir_create(h, rel) -> file handle`: like `dir_open` but for a not-yet-existing
/// target (confined via its parent, like `dir_write`).
fn host_dir_create(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve_write(&base, &rel))?;
    let files = &mut caller.data_mut().files;
    files.push(path);
    Ok((files.len() - 1) as i32)
}

/// `file_read_len(f) -> byte length`: read the confined file handle NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// Mirrors `dir_read_len`, but a `File` is a leaf so there is no path argument.
fn host_file_read_len(mut caller: Caller<'_, VmState>, f: i32) -> Result<i32> {
    let path = file_path(&caller, f)?;
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| Error::msg(format!("read failed for `{}`: {e}", path.display())))?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

/// `file_write(f, contents)`: write a confined file handle (RFC-0012).
fn host_file_write(mut caller: Caller<'_, VmState>, f: i32, contents_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let contents = read_wstr(mem.data(&caller), contents_ptr)?;
    let path = file_path(&caller, f)?;
    std::fs::write(&path, contents)
        .map_err(|e| Error::msg(format!("write failed for `{}`: {e}", path.display())))
}

/// `dir_read_len(h, rel) -> byte length`: read the confined file NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// A failed read traps — the interpreter errors on it too.
fn host_dir_read_len(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve(&base, &rel))?;
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| Error::msg(format!("read failed for `{}`: {e}", path.display())))?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

/// `exec_run(h, path, args, stdin) -> byte length`: spawn the executable named by
/// `path` within Dir handle `h` (confined like `dir_read`), passing `args` (a
/// single `\0`-joined argv string) and `stdin`. Captures the child's stdout+stderr
/// and stages a payload `"<exit_code>\n<stdout><stderr>"`, reporting its byte
/// length; the guest allocates and calls `fill_pending`. Mirrors `dir_read_len`'s
/// two-phase protocol so both backends produce identical results. Needs `Exec`.
fn host_exec_run(
    mut caller: Caller<'_, VmState>,
    h: i32,
    path_ptr: i32,
    args_ptr: i32,
    stdin_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let path = read_wstr(data, path_ptr)?;
    let joined = read_wstr(data, args_ptr)?;
    let stdin = read_wstr(data, stdin_ptr)?;
    let base = dir_base(&caller, h)?;
    let prog = confine(crate::confine::resolve(&base, &path))?;
    let argv: Vec<&str> = if joined.is_empty() { Vec::new() } else { joined.split('\0').collect() };
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&prog)
        .args(&argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("exec failed to spawn `{}`: {e}", prog.display())))?;
    if let Some(mut sin) = child.stdin.take() {
        sin.write_all(stdin.as_bytes())
            .map_err(|e| Error::msg(format!("exec failed writing stdin to `{}`: {e}", prog.display())))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| Error::msg(format!("exec failed running `{}`: {e}", prog.display())))?;
    let code = output.status.code().unwrap_or(-1);
    let out = String::from_utf8_lossy(&output.stdout);
    let serr = String::from_utf8_lossy(&output.stderr);
    let payload = format!("{code}\n{out}{serr}");
    let len = payload.len() as i32;
    caller.data_mut().pending = Some(payload.into_bytes());
    Ok(len)
}

/// `fill_pending(out_ptr)`: write the bytes staged by the matching size call.
fn host_fill_pending(mut caller: Caller<'_, VmState>, out_ptr: i32) -> Result<()> {
    let bytes = caller
        .data_mut()
        .pending
        .take()
        .ok_or_else(|| Error::msg("fill_pending called with nothing staged"))?;
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing staged data into guest memory: {e}")))
}

/// `dir_exists(h, rel) -> bool`: total — an escaping or missing path is `false`.
fn host_dir_exists(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let ok = crate::confine::resolve(&base, &rel)
        .map(|p| p.exists())
        .unwrap_or(false);
    Ok(ok as i32)
}

/// `dir_is_dir(h, rel) -> bool`: total, like `dir_exists`.
fn host_dir_is_dir(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let ok = crate::confine::resolve(&base, &rel)
        .map(|p| p.is_dir())
        .unwrap_or(false);
    Ok(ok as i32)
}

/// `dir_list_size(h) -> bytes`: read the directory NOW (sorted names, matching
/// the interpreter), stage the listing, and report the total byte size of the
/// witchy `List(String)` structure the guest must reserve.
fn host_dir_list_size(mut caller: Caller<'_, VmState>, h: i32) -> Result<i32> {
    let base = dir_base(&caller, h)?;
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .map_err(|e| Error::msg(format!("list failed for `{}`: {e}", base.display())))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let size = 4 + 8 * names.len() + names.iter().map(|n| 4 + n.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(names);
    Ok(size as i32)
}

/// `args_size() -> bytes`: stage the host-provided argv and report the byte
/// size of its `List(String)` structure (laid out by `write_pending_list`).
fn host_args_size(mut caller: Caller<'_, VmState>) -> Result<i32> {
    let args = caller.data().caps.args.clone();
    let size = 4 + 8 * args.len() + args.iter().map(|a| 4 + a.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(args);
    Ok(size as i32)
}

/// `write_pending_list(base_ptr)`: lay the staged string list out at `base_ptr`
/// in the guest's own list format — `[count][count x i64 slots][string
/// objects...]`, each slot holding the absolute guest pointer of its
/// `[len][bytes]` string. Authority-free: it only writes already-staged data.
fn host_write_pending_list(mut caller: Caller<'_, VmState>, base_ptr: i32) -> Result<()> {
    let names = caller
        .data_mut()
        .pending_list
        .take()
        .ok_or_else(|| Error::msg("write_pending_list called with nothing staged"))?;
    let n = names.len();
    let mut buf = Vec::with_capacity(4 + 8 * n + names.iter().map(|s| 4 + s.len()).sum::<usize>());
    buf.extend_from_slice(&(n as i32).to_le_bytes());
    let strings_start = base_ptr as i64 + 4 + 8 * n as i64;
    let mut offset = 0i64;
    for name in &names {
        let ptr = strings_start + offset;
        buf.extend_from_slice(&ptr.to_le_bytes());
        offset += 4 + name.len() as i64;
    }
    for name in &names {
        buf.extend_from_slice(&(name.len() as i32).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
    }
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, base_ptr as usize, &buf)
        .map_err(|e| Error::msg(format!("writing directory listing into guest memory: {e}")))
}

/// `dir_write(h, rel, contents)`: write a confined file (trap on failure or
/// escape — the interpreter errors on both).
fn host_dir_write(
    mut caller: Caller<'_, VmState>,
    h: i32,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve_write(&base, &rel))?;
    std::fs::write(&path, contents)
        .map_err(|e| Error::msg(format!("write failed for `{}`: {e}", path.display())))
}

/// `dir_append(h, rel, contents)`: append to a confined file, creating it if
/// absent — `dir_write`'s confinement without clobbering existing contents.
fn host_dir_append(
    mut caller: Caller<'_, VmState>,
    h: i32,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve_write(&base, &rel))?;
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(|e| Error::msg(format!("append failed for `{}`: {e}", path.display())))
}

/// `dir_make_dir(h, name)`: create a confined subdirectory (idempotent).
fn host_dir_make_dir(mut caller: Caller<'_, VmState>, h: i32, name_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve_write(&base, &name))?;
    std::fs::create_dir_all(&path)
        .map_err(|e| Error::msg(format!("make_dir failed for `{}`: {e}", path.display())))
}

// --- build-time host ops ---
//
// The build sandbox grants a single `BuildOut` (the confined output dir) and the
// `BuildRead` roots, both held host-side in `VmState` — the guest's handle is
// opaque and ignored. `write_out` confines through the same `resolve_write` as a
// runtime `Dir`; `read_build` tries each granted root, matching the interpreter.

/// `build_out_write(_h, name, contents)`: write generated source into the build
/// output sandbox (trap on escape — a `..` can't leave it).
fn host_build_out_write(
    mut caller: Caller<'_, VmState>,
    _h: i32,
    name_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let name = read_wstr(data, name_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let base = caller
        .data()
        .build_out
        .clone()
        .ok_or_else(|| Error::msg("write_out: no BuildOut grant"))?;
    let path = confine(crate::confine::resolve_write(&base, &name))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, contents)
        .map_err(|e| Error::msg(format!("write_out failed for `{}`: {e}", path.display())))
}

/// `build_read_len(_h, rel) -> byte length`: read the file from the first granted
/// read root that holds it, stage its bytes, and report the length; the guest
/// allocates and calls `fill_pending` (identical staging to `dir_read_len`).
fn host_build_read_len(mut caller: Caller<'_, VmState>, _h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let roots = caller.data().build_read_roots.clone();
    let mut last = String::from("no granted read root");
    for base in &roots {
        match crate::confine::resolve(base, &rel) {
            Ok(path) => match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    let len = contents.len() as i32;
                    caller.data_mut().pending = Some(contents.into_bytes());
                    return Ok(len);
                }
                Err(e) => last = format!("`{}`: {e}", path.display()),
            },
            Err(e) => last = e.0,
        }
    }
    Err(Error::msg(format!("read_build: `{rel}` not found in any granted read root ({last})")))
}

// --- the Net capability family ---
//
// Same shape as Dir: a guest `Net` is an i32 handle into the VM's host-side
// allowlist table (`restrict` mints narrower entries); `Socket`/`Listener` are
// handles into host-side connection tables. The allowlist check is the SAME
// exact-match rule the interpreter applies, so the backends agree on which
// addresses are reachable.

fn net_allow(caller: &Caller<'_, VmState>, h: i32) -> Result<Vec<String>> {
    caller
        .data()
        .nets
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Net handle {h}")))
}

/// `net_restrict(h, addr) -> handle`: attenuate to the allowlisted address set. `addr` may
/// carry several patterns newline-joined (a `confine.union`); each must already be admitted.
fn host_net_restrict(mut caller: Caller<'_, VmState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    let narrowed = witchy_caps::capabilities::net_only(&allow, &addr)
        .map_err(|p| Error::msg(format!("restrict: `{p}` is not in this Net capability")))?;
    let nets = &mut caller.data_mut().nets;
    nets.push(narrowed);
    Ok((nets.len() - 1) as i32)
}

/// `net_deny(h, addr) -> handle`: subtract an address pattern (RFC-0011 `net.deny`), or
/// several newline-joined (a `confine.union`). A monotone exclusion — recorded as
/// `!`-prefixed entries the shared `net_allows` honours; no admittance check (deny narrows).
fn host_net_deny(mut caller: Caller<'_, VmState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let mut allow = net_allow(&caller, h)?;
    for p in addr.split('\n') {
        allow.push(format!("!{p}"));
    }
    let nets = &mut caller.data_mut().nets;
    nets.push(allow);
    Ok((nets.len() - 1) as i32)
}

/// `net_connect(h, addr) -> Socket handle`: dial an allowlisted address.
fn host_net_connect(mut caller: Caller<'_, VmState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    let (tls, host_port) = crate::net::parse_scheme(&addr);
    let targets = witchy_caps::capabilities::resolve_admitted(&allow, host_port)
        .map_err(|e| Error::msg(format!("connect: {e}")))?;
    let stream = crate::net::dial(&targets, tls, host_port)
        .map_err(|e| Error::msg(format!("connect to `{addr}` failed: {e}")))?;
    let sockets = &mut caller.data_mut().sockets;
    sockets.push(std::io::BufReader::new(stream));
    Ok((sockets.len() - 1) as i32)
}

/// `net_try_connect(h, addr) -> Socket handle | -1`: dial like `net_connect`,
/// but a failed connection yields the `-1` sentinel (witchy wraps it as
/// `Option(Socket)`'s `None`) instead of trapping. A capability violation — an
/// address outside the allowlist — still traps: that is a policy breach, not a
/// transient dial failure.
fn host_net_try_connect(mut caller: Caller<'_, VmState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    let (tls, host_port) = crate::net::parse_scheme(&addr);
    let targets = witchy_caps::capabilities::resolve_admitted(&allow, host_port)
        .map_err(|e| Error::msg(format!("try_connect: {e}")))?;
    match crate::net::dial(&targets, tls, host_port) {
        Ok(stream) => {
            let sockets = &mut caller.data_mut().sockets;
            sockets.push(std::io::BufReader::new(stream));
            Ok((sockets.len() - 1) as i32)
        }
        Err(_) => Ok(-1),
    }
}

/// `net_listen(h, addr) -> Listener handle`: bind an allowlisted address.
fn host_net_listen(mut caller: Caller<'_, VmState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    if !witchy_caps::capabilities::net_allows(&allow, &addr) {
        bail!("listen: `{addr}` is not permitted by this Net capability");
    }
    let listener = std::net::TcpListener::bind(&addr)
        .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?;
    let listeners = &mut caller.data_mut().listeners;
    listeners.push(listener);
    Ok((listeners.len() - 1) as i32)
}

/// `net_accept(listener) -> Socket handle`: block for a client connection.
fn host_net_accept(mut caller: Caller<'_, VmState>, lid: i32) -> Result<i32> {
    let state = caller.data_mut();
    let listener = state
        .listeners
        .get(lid as usize)
        .ok_or_else(|| Error::msg("invalid listener"))?;
    let (stream, _peer) = listener
        .accept()
        .map_err(|e| Error::msg(format!("accept failed: {e}")))?;
    state
        .sockets
        .push(std::io::BufReader::new(Box::new(stream) as Box<dyn Stream>));
    Ok((state.sockets.len() - 1) as i32)
}

use crate::net::Stream;

fn socket_of(state: &mut VmState, sid: i32) -> Result<&mut std::io::BufReader<Box<dyn Stream>>> {
    state
        .sockets
        .get_mut(sid as usize)
        .ok_or_else(|| Error::msg("invalid socket"))
}

/// `net_send_line(sock, s)`: write the string and a trailing newline.
fn host_net_send_line(mut caller: Caller<'_, VmState>, sid: i32, line_ptr: i32) -> Result<()> {
    use std::io::Write;
    let mem = memory_of(&mut caller)?;
    let line = read_wstr(mem.data(&caller), line_ptr)?;
    let sock = socket_of(caller.data_mut(), sid)?;
    sock.get_mut()
        .write_all(line.as_bytes())
        .and_then(|_| sock.get_mut().write_all(b"\n"))
        .map_err(|e| Error::msg(format!("send failed: {e}")))
}

/// `net_send_bytes(sock, s)`: write the exact bytes, no framing added.
fn host_net_send_bytes(mut caller: Caller<'_, VmState>, sid: i32, ptr: i32) -> Result<()> {
    use std::io::Write;
    let mem = memory_of(&mut caller)?;
    let s = read_wstr(mem.data(&caller), ptr)?;
    let sock = socket_of(caller.data_mut(), sid)?;
    sock.get_mut()
        .write_all(s.as_bytes())
        .map_err(|e| Error::msg(format!("send failed: {e}")))
}

/// `net_recv_line_len(sock) -> len`: read one line NOW (newline trimmed, like
/// the interpreter), stage it, and report its length for `fill_pending`.
fn host_net_recv_line_len(mut caller: Caller<'_, VmState>, sid: i32) -> Result<i32> {
    use std::io::BufRead;
    let state = caller.data_mut();
    let sock = socket_of(state, sid)?;
    let mut line = String::new();
    sock.read_line(&mut line)
        .map_err(|e| Error::msg(format!("recv failed: {e}")))?;
    let trimmed = line.trim_end_matches('\n').to_string();
    let len = trimmed.len() as i32;
    state.pending = Some(trimmed.into_bytes());
    Ok(len)
}

/// `net_recv_all_len(sock) -> len`: read to EOF NOW (lossy UTF-8, like the
/// interpreter), stage it, and report its length.
fn host_net_recv_all_len(mut caller: Caller<'_, VmState>, sid: i32) -> Result<i32> {
    use std::io::Read;
    let state = caller.data_mut();
    let sock = socket_of(state, sid)?;
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf)
        .map_err(|e| Error::msg(format!("recv failed: {e}")))?;
    let s = String::from_utf8_lossy(&buf).into_owned();
    let len = s.len() as i32;
    state.pending = Some(s.into_bytes());
    Ok(len)
}

/// `net_recv_bytes_len(sock, n) -> len`: read exactly `n` bytes (fewer only on
/// early EOF, matching the interpreter), stage them lossily, report the length.
fn host_net_recv_bytes_len(mut caller: Caller<'_, VmState>, sid: i32, n: i64) -> Result<i32> {
    use std::io::Read;
    let state = caller.data_mut();
    let sock = socket_of(state, sid)?;
    let want = n.max(0) as usize;
    // `want` is guest-supplied (up to i64::MAX); do NOT pre-allocate it — that would let a
    // module request `recv_bytes(sock, huge)` and ABORT the host with a multi-GB/EB Vec even
    // when the socket holds almost nothing. Read in bounded chunks so memory tracks the bytes
    // actually received, not the claimed count.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while buf.len() < want {
        let to_read = (want - buf.len()).min(chunk.len());
        match sock.read(&mut chunk[..to_read]) {
            Ok(0) => break,
            Ok(k) => buf.extend_from_slice(&chunk[..k]),
            Err(e) => bail!("recv failed: {e}"),
        }
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    let len = s.len() as i32;
    state.pending = Some(s.into_bytes());
    Ok(len)
}

/// `net_close(sock)`: shut the connection down (idempotent).
fn host_net_close(mut caller: Caller<'_, VmState>, sid: i32) -> Result<()> {
    if let Some(sock) = caller.data_mut().sockets.get_mut(sid as usize) {
        sock.get_mut().shutdown();
    }
    Ok(())
}

/// The raw bytes of the secret at `handle` (an index into the host secret table).
/// Handle 0 falls back to the single `signing_key` so VM-spawn sites that grant a
/// lone Secret (without populating `secrets`) keep working.
fn secret_seed_bytes(caps: &Capabilities, handle: i32) -> Result<Vec<u8>> {
    if let Some((_, bytes)) = usize::try_from(handle).ok().and_then(|h| caps.secrets.get(h)) {
        return Ok(bytes.clone());
    }
    if handle == 0 {
        if let Some(seed) = caps.signing_key {
            return Ok(seed.to_vec());
        }
    }
    Err(Error::msg("crypto: no secret at that handle (none granted?)"))
}

/// `crypto.sign(key, msg_ptr, out_data_ptr)`: sign the message with the secret at
/// handle `key` (host-side bytes) and write the 128 hex signature bytes into the
/// guest's pre-allocated string — through the same native registry the interpreter
/// uses, so the output is byte-identical.
fn host_crypto_sign(mut caller: Caller<'_, VmState>, key: i32, msg_ptr: i32, out_ptr: i32) -> Result<()> {
    use crate::value::NativeValue as Value;
    let bytes = secret_seed_bytes(&caller.data().caps, key)?;
    let mem = memory_of(&mut caller)?;
    let msg = read_wstr(mem.data(&caller), msg_ptr)?;
    let f = crate::native::lookup("crypto.sign")
        .ok_or_else(|| Error::msg("crypto.sign is not registered"))?;
    let sig = match f(&[Value::Secret(bytes), Value::Str(msg)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.sign did not return a String")),
    };
    mem.write(&mut caller, out_ptr as usize, sig.as_bytes())
        .map_err(|e| Error::msg(format!("writing signature into guest memory: {e}")))
}

/// `crypto.public_key(key, out_data_ptr)`: write the 64 hex public-key bytes of the
/// secret at handle `key` (safe to publish) into the guest's pre-allocated string.
fn host_crypto_public_key(mut caller: Caller<'_, VmState>, key: i32, out_ptr: i32) -> Result<()> {
    use crate::value::NativeValue as Value;
    let bytes = secret_seed_bytes(&caller.data().caps, key)?;
    let f = crate::native::lookup("crypto.public_key")
        .ok_or_else(|| Error::msg("crypto.public_key is not registered"))?;
    let pk = match f(&[Value::Secret(bytes)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.public_key did not return a String")),
    };
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, pk.as_bytes())
        .map_err(|e| Error::msg(format!("writing public key into guest memory: {e}")))
}

/// `secretstore_lookup(name_ptr) -> i32`: the host-table handle of the secret
/// named `name`, or -1 if it was not granted. The bytes never cross into the
/// guest — only the opaque handle, which `crypto.sign`/`reveal` resolve back to
/// host-side bytes. `signing` resolves to handle 0 (the `--signing-key` slot).
fn host_secretstore_lookup(mut caller: Caller<'_, VmState>, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let caps = &caller.data().caps;
    if let Some(i) = caps.secrets.iter().position(|(n, _)| *n == name) {
        return Ok(i as i32);
    }
    if name == "signing" && caps.signing_key.is_some() {
        return Ok(0);
    }
    Ok(-1)
}

/// `crypto_reveal_len(key) -> i32`: reveal the secret at handle `key` as a string
/// (its raw bytes, lossy UTF-8 — through the same native registry the interpreter
/// uses), stage it for `fill_pending`, and report its byte length. For value
/// secrets handed to an external sink; the bytes only cross into the guest here.
fn host_crypto_reveal_len(mut caller: Caller<'_, VmState>, key: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let bytes = secret_seed_bytes(&caller.data().caps, key)?;
    if witchy_caps::capabilities::secret_is_signing_key(
        caller.data().caps.signing_key.as_ref().map(|s| s.as_slice()),
        &bytes,
    ) {
        bail!("the signing key is not revealable; use crypto.sign / crypto.public_key");
    }
    let f = crate::native::lookup("crypto.reveal")
        .ok_or_else(|| Error::msg("crypto.reveal is not registered"))?;
    let s = match f(&[Value::Secret(bytes)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("crypto.reveal did not return a String")),
    };
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
    Ok(len)
}

// --- small helpers for safe guest-memory access ---

/// Read a witchy string value (a `[i32 len][bytes...]` header) at `ptr`.
/// Run Binaryen's `wasm-opt -O2` over a module before Cranelift sees it — a
/// mature optimizer (inlining, GVN, const-prop, local coalescing) over our
/// deliberately naive emitter. OPT-IN via `WITCHY_WASM_OPT=1` (it costs
/// tens of milliseconds per module, which the test suite's hundreds of tiny
/// modules should not pay), and it degrades to the input untouched when the
/// binary is missing or fails — never a hard dependency.
pub fn optimize_module(input: &[u8]) -> Vec<u8> {
    if std::env::var_os("WITCHY_WASM_OPT").is_none_or(|v| v != "1") {
        return input.to_vec();
    }
    // A private, randomly named, owner-only directory per invocation — never
    // predictable paths in the shared temp dir (symlink-attack surface).
    let private_dir = (|| -> Option<std::path::PathBuf> {
        for _ in 0..4 {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).ok()?;
            let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let dir = std::env::temp_dir().join(format!("witchy-opt-{name}"));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            if builder.create(&dir).is_ok() {
                return Some(dir);
            }
        }
        None
    })();
    let Some(dir) = private_dir else {
        return input.to_vec();
    };
    let src = dir.join("module.wat");
    let out = dir.join("module.wasm");
    let optimized = (|| -> Option<Vec<u8>> {
        std::fs::write(&src, input).ok()?;
        let status = std::process::Command::new("wasm-opt")
            .args(["-O2", "--all-features"])
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        std::fs::read(&out).ok()
    })();
    let _ = std::fs::remove_dir_all(&dir);
    optimized.unwrap_or_else(|| input.to_vec())
}

fn read_wstr(data: &[u8], ptr: i32) -> Result<String> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let bytes = slice(data, ptr + 4, len)?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Read a guest `List(String)` — `[count: i32][count x i64 string pointers]`,
/// the layout `write_pending_list` produces and list literals share.
fn read_wstr_list(data: &[u8], ptr: i32) -> Result<Vec<String>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    // SEC: `len` is read from guest memory, so a crafted module can claim a huge count.
    // Do NOT pre-reserve from it — `Vec::with_capacity(len)` would try to allocate ~51GB and
    // ABORT the host process. Cap the hint at the most 8-byte slots that could fit in memory
    // (the loop below still fails closed on the first out-of-bounds slot read).
    let mut out = Vec::with_capacity((len.max(0) as usize).min(data.len() / 8));
    for i in 0..len {
        let slot = slice(data, ptr + 4 + 8 * i, 8)?;
        let elem = i64::from_le_bytes([
            slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7],
        ]);
        out.push(read_wstr(data, elem as i32)?);
    }
    Ok(out)
}

fn memory_of(caller: &mut Caller<'_, VmState>) -> Result<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(Error::msg("VM does not export a linear `memory`")),
    }
}

fn slice(data: &[u8], ptr: i32, len: i32) -> Result<&[u8]> {
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| Error::msg("pointer + length overflows"))?;
    data.get(start..end)
        .ok_or_else(|| Error::msg(format!("out-of-bounds guest memory access ({start}..{end})")))
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
