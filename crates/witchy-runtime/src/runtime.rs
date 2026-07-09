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
    bail, Cache, CacheConfig, Caller, Config, Engine, Error, Extern, ExternRef, Instance, Linker,
    Memory, Module, Result, Rooted, Store, StoreLimits, StoreLimitsBuilder,
};
use witchy_wir::layout::HEAP_REDZONE;

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
    // Trusted, owner-private base ONLY. `build_module` loads artifacts from here via
    // `unsafe Module::deserialize` (native code), so a directory another local user can
    // write is a native-code-execution vector OUTSIDE the wasm sandbox. Never fall back
    // to a world-writable temp dir: with no trusted base, return `None` and the caller
    // just compiles fresh (it already treats `None` as "no AOT fast-path").
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    let dir = base.join("witchy").join("aot");
    std::fs::create_dir_all(&dir).ok()?;
    // Owner-only (0700): even under a trusted HOME, deny group/other write so a
    // shared-account co-tenant cannot plant a `.cwasm` for `deserialize` to trust.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// (RFC-0034 L1) Run Binaryen `wasm-opt -O2` on the module — a heavier optimizer
/// than Cranelift's Speed tier (GVN, inlining, DCE, local CSE). It runs ONLY on
/// the cold compile path below (the result is AOT-cached), so warm runs pay
/// nothing. Optional + graceful: returns the input unchanged if `wasm-opt` isn't
/// on PATH or fails, or if the `wasm-opt` lever is off (`WITCHY_OPT=-wasm-opt`).
/// `--all-features` so it accepts witchy's bulk-memory (`memory.copy`); -O2 never
/// introduces new features, so the output runs under the same wasmtime config.
fn binaryen_enabled() -> bool {
    witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::WasmOpt)
}

fn binaryen_optimize(wasm: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;
    if !binaryen_enabled() {
        return Cow::Borrowed(wasm);
    }
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).ok();
    let tag: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir();
    let inp = dir.join(format!("witchy_wopt_{tag}.in.wasm"));
    let outp = dir.join(format!("witchy_wopt_{tag}.out.wasm"));
    let run = (|| -> Option<Vec<u8>> {
        std::fs::write(&inp, wasm).ok()?;
        let ok = std::process::Command::new("wasm-opt")
            .args(["-O2", "--all-features", "-o"])
            .arg(&outp)
            .arg(&inp)
            .output()
            .ok()?
            .status
            .success();
        if ok { std::fs::read(&outp).ok() } else { None }
    })();
    let _ = std::fs::remove_file(&inp);
    let _ = std::fs::remove_file(&outp);
    match run {
        Some(bytes) => Cow::Owned(bytes),
        None => Cow::Borrowed(wasm),
    }
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
        // The artifact differs by whether Binaryen ran, so the cache key must too —
        // otherwise toggling it returns a stale (un)optimized module under the same key.
        h.update(if binaryen_enabled() { b"+wopt".as_slice() } else { b"-wopt".as_slice() });
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
    let optimized = binaryen_optimize(opt_wasm);
    let module = Module::new(engine, &optimized)?;
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

/// One named secret backing the `SecretStore` capability: its name, its raw
/// bytes (kept host-side — the guest only ever holds the handle index), and
/// whether it is **use-only** (RFC-0060). A use-only secret is still consumable
/// by handle (`crypto.sign`, TLS serving), but `crypto.reveal` on it errors — so
/// key material a program *serves* with can never be read back into guest memory.
/// The default (`use_only == false`) is revealable, preserving existing behavior.
#[derive(Clone, Debug, Default)]
pub struct SecretGrant {
    pub name: String,
    pub bytes: Vec<u8>,
    pub use_only: bool,
}

impl SecretGrant {
    /// A revealable named secret (the default grant shape).
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self { name: name.into(), bytes, use_only: false }
    }
}

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
    /// May draw cryptographic randomness via `witchy.rand_u64` (a `Rand` capability).
    pub rand: bool,
    /// May read process environment variables via `witchy.env_*` (an `Env`
    /// capability).
    pub env: bool,
    /// Optional per-key restriction for `Env` host reads. `None` preserves the
    /// ordinary coarse `Env` grant; `Some(keys)` links the same host operation but
    /// refuses every key not named here. BuildEnv uses this path.
    pub env_allow: Option<Vec<String>>,
    /// The directory subtree backing the root `Dir` capability (handle 0).
    /// `None` denies the filesystem entirely; the `dir_read`/`dir_write` flags
    /// pick which operation families are linked within it.
    pub dir_root: Option<std::path::PathBuf>,
    /// Additional `Dir` grants beyond handle 0, for a `main` that takes several
    /// `Dir` parameters (positional: the i-th `Dir` param maps to handle `i`).
    /// Handle 0 is `dir_root`; these back handles 1.. in order.
    pub dir_roots: Vec<std::path::PathBuf>,
    /// Per-Dir-handle rights, aligned with `dir_root` followed by `dir_roots`.
    /// Empty means "use the coarse `dir_read`/`dir_write` grant for each handle",
    /// preserving older callers that grant one uniform root.
    pub dir_rights: Vec<FsRights>,
    /// Direct `File` grants (RFC-0012): the i-th `File` parameter of `main` maps to
    /// the i-th path here. Backed by `--file` or `[files]`.
    pub file_grants: Vec<std::path::PathBuf>,
    /// Per-File-grant rights, aligned with `file_grants`. Empty means full read and
    /// write, which preserves the historical precompiled-wasm `--file` behavior
    /// until that launch surface grows explicit rights syntax.
    pub file_rights: Vec<FsRights>,
    /// May read within `dir_root` (read/exists/is_dir/list/subdir).
    pub dir_read: bool,
    /// May write within `dir_root` (write/make_dir).
    pub dir_write: bool,
    /// May spawn a native subprocess via `exec` (an `Exec` capability). The
    /// executable is named + confined through a `Dir[Read]` handle, so `exec`
    /// without filesystem read is useless — but the flag is tracked separately so
    /// `Exec` appears as its own authority. See rfcs/0004-self-hosted-cli.md.
    pub exec: bool,
    /// Optional per-tool restriction for native subprocess execution. `None`
    /// preserves the ordinary coarse `Exec` grant; `Some(tools)` is used by
    /// BuildExec and refuses every tool not named here.
    pub exec_allow: Option<Vec<String>>,
    /// The `host:port` allowlist backing the root `Net` capability (handle 0).
    /// `None` denies the network entirely; the verb flags below pick which
    /// operation families are linked within it.
    pub net_allow: Option<Vec<String>>,
    /// The `host:port` allowlist backing a build step's `BuildNet` capability.
    /// Kept separate from runtime `Net` so precompiled modules granted network
    /// authority cannot import the build-only `fetch_build` primitive.
    pub build_net_allow: Option<Vec<String>>,
    /// May dial out (`connect`/`restrict`) to allowlisted addresses.
    pub net_connect: bool,
    /// May bind and accept (`listen`/`accept`) on allowlisted addresses.
    pub net_listen: bool,
    /// The program's command-line arguments (`main(args: List(String))`).
    /// Pure input chosen by the host, not authority.
    pub args: Vec<String>,
    /// RFC-0038: `[user_caps]` grant field values for a `main` binding bare
    /// grantable capabilities — outer index is the grantable-cap parameter
    /// (declaration order), inner is its policy fields in order. Empty otherwise.
    pub user_cap_fields: Vec<Vec<String>>,
    /// The Ed25519 seed backing the root `Secret` capability. The key
    /// material stays host-side; the guest only ever sees signatures and the
    /// public key.
    pub signing_key: Option<[u8; 32]>,
    /// Named secrets backing the `SecretStore` capability, in handle order — a
    /// `Secret` value is an index into this table, so the raw bytes stay host-side
    /// (the guest only ever holds the handle). Index 0 is the `signing` secret
    /// (from `--signing-key`), so a bare `Secret` capability is handle 0.
    pub secrets: Vec<SecretGrant>,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FsRights {
    pub read: bool,
    pub write: bool,
}

impl FsRights {
    pub const fn new(read: bool, write: bool) -> Self {
        Self { read, write }
    }

    const fn full() -> Self {
        Self { read: true, write: true }
    }
}

#[derive(Clone, Debug)]
struct DirHandle {
    base: std::path::PathBuf,
    policy: String,
    rights: FsRights,
}

#[derive(Clone, Debug)]
struct FileHandle {
    path: std::path::PathBuf,
    rights: FsRights,
}

/// Host-side state owned by a spawned VM's `Store`: its capability grant, output
/// buffer, and the host-side `Dir`/`Net`/build handle tables. One state type
/// means one set of capability host functions.
const HEAP_POISON: u8 = 0xDB;

pub struct VmState {
    id: VmId,
    caps: Capabilities,
    pub(crate) limits: StoreLimits,
    /// Everything the VM has printed (via the `print`/`print_int`
    /// capabilities), so the host can observe a compiled program's output.
    pub(crate) output: Arc<Mutex<Vec<String>>>,
    /// The `Dir` capability handle table: index 0 is the granted root, and each
    /// `subdir` mints a new confined entry. The handle carries base, entry policy,
    /// and read/write rights host-side; guest code only ever holds the i32 index.
    dirs: Vec<DirHandle>,
    /// Direct `File` grants awaiting wrapper-local `mint_file(i)`. A live guest
    /// `File` is an externref carrying a cloned `FileHandle`, not this index.
    files: Vec<FileHandle>,
    /// A host->guest transfer staged by a size-probing call (`dir_read_len`,
    /// `net_recv_*_len`, ...) and consumed by the matching `fill_pending`, so
    /// the data is read once with no time-of-check/time-of-use gap.
    pub(crate) pending: Option<Vec<u8>>,
    /// A staged directory listing (`dir_list_size` -> `dir_list_write`).
    pub(crate) pending_list: Option<Vec<String>>,
    /// (RFC-0032) Staged scalar results of `vm.par_map` (`vm_par_map_run` computes
    /// them — in parallel on worker VMs — then `vm_par_map_write` lays out the
    /// `List(Int)` `[count][i64..]` into the guest's reserved block).
    pub(crate) pending_ints: Option<Vec<i64>>,
    /// (RFC-0032) Staged raw-byte results of a `Bytes` `vm.par_map` — kept as raw
    /// `Vec<u8>` (NOT `String`) so arbitrary binary survives the round-trip.
    pub(crate) pending_bytes: Option<Vec<Vec<u8>>>,
    /// RFC-0038: `[user_caps]` grant field values for a `main` binding bare
    /// grantable capabilities — outer index = grantable-cap parameter (declaration
    /// order), inner = its policy fields in order. Materialized into the guest by
    /// `user_cap_field_len` + `fill_pending` and wrapped in a record by codegen.
    pub(crate) user_cap_fields: Vec<Vec<String>>,
    /// The `Net` capability handle table: index 0 is the granted allowlist,
    /// and each `restrict` mints a narrower entry — host-side, unforgeable.
    pub(crate) nets: Vec<Vec<String>>,
    /// Open sockets, indexed by the guest's i32 Socket handles. Each is a plain or
    /// TLS byte stream behind one `dyn Stream` (see `Stream`).
    sockets: Vec<std::io::BufReader<Box<dyn Stream>>>,
    /// Listening server sockets, indexed by the guest's i32 Listener handles, each
    /// with its server-TLS config (RFC-0060): `Some` for a `listen_tls` listener,
    /// whose accepts handshake host-side; `None` for plain HTTP. `Arc` so a
    /// `server.serve` worker pool (RFC-0032) can share ONE bound listener across
    /// worker VMs — `TcpListener::accept` is thread-safe, and the kernel load-balances
    /// connections across the workers' accept calls (TLS state is per-connection,
    /// so each worker handshakes its own accepts).
    listeners: Vec<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    /// (RFC-0032) When this VM is a `serve` POOL WORKER, the shared listener (plus
    /// its TLS config) it must reuse instead of binding its own. `listen` returns
    /// this; `serve_pool` sees it set and does NOT spawn another pool (only the
    /// primary spawns workers).
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    /// A build step's confined output directory (`BuildOut`) and read roots
    /// (`BuildRead`) — host-side, so a guest holds only an opaque handle.
    build_out: Option<std::path::PathBuf>,
    build_read_roots: Vec<std::path::PathBuf>,
    /// (RFC-0023) Checked-heap shadow. Each `heap_register(start,end)` the guest's
    /// checked allocators emit records an object's `[start,end)`; the host poisons
    /// `[end, end+HEAP_REDZONE)` and the post-run sweep traps if any poison byte was
    /// overwritten. Empty — and the sweep a no-op — unless the checked codegen ran.
    pub(crate) heap_objects: Vec<(u32, u32)>,
    /// (`Rand`) splitmix64 state for the seeded test/parity path (`WITCHY_RAND_SEED`);
    /// `None` means `rand_u64` draws from the OS CSPRNG instead.
    rand_state: Option<u64>,
    /// (RFC-0032) The engine + compiled module + preemption flag, stashed so a host
    /// op (`vm.par_map`) can instantiate fresh worker VMs on OS threads from inside a
    /// call. `Engine`/`Module` are `Arc`-backed (cheap clone, `Send`+`Sync`).
    engine: Engine,
    module: Module,
    preempt: bool,
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
        self.heap_sweep()?;
        if std::env::var_os("WITCHY_REGION_STATS").is_some_and(|v| v == "1") {
            if let Some(bytes) = self.region_copy_bytes() {
                eprintln!("region copy-out: {bytes} byte(s)");
            }
        }
        Ok(())
    }

    /// (RFC-0023) After the run, prove every checked allocation's trailing redzone is
    /// intact. A flipped poison byte means a write ran past the object's end — an
    /// out-of-object overrun that is in-bounds for the linear memory, so wasmtime never
    /// saw it. A no-op (and free) when nothing was registered, i.e. on uninstrumented
    /// builds. Drains the shadow so a reused VM starts clean.
    fn heap_sweep(&mut self) -> Result<()> {
        let objs = std::mem::take(&mut self.store.data_mut().heap_objects);
        if objs.is_empty() {
            return Ok(());
        }
        let Some(mem) = self.instance.get_memory(&mut self.store, "memory") else {
            return Ok(());
        };
        let data = mem.data(&self.store);
        for (start, end) in objs {
            let rz_start = end as usize;
            let rz_end = rz_start + HEAP_REDZONE;
            if rz_end > data.len() {
                continue;
            }
            if let Some(i) = data[rz_start..rz_end].iter().position(|&b| b != HEAP_POISON) {
                return Err(Error::msg(format!(
                    "HEAP CHECK: object [{start:#x},{end:#x}) redzone byte {i} was \
                     overwritten — a write ran past the object end (a wrong field offset \
                     or a missing ensure())"
                )));
            }
        }
        Ok(())
    }

    /// The total bytes the `region:` copy-outs moved (the exported
    /// `__region_copy_bytes` counter), or None when the module has no regions.
    /// The exported re-own counter: how many in-place accumulation sites
    /// entered with a zero ownership token (each one copies). None when the
    /// module has no in-place machinery. (A test/diagnostic API.)
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

    /// (RFC-0016) The total bytes `$rc_alloc` handed back out of the RC-floor
    /// free-list (reused rather than freshly bumped) — the exported
    /// `__rc_reused_bytes` counter. 0 unless the free-at-overwrite rule (gated
    /// `WITCHY_OPT=rc-floor`) populated the list, so it proves the reclamation
    /// actually fired and recycled. None when the module has no heap.
    pub fn rc_reused_bytes(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__rc_reused_bytes")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// (RFC-0035) Live rc_alloc objects at program end — the exported
    /// `__witchy_live_cells` counter (`$rc_alloc` +1, `$rc_free` -1). A leak metric:
    /// for a fully-reclaiming rc-floor program it stays bounded (the reachable roots);
    /// an unbounded leak grows it with the input. 0 unless a `$rc_free` fired.
    pub fn live_cells(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__witchy_live_cells")
            .and_then(|g| g.get(&mut self.store).i64())
    }

    /// (RFC-0030) The final value of the `$heap` bump-pointer (exported as
    /// `__heap`): the live heap frontier in bytes at program end. With no
    /// region/watermark reclaim this is the peak. For a fixed program the delta
    /// across `WITCHY_OPT` settings shows an allocation optimization firing — e.g.
    /// in-place push keeps an accumulation O(n) where forced-copy is O(n^2). A
    /// test/diagnostic API consumed by `witchy stats`.
    pub fn heap_bytes(&mut self) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, "__heap")
            .and_then(|g| g.get(&mut self.store).i32())
            .map(i64::from)
    }

    /// Everything this VM has printed so far, in order. (Used by tests to
    /// assert a compiled program's behavior end to end.)
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
        // (RFC-0005 step 7) Defense in depth: shrink the codegen/runtime surface to
        // exactly what witchy emits. Disable every WASM proposal we never lower to —
        // threads, SIMD, relaxed-SIMD, multiple memories, tail calls, memory64 — so a
        // miscompile or a crafted module can't reach that machinery. We KEEP the
        // proposals we DO emit (bulk memory for `MemoryCopy`/`MemoryFill`, multi-value
        // for the `*_cap` ABI) at their defaults, and leave reference types / GC on for
        // the eventual externref capability core (RFC-0005 steps 3–6). Cranelift's
        // Spectre mitigations (heap + table access) are ON by default and stay on — do
        // NOT set `signals_based_traps(false)`, which would force them off.
        config.wasm_threads(false);
        config.wasm_simd(false);
        config.wasm_relaxed_simd(false);
        config.wasm_multi_memory(false);
        config.wasm_tail_call(false);
        config.wasm_memory64(false);
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
        // (RFC-0034 L4) Pooling instance allocator: reuse pre-reserved instance
        // slots instead of fresh mmap/teardown per instantiation — the lever for
        // the many-instance paths (serve_pool, a future par_map worker pool). Each
        // slot must reserve witchy's per-instance memory cap (1 GiB), so the pool is
        // kept modest. Opt-in via WITCHY_POOL while we measure whether the up-front
        // reservation pays off for one-shot runs (it likely doesn't — the win is
        // many-instance) before making it default.
        if !preempt && std::env::var_os("WITCHY_POOL").is_some() {
            let mut pool = wasmtime::PoolingAllocationConfig::default();
            pool.total_memories(64);
            pool.total_core_instances(64);
            pool.max_memory_size(16384 * 64 * 1024);
            config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
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
        // `wasm-opt` runs inside `build_module` (ahead of time, on the cold compile
        // only, then AOT-cached) — never here on the hot per-spawn path.
        let module = build_module(&self.engine, wasm.as_ref(), !self.preempt)?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let state =
            vmstate_from_caps(id, &caps, limits, None, &self.engine, &module, self.preempt);

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
    // (RFC-0023) The checked-heap shadow imports are not capabilities — they grant no
    // authority (they only poison/record/reclaim a redzone), so they are always defined
    // and only the checked codegen ever emits calls to them.
    linker.func_wrap("witchy", "heap_register", host_heap_register)?;
    linker.func_wrap("witchy", "heap_frontier", host_heap_frontier)?;

    // (RFC-0045) `__witchy_abort` is not a capability — it grants no authority (it
    // reads only the `(str_ptr)` string the guest hands it, returns nothing to the
    // guest, and its only effect is to terminate execution with a diagnostic label,
    // an ability the guest already has via `unreachable`). So, like the checked-heap
    // imports above, it is ALWAYS defined; codegen calls it right before the trap so
    // the compiled abort carries the interpreter's exact message.
    linker.func_wrap("witchy", "__witchy_abort", host_witchy_abort)?;

    // --- capability wiring: only granted host functions are defined ---
    if caps.print {
        linker.func_wrap("witchy", "print", host_print)?;
    }
    if caps.print_int {
        linker.func_wrap("witchy", "print_int", host_print_int)?;
        // Same "print a computed result" facility, for a Float-returning main.
        linker.func_wrap("witchy", "print_float", host_print_float)?;
    }
    if caps.rand {
        linker.func_wrap("witchy", "rand_u64", host_rand_u64)?;
    }
    if caps.clock {
        linker.func_wrap("witchy", "now", host_now)?;
        linker.func_wrap("witchy", "now_monotonic", host_now_monotonic)?;
    }
    if caps.env {
        linker.func_wrap("witchy", "env_len", host_env_len)?;
        linker.func_wrap("witchy", "env_fill", host_env_fill)?;
    }
    // The Dir family is linked per RIGHT, so a module compiled against a
    // write operation cannot even instantiate under a read-only grant.
    let has_dir = caps.dir_root.is_some();
    let dir_read = caps.dir_read || caps.dir_rights.iter().any(|r| r.read);
    let dir_write = caps.dir_write || caps.dir_rights.iter().any(|r| r.write);
    if has_dir && dir_read {
        linker.func_wrap("witchy", "dir_subdir", host_dir_subdir)?;
        linker.func_wrap("witchy", "dir_only", host_dir_only)?;
        linker.func_wrap("witchy", "dir_read_len", host_dir_read_len)?;
        linker.func_wrap("witchy", "dir_exists", host_dir_exists)?;
        linker.func_wrap("witchy", "dir_is_dir", host_dir_is_dir)?;
        linker.func_wrap("witchy", "dir_list_size", host_dir_list_size)?;
        // RFC-0012/RFC-0005 Stage 2: `dir.open` navigates a readable Dir to a
        // File externref.
        linker.func_wrap("witchy", "dir_open", host_dir_open)?;
    }
    if has_dir && dir_write {
        linker.func_wrap("witchy", "dir_write", host_dir_write)?;
        linker.func_wrap("witchy", "dir_append", host_dir_append)?;
        linker.func_wrap("witchy", "dir_make_dir", host_dir_make_dir)?;
        // RFC-0012/RFC-0005 Stage 2: `dir.create` navigates a writable Dir to a
        // File externref.
        linker.func_wrap("witchy", "dir_create", host_dir_create)?;
    }
    // RFC-0012 File leaf ops: usable on a File obtained by navigation (above) OR
    // granted directly to `main` (`--file`), so they are linked whenever either a
    // Dir grant or a direct File grant is present.
    let has_file_grants = !caps.file_grants.is_empty();
    let direct_file_read = if caps.file_rights.is_empty() {
        has_file_grants
    } else {
        caps.file_rights.iter().any(|r| r.read)
    };
    let direct_file_write = if caps.file_rights.is_empty() {
        has_file_grants
    } else {
        caps.file_rights.iter().any(|r| r.write)
    };
    if (has_dir && dir_read) || direct_file_read {
        linker.func_wrap("witchy", "file_read_len", host_file_read_len)?;
    }
    if (has_dir && dir_write) || direct_file_write {
        linker.func_wrap("witchy", "file_write", host_file_write)?;
    }
    // (RFC-0005 Stage 2) The `run` wrapper mints each `--file` `main` param as an
    // `externref` via `mint_file`; only reachable when a direct File grant exists.
    if has_file_grants {
        linker.func_wrap("witchy", "mint_file", host_mint_file)?;
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
        // (RFC-0020) resolve-then-pin: inspect a name's IPs, then dial an exact IP with
        // the hostname carried only for SNI/Host. Gated with connect (adds no authority —
        // `connect_pinned` re-checks the pinned IP against the same allowlist).
        linker.func_wrap("witchy", "net_resolve_size", host_net_resolve_size)?;
        linker.func_wrap("witchy", "net_connect_pinned", host_net_connect_pinned)?;
        linker.func_wrap("witchy", "net_try_connect_pinned", host_net_try_connect_pinned)?;
    }
    if net && caps.net_listen {
        linker.func_wrap("witchy", "net_listen", host_net_listen)?;
        // (RFC-0060) HTTPS listen: same Listen right, plus a Secret key handle
        // resolved host-side — an ungranted handle fails loudly in
        // `secret_seed_bytes` at listen time, so no extra linking condition.
        linker.func_wrap("witchy", "net_listen_tls", host_net_listen_tls)?;
        linker.func_wrap("witchy", "net_accept", host_net_accept)?;
        // (RFC-0032) The `server.serve` worker pool: spawn one worker VM per core, all
        // sharing the bound listener. No authority of its own — workers get the same
        // caps the server already holds.
        linker.func_wrap("witchy", "serve_pool", host_serve_pool)?;
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
    if caps.env_allow.is_some() {
        linker.func_wrap("witchy", "build_env_len", host_build_env_len)?;
        linker.func_wrap("witchy", "build_env_fill", host_build_env_fill)?;
    }
    if caps.build_net_allow.is_some() {
        linker.func_wrap("witchy", "build_fetch_len", host_build_fetch_len)?;
    }
    if caps.exec_allow.is_some() {
        linker.func_wrap("witchy", "build_exec_run", host_build_exec_run)?;
    }
    // `fill_pending` / `write_pending_list` only write out data already
    // staged by a granted size call — no authority of their own, so they are
    // always available. `args_size` stages the host-chosen argv (pure
    // input, not authority), so it is always available too.
    linker.func_wrap("witchy", "fill_pending", host_fill_pending)?;
    linker.func_wrap("witchy", "write_pending_list", host_write_pending_list)?;
    linker.func_wrap("witchy", "args_size", host_args_size)?;
    // (RFC-0032) `vm.par_map` staging: compute (re-entrant closure calls) + write-out.
    // No authority of their own — the closure runs with the module's existing caps.
    linker.func_wrap("witchy", "vm_par_map_run", host_vm_par_map_run)?;
    linker.func_wrap("witchy", "vm_par_map_write", host_vm_par_map_write)?;
    linker.func_wrap("witchy", "vm_par_map_bytes_run", host_vm_par_map_bytes_run)?;
    linker.func_wrap("witchy", "vm_par_map_bytes_write", host_vm_par_map_bytes_write)?;
    // (RFC-0032) Capability-passing: the only authority the worker gets is the `Dir`
    // the caller already holds and explicitly passes — a re-grant, no new authority.
    linker.func_wrap("witchy", "vm_with_dir_run", host_vm_with_dir_run)?;
    linker.func_wrap("witchy", "vm_serve_run", host_vm_serve_run)?;
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
    linker.func_wrap("witchy", "crypto.__ed25519_verify_status", host_ed25519_verify_status)?;
    linker.func_wrap("witchy", "crypto.sha256", host_sha256)?;
    linker.func_wrap("witchy", "crypto.__ecdsa_p256_verify_status", host_ecdsa_p256_verify_status)?;
    linker.func_wrap("witchy", "crypto.__ecdsa_p256_verify_hex_status", host_ecdsa_p256_verify_hex_status)?;
    linker.func_wrap("witchy", "crypto.__rsa_pkcs1_sha256_verify_status", host_rsa_pkcs1_sha256_verify_status)?;
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
    linker.func_wrap("witchy", "compiler_doc_result_json_len", host_compiler_doc_result_json_len)?;
    linker.func_wrap("witchy", "user_cap_field_len", host_user_cap_field_len)?;
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

/// (RFC-0045) `__witchy_abort(template, a, b, str_ptr)` — the always-linked,
/// authority-free abort channel. Renders the shared [`DiagTemplate`] (the SAME
/// wording the interpreter constructs) from the integer holes `a`/`b` and, when
/// the template has a string hole, the witchy string at `str_ptr`, then traps
/// with the interpreter's `runtime error: <core>` text so the compiled backend's
/// surfaced message matches byte-for-byte. It never returns (codegen still emits
/// `unreachable` after the call). `str_ptr == 0` means "no string hole".
fn host_witchy_abort(
    mut caller: Caller<'_, VmState>,
    template: i32,
    a: i64,
    b: i64,
    str_ptr: i32,
) -> Result<()> {
    use witchy_syntax::diag::{runtime_error, DiagTemplate};
    let Some(tmpl) = DiagTemplate::from_id(template) else {
        bail!("runtime error: abort with unknown diagnostic template id {template}");
    };
    // Read the string hole only for a template that carries one, and only for a
    // non-null pointer (a defensive read: a crafted module could pass junk, but
    // `read_wstr` fails closed on an out-of-bounds slice).
    let s = if str_ptr != 0 {
        let mem = memory_of(&mut caller)?;
        read_wstr(mem.data(&caller), str_ptr).unwrap_or_default()
    } else {
        String::new()
    };
    let core = tmpl.render(a, b, &s);
    // The compiled backend has no per-abort source line/function yet (the site
    // table is a deferred RFC-0045 (c) channel), so surface the bare core message
    // — the differential gate compares the message *core*, which is what
    // distinguishes an abort class and carries its dynamic data.
    bail!("{}", runtime_error("", 0, &core));
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

/// `crypto.__ed25519_verify_status(pk, msg, sig) -> Int`, bridged to the shared native
/// registry (the same implementation the interpreter uses). Each argument is a
/// pointer to a witchy string header (`[i32 len][bytes]`).
fn host_ed25519_verify_status(
    mut caller: Caller<'_, VmState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let args = [
        Value::Str(read_wstr(data, pk)?),
        Value::Str(read_wstr(data, msg)?),
        Value::Str(read_wstr(data, sig)?),
    ];
    let f = crate::native::lookup("crypto.__ed25519_verify_status")
        .ok_or_else(|| Error::msg("crypto.__ed25519_verify_status is not registered"))?;
    match f(&args).map_err(|e| Error::msg(e.message))? {
        Value::Int(n) => Ok(n),
        _ => Err(Error::msg("crypto.__ed25519_verify_status did not return an Int")),
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

/// Shared bridge for private three-string crypto verifier status intrinsics:
/// read three string headers, dispatch through the shared native registry (the
/// SAME aws-lc-rs impl the interpreter uses), return an Int status.
fn host_crypto_verify_status3(
    mut caller: Caller<'_, VmState>,
    native: &str,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i64> {
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
        Value::Int(n) => Ok(n),
        _ => Err(Error::msg(format!("{native} did not return an Int"))),
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

fn host_ecdsa_p256_verify_status(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i64> {
    host_crypto_verify_status3(caller, "crypto.__ecdsa_p256_verify_status", pk, msg, sig)
}

fn host_ecdsa_p256_verify_hex_status(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i64> {
    host_crypto_verify_status3(caller, "crypto.__ecdsa_p256_verify_hex_status", pk, msg, sig)
}

fn host_rsa_pkcs1_sha256_verify_status(caller: Caller<'_, VmState>, pk: i32, msg: i32, sig: i32) -> Result<i64> {
    host_crypto_verify_status3(caller, "crypto.__rsa_pkcs1_sha256_verify_status", pk, msg, sig)
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

/// `user_cap_field_len(param, field) -> Int` (RFC-0038): stage the (param, field)
/// policy string of a bare grantable-capability grant for `fill_pending`, and
/// report its byte length. Out of range is a launch/codegen mismatch — trap (the
/// compiled analog of the interpreter's under-grant error), so both backends
/// refuse a missing grant identically rather than diverging.
fn host_user_cap_field_len(mut caller: Caller<'_, VmState>, param: i32, field: i32) -> Result<i32> {
    let s = caller
        .data()
        .user_cap_fields
        .get(param as usize)
        .and_then(|fs| fs.get(field as usize))
        .cloned()
        .ok_or_else(|| Error::msg("a grantable-capability field is missing from the [user_caps] grant"))?;
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
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

/// `compiler_doc_result_json_len(name_ptr, src_ptr) -> Int`: render the
/// inspectable `compiler.try_doc` JSON result, stage it for `fill_pending`, and
/// report its byte length.
fn host_compiler_doc_result_json_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup("compiler.__doc_result_json")
        .ok_or_else(|| Error::msg("compiler.__doc_result_json is not registered"))?;
    let json = match f(&[Value::Str(name), Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.__doc_result_json did not return a String")),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
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

/// `encoding.*(op, in_header_ptr, out_data_ptr) -> byte length`: read a flat
/// String/Bytes buffer, run the selected hex/base64 transform through the shared
/// native registry (the same implementation the interpreter uses), write the
/// result flat buffer at `out_data_ptr`, and return its byte length. The guest
/// reserves a sufficient buffer (`2*len + slack`) beforehand.
fn host_encoding(mut caller: Caller<'_, VmState>, op: i32, in_ptr: i32, out_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    enum Input {
        String,
        Bytes,
        LossyString,
    }
    let (name, input) = match op {
        0 => ("encoding.hex_encode", Input::String),
        1 => ("encoding.hex_decode_lossy", Input::String),
        2 => ("encoding.base64_encode", Input::String),
        3 => ("encoding.base64_decode_lossy", Input::String),
        4 => ("encoding.hex_to_base64url_lossy", Input::String),
        5 => ("encoding.base64url_decode_lossy", Input::String),
        6 => ("encoding.base64url_to_hex_lossy", Input::String),
        7 => ("encoding.utf8_lossy", Input::LossyString),
        8 => ("encoding.hex_encode_bytes", Input::Bytes),
        9 => ("encoding.base64_encode_bytes", Input::Bytes),
        10 => ("encoding.base64url_encode_bytes", Input::Bytes),
        11 => ("encoding.hex_decode_bytes_raw", Input::String),
        12 => ("encoding.base64_decode_bytes_raw", Input::String),
        13 => ("encoding.base64url_decode_bytes_raw", Input::String),
        _ => return Err(Error::msg(format!("unknown encoding op {op}"))),
    };
    let mem = memory_of(&mut caller)?;
    let arg = match input {
        Input::String => Value::Str(read_wstr(mem.data(&caller), in_ptr)?),
        Input::Bytes => Value::Bytes(read_wbytes(mem.data(&caller), in_ptr)?),
        Input::LossyString => {
            Value::Str(String::from_utf8_lossy(&read_wbytes(mem.data(&caller), in_ptr)?).into_owned())
        }
    };
    let f = crate::native::lookup(name)
        .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
    let out = match f(&[arg]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s.into_bytes(),
        Value::Bytes(bytes) => bytes,
        _ => return Err(Error::msg(format!("{name} did not return a flat buffer"))),
    };
    mem.write(&mut caller, out_ptr as usize, &out)
        .map_err(|e| Error::msg(format!("writing {name} result into guest memory: {e}")))?;
    Ok(out.len() as i32)
}

/// `now() -> Int`: wall-clock milliseconds since the Unix epoch — the same value
/// the interpreter's `now(Clock)` produces. Linked only when the VM was
/// granted a `Clock` capability.
/// `rand_u64() -> i64`: a fresh draw of the `Rand` capability. Seeded (WITCHY_RAND_SEED)
/// it advances the per-VM splitmix64 state so a run is deterministic and parity-stable;
/// unseeded it draws 8 bytes from the OS CSPRNG. Linked only when `Rand` was granted.
fn host_rand_u64(mut caller: Caller<'_, VmState>) -> i64 {
    match &mut caller.data_mut().rand_state {
        Some(state) => crate::rand::seeded_next(state) as i64,
        None => {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).expect("OS CSPRNG is available");
            i64::from_le_bytes(b)
        }
    }
}

fn host_now(_caller: Caller<'_, VmState>) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `now_monotonic() -> i64`: nanoseconds elapsed on a monotonic (steady) clock
/// since first use. The reference `Instant` is set lazily on the first call, so
/// a start/stop bracket around a computation measures its elapsed time without
/// the wall-clock jump hazard of `now`.
fn host_now_monotonic(_caller: Caller<'_, VmState>) -> i64 {
    static START: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    START.elapsed().as_nanos() as i64
}

/// `env_len(name_ptr) -> Int`: the byte length of the named environment
/// variable's value, or -1 when unset (or not valid Unicode — matching the
/// interpreter's `std::env::var`, which errors on both). The guest sizes its
/// buffer from this, then calls `env_fill`. Linked only under an `Env` grant.
fn host_env_len(mut caller: Caller<'_, VmState>, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    if let Some(allow) = &caller.data().caps.env_allow {
        if !allow.iter().any(|k| k == &name) {
            return Err(Error::msg(format!("get_env: `{name}` is not in this Env grant's allow-list")));
        }
    }
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
    if let Some(allow) = &caller.data().caps.env_allow {
        if !allow.iter().any(|k| k == &name) {
            return Err(Error::msg(format!("get_env: `{name}` is not in this Env grant's allow-list")));
        }
    }
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
        .map(|d| d.base.clone())
        .ok_or_else(|| Error::msg(format!("invalid Dir handle {h}")))
}

/// Look up a Dir handle's entry policy (RFC-0011; `""` = unrestricted).
fn dir_policy(caller: &Caller<'_, VmState>, h: i32) -> Result<String> {
    caller
        .data()
        .dirs
        .get(h as usize)
        .map(|d| d.policy.clone())
        .ok_or_else(|| Error::msg(format!("invalid Dir handle {h}")))
}

fn dir_rights(caller: &Caller<'_, VmState>, h: i32) -> Result<FsRights> {
    caller
        .data()
        .dirs
        .get(h as usize)
        .map(|d| d.rights)
        .ok_or_else(|| Error::msg(format!("invalid Dir handle {h}")))
}

fn dir_require_read(caller: &Caller<'_, VmState>, h: i32) -> Result<()> {
    if dir_rights(caller, h)?.read {
        Ok(())
    } else {
        Err(Error::msg("this Dir capability does not grant Read"))
    }
}

fn dir_require_write(caller: &Caller<'_, VmState>, h: i32) -> Result<()> {
    if dir_rights(caller, h)?.write {
        Ok(())
    } else {
        Err(Error::msg("this Dir capability does not grant Write"))
    }
}

/// Trap unless Dir handle `h`'s entry policy admits touching `name` (RFC-0011).
/// `is_dir` is whether `name` denotes a directory entry (a traversal), so the policy's
/// `kind:` dimension can distinguish files from directories.
fn dir_guard(caller: &Caller<'_, VmState>, h: i32, name: &str, is_dir: bool) -> Result<()> {
    if witchy_caps::capabilities::dir_admits(&dir_policy(caller, h)?, name, is_dir) {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "`{name}` is not permitted by this Dir capability's entry policy"
        )))
    }
}

fn confine(r: std::result::Result<std::path::PathBuf, crate::confine::ConfineError>) -> Result<std::path::PathBuf> {
    r.map_err(|e| Error::msg(e.0))
}

/// `dir_subdir(h, name) -> handle`: attenuate to a confined subdirectory,
/// minting a new handle.
fn host_dir_subdir(mut caller: Caller<'_, VmState>, h: i32, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    dir_require_read(&caller, h)?;
    // Opening a sub-directory is a directory traversal (RFC-0011 `kind`): a `files()`
    // policy forbids it, an `ext`/empty policy does not.
    dir_guard(&caller, h, &name, true)?;
    let base = dir_base(&caller, h)?;
    let pol = dir_policy(&caller, h)?;
    let rights = dir_rights(&caller, h)?;
    let sub = confine(crate::confine::resolve(&base, &name))?;
    let dirs = &mut caller.data_mut().dirs;
    dirs.push(DirHandle { base: sub, policy: pol, rights }); // a subtree inherits policy and rights
    Ok((dirs.len() - 1) as i32)
}

/// `dir_only(h, policy) -> handle` (RFC-0011): mint a Dir handle whose entry
/// policy is the current one narrowed by `policy` (refinement only shrinks).
fn host_dir_only(mut caller: Caller<'_, VmState>, h: i32, policy_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let refine = read_wstr(mem.data(&caller), policy_ptr)?;
    dir_require_read(&caller, h)?;
    let base = dir_base(&caller, h)?;
    let narrowed = witchy_caps::capabilities::dir_only(&dir_policy(&caller, h)?, &refine);
    let rights = dir_rights(&caller, h)?;
    let dirs = &mut caller.data_mut().dirs;
    dirs.push(DirHandle { base, policy: narrowed, rights });
    Ok((dirs.len() - 1) as i32)
}

/// Look up a direct `--file` grant by ordinal while minting the root `File`
/// externref for `main`. This ordinal never crosses into guest code.
fn file_handle(caller: &Caller<'_, VmState>, f: i32) -> Result<FileHandle> {
    caller
        .data()
        .files
        .get(f as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid File grant index {f}")))
}

/// Recover the authority object carried by a guest `File` value. A valid guest
/// value is an opaque `externref` minted by `mint_file` or `dir_open/create`; there
/// is no guest-side integer handle to forge or swap.
fn file_handle_ref(caller: &Caller<'_, VmState>, f: Option<Rooted<ExternRef>>) -> Result<FileHandle> {
    let f = f.ok_or_else(|| Error::msg("File externref is null"))?;
    f.data(caller)?
        .ok_or_else(|| Error::msg("File externref has no host data"))?
        .downcast_ref::<FileHandle>()
        .cloned()
        .ok_or_else(|| Error::msg("File externref has wrong host data"))
}

/// (RFC-0005 Stage 2) `mint_file(i) -> externref` — mint an unforgeable `File`
/// capability for `--file` grant index `i`, wrapping the confined file authority
/// in a wasmtime `externref` the guest cannot fabricate or corrupt (unlike the old
/// i32 handle a linear-memory overwrite could swap). The `run` wrapper calls this
/// in declaration order to fill `main`'s `File` params. The returned reference
/// roots the backing grant for as long as the guest holds it (the GC keeps host
/// data alive while a live wasm frame references the `externref`), so no separate
/// index table survives it (§8.9).
fn host_mint_file(mut caller: Caller<'_, VmState>, i: i32) -> Result<Option<Rooted<ExternRef>>> {
    let file = file_handle(&caller, i)?;
    ExternRef::new(&mut caller, file).map(Some)
}

/// `dir_open(h, rel) -> File` (RFC-0012/RFC-0005 Stage 2): open an existing file
/// confined to Dir handle `h`, minting an unforgeable `File` externref.
/// `dir_create` is the write counterpart (the target need not exist yet).
fn host_dir_open(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    dir_require_read(&caller, h)?;
    dir_guard(&caller, h, &rel, false)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve(&base, &rel))?;
    ExternRef::new(&mut caller, FileHandle { path, rights: FsRights::new(true, false) }).map(Some)
}

/// `dir_create(h, rel) -> File`: like `dir_open` but for a not-yet-existing target
/// (confined via its parent, like `dir_write`).
fn host_dir_create(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<Option<Rooted<ExternRef>>> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    dir_require_write(&caller, h)?;
    dir_guard(&caller, h, &rel, false)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::confine::resolve_write(&base, &rel))?;
    ExternRef::new(&mut caller, FileHandle { path, rights: FsRights::new(false, true) }).map(Some)
}

/// `file_read_len(f) -> byte length`: read the confined File externref NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// Mirrors `dir_read_len`, but a `File` is a leaf so there is no path argument.
fn host_file_read_len(mut caller: Caller<'_, VmState>, f: Option<Rooted<ExternRef>>) -> Result<i32> {
    let file = file_handle_ref(&caller, f)?;
    if !file.rights.read {
        return Err(Error::msg("this File capability does not grant Read"));
    }
    let contents = std::fs::read_to_string(&file.path)
        .map_err(|e| Error::msg(format!("read failed for `{}`: {e}", file.path.display())))?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

/// `file_write(f, contents)`: write a confined File externref
/// (RFC-0012/RFC-0005 Stage 2).
fn host_file_write(
    mut caller: Caller<'_, VmState>,
    f: Option<Rooted<ExternRef>>,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let contents = read_wstr(mem.data(&caller), contents_ptr)?;
    let file = file_handle_ref(&caller, f)?;
    if !file.rights.write {
        return Err(Error::msg("this File capability does not grant Write"));
    }
    std::fs::write(&file.path, contents)
        .map_err(|e| Error::msg(format!("write failed for `{}`: {e}", file.path.display())))
}

/// `dir_read_len(h, rel) -> byte length`: read the confined file NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// A failed read traps — the interpreter errors on it too.
fn host_dir_read_len(mut caller: Caller<'_, VmState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    dir_require_read(&caller, h)?;
    dir_guard(&caller, h, &rel, false)?;
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
    if let Some(allow) = &caller.data().caps.exec_allow {
        if !allow.iter().any(|tool| tool == &path) {
            return Err(Error::msg(format!("exec: `{path}` is not in this Exec grant's allow-list")));
        }
    }
    dir_require_read(&caller, h)?;
    // (RFC-0011) exec is the sharpest right, so it takes the SAME entry-policy gate as
    // every other Dir op: a `Dir[...].only(...)` may only run a file it admits — "you
    // can only run a file you can read" was false while this was skipped.
    dir_guard(&caller, h, &path, false)?;
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
    dir_require_read(&caller, h)?;
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
    dir_require_read(&caller, h)?;
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
    dir_require_read(&caller, h)?;
    let base = dir_base(&caller, h)?;
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .map_err(|e| Error::msg(format!("list failed for `{}`: {e}", base.display())))?
        .map(|entry| {
            let entry =
                entry.map_err(|e| Error::msg(format!("list failed for `{}`: {e}", base.display())))?;
            entry.file_name().into_string().map_err(|_| {
                Error::msg(format!(
                    "list failed for `{}`: directory entry name is not valid UTF-8",
                    base.display()
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;
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

/// Process one contiguous chunk of `vm.par_map` inputs on a fresh worker VM.
type ChunkRunner<T> = fn(&Engine, &Module, bool, i32, &[T]) -> Result<Vec<T>>;

/// Fan `inputs` out across one worker VM per core (capped by the input count), each
/// processing a contiguous chunk via `run_chunk`, and gather the results IN INPUT ORDER.
/// The shared parallel engine behind both `vm.par_map` variants (scalar `i64` and buffer
/// `Vec<u8>`) — they differ only in the element type and the per-chunk runner. Empty
/// `inputs` yields an empty result with no threads.
fn par_fan_out<T: Send + Clone>(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    code_idx: i32,
    inputs: &[T],
    run_chunk: ChunkRunner<T>,
) -> Result<Vec<T>> {
    let n = inputs.len();
    let workers = std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1).min(n).max(1);
    let chunk = n.div_ceil(workers);
    let mut results: Vec<T> = Vec::with_capacity(n);
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        let mut start = 0;
        while start < n {
            let end = (start + chunk).min(n);
            let chunk_inputs = inputs[start..end].to_vec();
            handles.push(scope.spawn(move || run_chunk(engine, module, preempt, code_idx, &chunk_inputs)));
            start = end;
        }
        for h in handles {
            results.extend(h.join().map_err(|_| Error::msg("vm.par_map worker thread panicked"))??);
        }
        Ok(())
    })?;
    Ok(results)
}

/// (RFC-0032) `vm_par_map_run(xs_ptr, f_ptr) -> byte_size`: map the closure at `f_ptr`
/// over the `List(Int)` at `xs_ptr` across worker VMs (`par_fan_out` + `run_par_chunk`),
/// staging the results for `vm_par_map_write` and returning the byte size of the resulting
/// flat `List(Int)` (`[count][count x i64]`).
fn host_vm_par_map_run(mut caller: Caller<'_, VmState>, xs_ptr: i32, f_ptr: i32) -> Result<i32> {
    // Snapshot the input scalars + the closure's code index, then drop the borrow.
    let (inputs, code_idx): (Vec<i64>, i32) = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let lb = slice(data, xs_ptr, 4)?;
        let n = i32::from_le_bytes([lb[0], lb[1], lb[2], lb[3]]);
        let mut v = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let s = slice(data, xs_ptr + 4 + 8 * i, 8)?;
            v.push(i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]));
        }
        // The closure record's first word is its table (code) index.
        let cb = slice(data, f_ptr, 4)?;
        (v, i32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]))
    };
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let preempt = caller.data().preempt;
    let results = par_fan_out(&engine, &module, preempt, code_idx, &inputs, run_par_chunk)?;
    let size = 4 + 8 * results.len();
    caller.data_mut().pending_ints = Some(results);
    Ok(size as i32)
}

/// (RFC-0032) Run one chunk of a `vm.par_map` on a fresh, isolated worker VM: a new
/// `Store`/`Instance` of the same module (own linear memory), invoking the mapped
/// function by table index through the `__call_idx` trampoline for each input.
///
/// The worker is granted ZERO ambient authority — `vm.par_map`'s function is pure
/// (no capability parameters), so the only capability imports linked are the
/// authority-free staging helpers; every other host import is defined as a TRAP
/// (deny-by-omission). A worker thus physically cannot touch the filesystem, network,
/// or any other host resource, even though it shares the parent's compiled module.
/// This is RFC-0032's Tier-B isolation mechanism in miniature.
fn run_par_chunk(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    code_idx: i32,
    inputs: &[i64],
) -> Result<Vec<i64>> {
    let (mut store, instance) = sandbox_worker(engine, module, preempt)?;
    let call_idx = instance.get_typed_func::<(i32, i64), i64>(&mut store, "__call_idx")?;
    let mut out = Vec::with_capacity(inputs.len());
    for &x in inputs {
        out.push(call_idx.call(&mut store, (code_idx, x))?);
    }
    Ok(out)
}

/// The single `VmState` constructor: build a VM's host state from the capabilities it
/// is granted. The primary VM (`spawn`) and every worker VM (`spawn_worker`) go through
/// here, so the dirs/nets/files/build handle tables are derived from `caps` in exactly
/// one place. `worker_listener` is `Some` only for a `serve` pool worker.
fn vmstate_from_caps(
    id: VmId,
    caps: &Capabilities,
    limits: StoreLimits,
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    engine: &Engine,
    module: &Module,
    preempt: bool,
) -> VmState {
    let default_dir_rights = FsRights::new(caps.dir_read, caps.dir_write);
    let dirs = caps
        .dir_root
        .iter()
        .cloned()
        .chain(caps.dir_roots.iter().cloned())
        .enumerate()
        .map(|(i, p)| DirHandle {
            base: p,
            policy: String::new(),
            rights: caps.dir_rights.get(i).copied().unwrap_or(default_dir_rights),
        })
        .collect();
    let files = caps
        .file_grants
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, path)| FileHandle {
            path,
            rights: caps.file_rights.get(i).copied().unwrap_or_else(FsRights::full),
        })
        .collect();
    let nets = caps.net_allow.iter().cloned().collect();
    VmState {
        id,
        caps: caps.clone(),
        limits,
        output: Arc::new(Mutex::new(Vec::new())),
        dirs,
        files,
        pending: None,
        user_cap_fields: caps.user_cap_fields.clone(),
        pending_list: None,
        pending_ints: None,
        pending_bytes: None,
        nets,
        sockets: Vec::new(),
        listeners: Vec::new(),
        worker_listener,
        build_out: caps.build_out.clone(),
        build_read_roots: caps.build_read_roots.clone(),
        heap_objects: Vec::new(),
        rand_state: crate::rand::seed_from_env(),
        engine: engine.clone(),
        module: module.clone(),
        preempt,
    }
}

/// Instantiate a worker VM of the same `module`, granted `caps`. The single setup path
/// for all RFC-0032 worker VMs (`vm.par_map`/`with_dir`/`serve`, and the `server.serve`
/// pool): build the state, the store (with the worker epoch deadline), and the linker,
/// then instantiate. `sandboxed` defines the UNGRANTED imports as traps (deny-by-omission
/// — the par_map/with_dir/serve workers); a non-sandboxed worker (the full-capability
/// server pool) instead fails to instantiate if it needs an ungranted import, exactly
/// like the primary VM. `worker_listener` shares the primary's bound socket with a pool
/// worker.
fn spawn_worker(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    caps: &Capabilities,
    sandboxed: bool,
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    limits: StoreLimits,
) -> Result<(Store<VmState>, Instance)> {
    let state = vmstate_from_caps(0, caps, limits, worker_listener, engine, module, preempt);
    let mut store = Store::new(engine, state);
    store.limiter(|s| &mut s.limits);
    if preempt {
        // The shared engine has epoch interruption on (the primary VM's watchdog); a
        // worker pushes its deadline out of the way (it is its own short/long task).
        store.set_epoch_deadline(u64::MAX);
    }
    let mut linker: Linker<VmState> = Linker::new(engine);
    link_capability_imports(&mut linker, caps)?;
    if sandboxed {
        linker.define_unknown_imports_as_traps(module)?;
    }
    let instance = linker.instantiate(&mut store, module)?;
    Ok((store, instance))
}

/// The default zero-authority worker grant + limits for the compute/serve workers.
fn sandbox_worker(
    engine: &Engine,
    module: &Module,
    preempt: bool,
) -> Result<(Store<VmState>, Instance)> {
    spawn_worker(
        engine,
        module,
        preempt,
        &Capabilities::default(),
        true,
        None,
        StoreLimitsBuilder::new().build(),
    )
}

/// (RFC-0032) `vm_with_dir_run(dir_handle, f_ptr, input_ptr) -> byte_size`: run `f` on
/// `input` inside an isolated worker VM granted EXACTLY the `Dir` at `dir_handle` (its
/// `read`/`write` rights inherited from the parent) and NOTHING else — every other host
/// import traps. Stages the result `Bytes` (`[len][bytes]`) for `fill_pending`. This is
/// the capability-PASSING (Tier B) primitive: a sandboxed worker with attenuated authority.
fn host_vm_with_dir_run(
    mut caller: Caller<'_, VmState>,
    dir_handle: i32,
    f_ptr: i32,
    input_ptr: i32,
) -> Result<i32> {
    let (path, policy, rights, input, code_idx) = {
        let handle = caller
            .data()
            .dirs
            .get(dir_handle as usize)
            .cloned()
            .ok_or_else(|| Error::msg(format!("invalid Dir handle {dir_handle}")))?;
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let input = read_wbytes(data, input_ptr)?;
        let cb = slice(data, f_ptr, 4)?;
        (
            handle.base,
            handle.policy,
            handle.rights,
            input,
            i32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]),
        )
    };
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let preempt = caller.data().preempt;
    let result =
        run_with_dir_worker(&engine, &module, preempt, path, policy, rights, code_idx, &input)?;
    let mut staged = Vec::with_capacity(4 + result.len());
    staged.extend_from_slice(&(result.len() as i32).to_le_bytes());
    staged.extend_from_slice(&result);
    let size = staged.len() as i32;
    caller.data_mut().pending = Some(staged);
    Ok(size)
}

/// Run `f(dir, input)` on a fresh worker VM granted exactly the one `Dir`. `f` is a
/// two-argument closure (the dir handle `0` + the input `Bytes` pointer), invoked through
/// the `__call2` trampoline; the result `Bytes` is read raw out of the worker's memory.
#[allow(clippy::too_many_arguments)]
fn run_with_dir_worker(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    path: std::path::PathBuf,
    policy: String,
    rights: FsRights,
    code_idx: i32,
    input: &[u8],
) -> Result<Vec<u8>> {
    let caps = Capabilities {
        dir_root: Some(path),
        dir_read: rights.read,
        dir_write: rights.write,
        dir_rights: vec![rights],
        ..Capabilities::default()
    };
    let (mut store, instance) =
        spawn_worker(engine, module, preempt, &caps, true, None, StoreLimitsBuilder::new().build())?;
    // Attach the granted Dir's entry policy (RFC-0011) to handle 0.
    if let Some(d) = store.data_mut().dirs.get_mut(0) {
        d.policy = policy;
    }
    let galloc = instance.get_typed_func::<i32, i32>(&mut store, "__galloc")?;
    let call2 = instance.get_typed_func::<(i32, i64, i64), i64>(&mut store, "__call2")?;
    let mem = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| Error::msg("vm.with_dir worker VM exports no `memory`"))?;
    let total = 4 + input.len() as i32;
    let iptr = galloc.call(&mut store, total)?;
    let mut buf = Vec::with_capacity(total as usize);
    buf.extend_from_slice(&(input.len() as i32).to_le_bytes());
    buf.extend_from_slice(input);
    mem.write(&mut store, iptr as usize, &buf)
        .map_err(|e| Error::msg(format!("writing vm.with_dir input into worker: {e}")))?;
    let rptr = call2.call(&mut store, (code_idx, 0i64, iptr as i64))?;
    read_wbytes(mem.data(&store), rptr as i32)
}

/// (RFC-0032) `vm_serve_run(init_ptr, requests_ptr, handler_ptr) -> byte_size`: a stateful
/// SERVICE on a single long-lived isolated worker VM. The worker is created once and
/// processes the request stream IN ORDER, threading the accumulator `state` through
/// `handler(state, request) -> new_state` and emitting each new state as the response.
/// This is the deterministic, parity-safe realization of cross-VM channels: a worker that
/// processes a message stream with persistent state, lock-step (no nondeterministic
/// interleaving), so the interpreter's sequential scan reproduces the result exactly.
fn host_vm_serve_run(
    mut caller: Caller<'_, VmState>,
    init_ptr: i32,
    requests_ptr: i32,
    handler_ptr: i32,
) -> Result<i32> {
    let (init, requests, code_idx) = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let init = read_wbytes(data, init_ptr)?;
        let requests = read_wbytes_list(data, requests_ptr)?;
        let cb = slice(data, handler_ptr, 4)?;
        (init, requests, i32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]))
    };
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let preempt = caller.data().preempt;
    let responses = run_serve_worker(&engine, &module, preempt, code_idx, &init, &requests)?;
    let n = responses.len();
    let size = 4 + 8 * n + responses.iter().map(|b| 4 + b.len()).sum::<usize>();
    caller.data_mut().pending_bytes = Some(responses);
    Ok(size as i32)
}

/// Drive a `vm.serve` worker: one zero-authority instance, `state` (a `Bytes` pointer)
/// kept live in the worker's memory and threaded through `handler(state, req)` via the
/// `__call2` trampoline; each new state is read out as that request's response.
fn run_serve_worker(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    code_idx: i32,
    init: &[u8],
    requests: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>> {
    let (mut store, instance) = sandbox_worker(engine, module, preempt)?;
    let galloc = instance.get_typed_func::<i32, i32>(&mut store, "__galloc")?;
    let call2 = instance.get_typed_func::<(i32, i64, i64), i64>(&mut store, "__call2")?;
    let mem = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| Error::msg("vm.serve worker VM exports no `memory`"))?;
    // Helper: copy a byte buffer into the worker as a `[len][bytes]` value, return ptr.
    let copy_in = |store: &mut Store<VmState>, bytes: &[u8]| -> Result<i32> {
        let total = 4 + bytes.len() as i32;
        let p = galloc.call(&mut *store, total)?;
        let mut buf = Vec::with_capacity(total as usize);
        buf.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
        buf.extend_from_slice(bytes);
        mem.write(&mut *store, p as usize, &buf)
            .map_err(|e| Error::msg(format!("writing vm.serve buffer into worker: {e}")))?;
        Ok(p)
    };
    let mut state_ptr = copy_in(&mut store, init)?;
    let mut responses = Vec::with_capacity(requests.len());
    for req in requests {
        let req_ptr = copy_in(&mut store, req)?;
        state_ptr = call2.call(&mut store, (code_idx, state_ptr as i64, req_ptr as i64))? as i32;
        responses.push(read_wbytes(mem.data(&store), state_ptr)?);
    }
    Ok(responses)
}

/// (RFC-0032) `vm_par_map_bytes_run(xs_ptr, f_ptr) -> byte_size`: the `Bytes` variant of
/// `vm.par_map`. Identical to the `String` variant but the payload is kept as RAW bytes
/// (`Vec<u8>`, no UTF-8 decode), so arbitrary binary survives the cross-VM round-trip.
fn host_vm_par_map_bytes_run(mut caller: Caller<'_, VmState>, xs_ptr: i32, f_ptr: i32) -> Result<i32> {
    let (inputs, code_idx): (Vec<Vec<u8>>, i32) = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let inputs = read_wbytes_list(data, xs_ptr)?;
        let cb = slice(data, f_ptr, 4)?;
        (inputs, i32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]))
    };
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let preempt = caller.data().preempt;
    let results = par_fan_out(&engine, &module, preempt, code_idx, &inputs, run_par_chunk_bytes)?;
    let size = 4 + 8 * results.len() + results.iter().map(|b| 4 + b.len()).sum::<usize>();
    caller.data_mut().pending_bytes = Some(results);
    Ok(size as i32)
}

/// One chunk of a `String`/`Bytes` `vm.par_map`: copy each input buffer into the worker
/// (`__galloc` + `[len][bytes]`), invoke `f` by table index (`__call_idx`), and read the
/// result buffer back out RAW (no UTF-8 decode) — a witchy `String` is valid-UTF-8 `Bytes`,
/// so binary and text cross the worker boundary through the same path, unchanged.
fn run_par_chunk_bytes(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    code_idx: i32,
    inputs: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>> {
    let (mut store, instance) = sandbox_worker(engine, module, preempt)?;
    let galloc = instance.get_typed_func::<i32, i32>(&mut store, "__galloc")?;
    let call_idx = instance.get_typed_func::<(i32, i64), i64>(&mut store, "__call_idx")?;
    let mem = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| Error::msg("vm.par_map worker VM exports no `memory`"))?;
    let mut out = Vec::with_capacity(inputs.len());
    for bytes in inputs {
        let total = 4 + bytes.len() as i32;
        let wptr = galloc.call(&mut store, total)?;
        let mut buf = Vec::with_capacity(total as usize);
        buf.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
        buf.extend_from_slice(bytes);
        mem.write(&mut store, wptr as usize, &buf)
            .map_err(|e| Error::msg(format!("writing par_map input bytes into worker: {e}")))?;
        let rptr = call_idx.call(&mut store, (code_idx, wptr as i64))?;
        out.push(read_wbytes(mem.data(&store), rptr as i32)?);
    }
    Ok(out)
}

/// (RFC-0032) `vm_par_map_bytes_write(base_ptr)`: lay the staged `Bytes` results out at
/// `base_ptr` as a guest `List(Bytes)` — `[count][count x i64 ptr][.[len][bytes]…]`,
/// the same structure as `List(String)` (see `write_pending_list`).
fn host_vm_par_map_bytes_write(mut caller: Caller<'_, VmState>, base_ptr: i32) -> Result<()> {
    let items = caller
        .data_mut()
        .pending_bytes
        .take()
        .ok_or_else(|| Error::msg("vm_par_map_bytes_write called with nothing staged"))?;
    let n = items.len();
    let mut buf = Vec::with_capacity(4 + 8 * n + items.iter().map(|b| 4 + b.len()).sum::<usize>());
    buf.extend_from_slice(&(n as i32).to_le_bytes());
    let payload_start = base_ptr as i64 + 4 + 8 * n as i64;
    let mut offset = 0i64;
    for b in &items {
        buf.extend_from_slice(&(payload_start + offset).to_le_bytes());
        offset += 4 + b.len() as i64;
    }
    for b in &items {
        buf.extend_from_slice(&(b.len() as i32).to_le_bytes());
        buf.extend_from_slice(b);
    }
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, base_ptr as usize, &buf)
        .map_err(|e| Error::msg(format!("writing par_map bytes into guest memory: {e}")))
}

/// (RFC-0032) `vm_par_map_write(base_ptr)`: lay the staged `vm.par_map` results out
/// at `base_ptr` in the guest's `List(Int)` format — `[count][count x i64]`.
fn host_vm_par_map_write(mut caller: Caller<'_, VmState>, base_ptr: i32) -> Result<()> {
    let vals = caller
        .data_mut()
        .pending_ints
        .take()
        .ok_or_else(|| Error::msg("vm_par_map_write called with nothing staged"))?;
    let n = vals.len();
    let mut buf = Vec::with_capacity(4 + 8 * n);
    buf.extend_from_slice(&(n as i32).to_le_bytes());
    for v in &vals {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, base_ptr as usize, &buf)
        .map_err(|e| Error::msg(format!("writing par_map results into guest memory: {e}")))
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
    dir_require_write(&caller, h)?;
    dir_guard(&caller, h, &rel, false)?;
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
    dir_require_write(&caller, h)?;
    dir_guard(&caller, h, &rel, false)?;
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

/// `dir_make_dir(h, name)`: create a confined subdirectory (idempotent). Creating a
/// directory is a directory op (RFC-0011 `kind`): a `files()` policy forbids it.
fn host_dir_make_dir(mut caller: Caller<'_, VmState>, h: i32, name_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    dir_require_write(&caller, h)?;
    dir_guard(&caller, h, &name, true)?;
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

fn host_build_env_len(mut caller: Caller<'_, VmState>, _h: i32, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let allow = caller
        .data()
        .caps
        .env_allow
        .as_ref()
        .ok_or_else(|| Error::msg("get_build_env: no BuildEnv grant"))?;
    if !allow.iter().any(|k| k == &name) {
        return Err(Error::msg(format!(
            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
        )));
    }
    match std::env::var(&name) {
        Ok(v) => Ok(v.len() as i32),
        Err(_) => Ok(-1),
    }
}

fn host_build_env_fill(
    mut caller: Caller<'_, VmState>,
    _h: i32,
    name_ptr: i32,
    out_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let allow = caller
        .data()
        .caps
        .env_allow
        .as_ref()
        .ok_or_else(|| Error::msg("get_build_env: no BuildEnv grant"))?;
    if !allow.iter().any(|k| k == &name) {
        return Err(Error::msg(format!(
            "get_build_env: `{name}` is not in this BuildEnv grant's allow-list"
        )));
    }
    let value = std::env::var(&name).unwrap_or_default();
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| Error::msg(format!("writing build env value into guest memory: {e}")))
}

fn host_build_fetch_len(
    mut caller: Caller<'_, VmState>,
    _h: i32,
    host_ptr: i32,
    path_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let host = read_wstr(data, host_ptr)?;
    let path = read_wstr(data, path_ptr)?;
    let allow = caller
        .data()
        .caps
        .build_net_allow
        .as_ref()
        .ok_or_else(|| Error::msg("fetch_build: no BuildNet grant"))?;
    if !allow.iter().any(|h| h == &host) {
        return Err(Error::msg(format!(
            "fetch_build: `{host}` is not in this BuildNet grant's allow-list"
        )));
    }

    use std::io::{Read as _, Write as _};
    let mut sock = std::net::TcpStream::connect(host.as_str())
        .map_err(|e| Error::msg(format!("fetch_build: cannot connect to `{host}`: {e}")))?;
    let hostname = host.split(':').next().unwrap_or(host.as_str());
    let req = format!("GET {path} HTTP/1.1\r\nHost: {hostname}\r\nConnection: close\r\n\r\n");
    sock.write_all(req.as_bytes())
        .map_err(|e| Error::msg(format!("fetch_build: sending to `{host}`: {e}")))?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw)
        .map_err(|e| Error::msg(format!("fetch_build: reading from `{host}`: {e}")))?;
    let text = String::from_utf8_lossy(&raw);
    let body = match text.split_once("\r\n\r\n") {
        Some((_, b)) => b.to_string(),
        None => text.into_owned(),
    };
    let len = body.len() as i32;
    caller.data_mut().pending = Some(body.into_bytes());
    Ok(len)
}

fn host_build_exec_run(
    mut caller: Caller<'_, VmState>,
    _h: i32,
    tool_ptr: i32,
    input_ptr: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let tool = read_wstr(data, tool_ptr)?;
    let input = read_wstr(data, input_ptr)?;
    let allow = caller
        .data()
        .caps
        .exec_allow
        .as_ref()
        .ok_or_else(|| Error::msg("run_tool: no BuildExec grant"))?;
    if !allow.iter().any(|t| t == &tool) {
        return Err(Error::msg(format!(
            "run_tool: `{tool}` is not in this BuildExec grant's allow-list"
        )));
    }

    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&tool)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("run_tool: cannot start `{tool}`: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| Error::msg(format!("run_tool: writing to `{tool}` stdin: {e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::msg(format!("run_tool: `{tool}` failed: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!("run_tool: `{tool}` exited with {}", out.status)));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let len = stdout.len() as i32;
    caller.data_mut().pending = Some(stdout.into_bytes());
    Ok(len)
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

/// `net_resolve_size(h, host_ptr) -> bytes` (RFC-0020): resolve `host` to its current IP
/// literals NOW, stage them, and report the `List(String)` byte size the guest reserves
/// (then `write_pending_list` lays it out — the same two-phase protocol as `dir_list`). NO
/// allowlist filtering: the program inspects the IPs and `connect_pinned` re-checks the
/// chosen one, so resolve adds no authority beyond `connect`. Empty list => resolve failed.
fn host_net_resolve_size(mut caller: Caller<'_, VmState>, _h: i32, host_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let ips = crate::net::resolve_ips(&host);
    let size = 4 + 8 * ips.len() + ips.iter().map(|s| 4 + s.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(ips);
    Ok(size as i32)
}

/// `net_connect_pinned(h, ip_ptr, host_ptr, port, secure) -> Socket handle` (RFC-0020):
/// dial the EXACT `ip:port` — no DNS — while presenting `host` as the TLS SNI / `Host`.
/// The Net allowlist is still enforced on `ip` (via `resolve_admitted`, which on a literal
/// IP resolves to itself), so a pin can never exceed the capability's confinement. This is
/// what closes the rebinding TOCTOU: the inspected IP is the dialed IP, no re-resolution.
fn host_net_connect_pinned(
    mut caller: Caller<'_, VmState>,
    h: i32,
    ip_ptr: i32,
    host_ptr: i32,
    port: i64,
    secure: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let ip = read_wstr(mem.data(&caller), ip_ptr)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let allow = net_allow(&caller, h)?;
    let ip_port = crate::net::authority(&ip, port);
    let targets = witchy_caps::capabilities::resolve_admitted(&allow, &ip_port)
        .map_err(|e| Error::msg(format!("connect_pinned: {e}")))?;
    let host_port = crate::net::authority(&host, port);
    let stream = crate::net::dial(&targets, secure != 0, &host_port)
        .map_err(|e| Error::msg(format!("connect_pinned to `{ip_port}` failed: {e}")))?;
    let sockets = &mut caller.data_mut().sockets;
    sockets.push(std::io::BufReader::new(stream));
    Ok((sockets.len() - 1) as i32)
}

/// `net_try_connect_pinned(...) -> Socket handle | -1`: like `net_connect_pinned` but a
/// failed connection yields `-1` (witchy wraps it as `Option(Socket)`'s `None`). A
/// capability violation — a pinned IP outside the allowlist — still traps.
fn host_net_try_connect_pinned(
    mut caller: Caller<'_, VmState>,
    h: i32,
    ip_ptr: i32,
    host_ptr: i32,
    port: i64,
    secure: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let ip = read_wstr(mem.data(&caller), ip_ptr)?;
    let host = read_wstr(mem.data(&caller), host_ptr)?;
    let allow = net_allow(&caller, h)?;
    let ip_port = crate::net::authority(&ip, port);
    let targets = witchy_caps::capabilities::resolve_admitted(&allow, &ip_port)
        .map_err(|e| Error::msg(format!("try_connect_pinned: {e}")))?;
    let host_port = crate::net::authority(&host, port);
    match crate::net::dial(&targets, secure != 0, &host_port) {
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
    // A `serve` pool worker reuses the primary's already-bound listener (so all
    // workers `accept` from one socket) instead of binding the address again.
    let shared = caller.data().worker_listener.clone();
    let entry = match shared {
        Some(entry) => entry,
        None => (
            std::sync::Arc::new(
                std::net::TcpListener::bind(&addr)
                    .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?,
            ),
            None,
        ),
    };
    let listeners = &mut caller.data_mut().listeners;
    listeners.push(entry);
    Ok((listeners.len() - 1) as i32)
}

/// (RFC-0060) `net_listen_tls(h, addr, cert_pem, key_handle) -> Listener handle`:
/// bind an allowlisted address for HTTPS. The rustls `ServerConfig` is built ONCE
/// here, from the certificate-chain PEM (public, guest-supplied text) and the
/// private key resolved BY HANDLE from the host secret table — the key bytes
/// never cross into guest memory (the signing-key pattern, `secret_seed_bytes`).
/// Malformed or mismatched cert/key is a loud listen-time error; accepts on the
/// returned listener handshake host-side and yield ordinary Socket handles.
fn host_net_listen_tls(
    mut caller: Caller<'_, VmState>,
    h: i32,
    addr_ptr: i32,
    cert_ptr: i32,
    key_handle: i32,
) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let cert_pem = read_wstr(mem.data(&caller), cert_ptr)?;
    let allow = net_allow(&caller, h)?;
    if !witchy_caps::capabilities::net_allows(&allow, &addr) {
        bail!("listen: `{addr}` is not permitted by this Net capability");
    }
    let key_bytes = secret_seed_bytes(&caller.data().caps, key_handle)?;
    // A pool worker reuses the primary's bound listener + its TLS config wholesale;
    // the primary builds the config (loud startup failure on bad key material).
    let shared = caller.data().worker_listener.clone();
    let entry = match shared {
        Some(entry) => entry,
        None => {
            let config = crate::net::server_tls_config(&cert_pem, &key_bytes).map_err(Error::msg)?;
            (
                std::sync::Arc::new(
                    std::net::TcpListener::bind(&addr)
                        .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?,
                ),
                Some(config),
            )
        }
    };
    let listeners = &mut caller.data_mut().listeners;
    listeners.push(entry);
    Ok((listeners.len() - 1) as i32)
}

/// `net_accept(listener) -> Socket handle`: block for a client connection. On a
/// TLS listener (RFC-0060) the handshake is completed host-side before the Socket
/// is minted; a failed handshake (plaintext client, bad ClientHello) drops that
/// connection and keeps accepting — connection weather, not a program error.
fn host_net_accept(mut caller: Caller<'_, VmState>, lid: i32) -> Result<i32> {
    // Clone the `Arc` out so the blocking `accept` doesn't hold a borrow of the VM
    // state — and so a shared (pool) listener is accepted concurrently by every worker.
    let (listener, tls) = caller
        .data()
        .listeners
        .get(lid as usize)
        .cloned()
        .ok_or_else(|| Error::msg("invalid listener"))?;
    loop {
        let (stream, _peer) = listener
            .accept()
            .map_err(|e| Error::msg(format!("accept failed: {e}")))?;
        let stream: Box<dyn Stream> = match &tls {
            None => Box::new(stream),
            Some(config) => match crate::net::accept_tls(config.clone(), stream) {
                Ok(tls_stream) => tls_stream,
                Err(_) => continue,
            },
        };
        let state = caller.data_mut();
        state.sockets.push(std::io::BufReader::new(stream));
        return Ok((state.sockets.len() - 1) as i32);
    }
}

/// (RFC-0032) `serve_pool(listener_handle)`: the `server.serve` worker pool. On the
/// PRIMARY VM it spawns one worker VM per remaining core, each re-running the program
/// (rebuilding the same routes with the same capabilities) but SHARING this bound
/// listener. Every worker `accept`s from the one socket and the kernel load-balances
/// connections, so the server uses all cores. A pool worker (its `worker_listener`
/// already set) is a no-op here — only the primary spawns the pool.
fn host_serve_pool(caller: Caller<'_, VmState>, listener_handle: i32) -> Result<()> {
    if caller.data().worker_listener.is_some() {
        return Ok(());
    }
    let Some(listener) = caller.data().listeners.get(listener_handle as usize).cloned() else {
        return Ok(());
    };
    let workers = std::thread::available_parallelism().map(|p| p.get()).unwrap_or(1).max(1);
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let caps = caller.data().caps.clone();
    let preempt = caller.data().preempt;
    for _ in 1..workers {
        let (engine, module, caps, listener) =
            (engine.clone(), module.clone(), caps.clone(), listener.clone());
        std::thread::spawn(move || {
            if let Err(e) = run_server_worker(&engine, &module, &caps, preempt, listener) {
                eprintln!("[serve worker] exited: {e}");
            }
        });
    }
    Ok(())
}

/// Run one `server.serve` pool worker: a fresh VM of the same program + capabilities,
/// marked with the shared listener (and its TLS config, RFC-0060), re-running `run`
/// (main) so it rebuilds the routes and enters the accept loop on the shared socket.
/// Runs until the process exits.
fn run_server_worker(
    engine: &Engine,
    module: &Module,
    caps: &Capabilities,
    preempt: bool,
    listener: (std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>),
) -> Result<()> {
    // Full capabilities (NOT sandboxed — a server worker is a full copy of the program,
    // like the primary), a generous memory budget, and the shared listener.
    let (mut store, instance) = spawn_worker(
        engine,
        module,
        preempt,
        caps,
        false,
        Some(listener),
        StoreLimitsBuilder::new().memory_size(16384 * 64 * 1024).build(),
    )?;
    let run = instance.get_typed_func::<(), ()>(&mut store, "run")?;
    run.call(&mut store, ())
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
    let state = caller.data_mut();
    let sock = socket_of(state, sid)?;
    let raw = crate::net::read_line_capped(sock).map_err(|e| Error::msg(format!("recv failed: {e}")))?;
    let line = String::from_utf8_lossy(&raw);
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
    // (SEC-035) Cap the read so a peer streaming without EOF can't OOM the host. Read one
    // byte past the cap to detect overflow, then fail closed.
    sock.by_ref()
        .take(crate::net::MAX_RECV_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| Error::msg(format!("recv failed: {e}")))?;
    if buf.len() as u64 > crate::net::MAX_RECV_BYTES {
        bail!("recv_all exceeded the {}-byte cap", crate::net::MAX_RECV_BYTES);
    }
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
/// (RFC-0005) No magic-index fallback: `handle` must name a real granted entry in
/// `caps.secrets`. The signing key, when granted, is a normal `"signing"` entry in
/// that table (populated at every grant site), so it resolves like any other secret —
/// there is no special case a fabricated handle `0` could exploit.
fn secret_seed_bytes(caps: &Capabilities, handle: i32) -> Result<Vec<u8>> {
    if let Some(grant) = usize::try_from(handle).ok().and_then(|h| caps.secrets.get(h)) {
        return Ok(grant.bytes.clone());
    }
    Err(Error::msg("crypto: no secret at that handle (none granted?)"))
}

/// Whether the secret at `handle` was granted **use-only** (RFC-0060) — usable by
/// handle but not revealable. A handle that names no granted secret is treated as
/// not use-only; the consuming op's own `secret_seed_bytes` call raises the "no
/// secret" error.
fn secret_use_only(caps: &Capabilities, handle: i32) -> bool {
    usize::try_from(handle)
        .ok()
        .and_then(|h| caps.secrets.get(h))
        .is_some_and(|grant| grant.use_only)
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
/// host-side bytes. (RFC-0005) The signing key is a normal `"signing"` entry in
/// `caps.secrets` (populated at every grant site), so it is found by name like any
/// other — no magic-index fallback.
fn host_secretstore_lookup(mut caller: Caller<'_, VmState>, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let caps = &caller.data().caps;
    match caps.secrets.iter().position(|grant| grant.name == name) {
        Some(i) => Ok(i as i32),
        None => Ok(-1),
    }
}

/// `crypto_reveal_len(key) -> i32`: reveal the secret at handle `key` as a string
/// (its raw bytes, lossy UTF-8 — through the same native registry the interpreter
/// uses), stage it for `fill_pending`, and report its byte length. For value
/// secrets handed to an external sink; the bytes only cross into the guest here.
fn host_crypto_reveal_len(mut caller: Caller<'_, VmState>, key: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let bytes = secret_seed_bytes(&caller.data().caps, key)?;
    // (RFC-0060) A use-only secret is consumable by handle but never revealable.
    if secret_use_only(&caller.data().caps, key) {
        bail!("{}", witchy_caps::capabilities::USE_ONLY_SECRET_REVEAL_ERROR);
    }
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
fn read_wstr(data: &[u8], ptr: i32) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_wbytes(data, ptr)?).into_owned())
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

/// Read a guest `Bytes` — `[len: i32][bytes…]` — as RAW bytes (no UTF-8 decode).
fn read_wbytes(data: &[u8], ptr: i32) -> Result<Vec<u8>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    Ok(slice(data, ptr + 4, len)?.to_vec())
}

/// Read a guest `List(Bytes)` — same `[count][i64 ptr…]` layout as `List(String)`, but
/// each element is read as raw bytes.
fn read_wbytes_list(data: &[u8], ptr: i32) -> Result<Vec<Vec<u8>>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let mut out = Vec::with_capacity((len.max(0) as usize).min(data.len() / 8));
    for i in 0..len {
        let slot = slice(data, ptr + 4 + 8 * i, 8)?;
        let elem = i64::from_le_bytes([
            slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7],
        ]);
        out.push(read_wbytes(data, elem as i32)?);
    }
    Ok(out)
}

/// (RFC-0023) Record a checked allocation and poison its trailing redzone. The guest's
/// checked allocators call this right after writing an object spanning `[start,end)`;
/// the host fills `[end, end+HEAP_REDZONE)` with `HEAP_POISON` and remembers the object
/// so [`Vm::heap_sweep`] can later prove the redzone was never overwritten. Out-of-range
/// arguments are ignored — this is a debug oracle, never an authority boundary.
fn host_heap_register(mut caller: Caller<'_, VmState>, start: i32, end: i32) -> Result<()> {
    if start < 0 || end < start {
        return Ok(());
    }
    let (s, e) = (start as u32, end as u32);
    let mem = memory_of(&mut caller)?;
    let rz_start = e as usize;
    let rz_end = rz_start + HEAP_REDZONE;
    let data = mem.data_mut(&mut caller);
    if rz_end <= data.len() {
        data[rz_start..rz_end].fill(HEAP_POISON);
    }
    caller.data_mut().heap_objects.push((s, e));
    Ok(())
}

/// (RFC-0023) Reset-aware reclaim: drop every registered object whose redzone reaches
/// `addr`, since the space at/above `addr` is about to be legitimately reused. Two
/// callers, both passing the low edge of the reclaimed range: the checked `$ensure`
/// passes the current `$heap` (so reuse by *any* allocator after a `$heap = wm` reset is
/// covered — even uninstrumented ones — with no codegen hook on the reset); the
/// `region:` pointer copy-out passes its watermark `wm` *before* sliding the result down
/// over the body's allocations (a raw `memory.copy` that bypasses `$ensure`). The
/// object's body may still be live below `addr`; we only stop guarding its redzone.
fn host_heap_frontier(mut caller: Caller<'_, VmState>, addr: i32) -> Result<()> {
    let frontier = addr.max(0) as u32;
    // Keep only objects whose entire redzone lies strictly below the frontier; evict any
    // whose redzone `[oe, oe+rz)` reaches the frontier, since a write at/above it will
    // legitimately reuse that space.
    caller
        .data_mut()
        .heap_objects
        .retain(|&(_, oe)| oe as u64 + HEAP_REDZONE as u64 <= frontier as u64);
    Ok(())
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
