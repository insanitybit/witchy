//! Wasmtime sandbox and capability host for compiled Witchy programs.
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
    bail, Cache, CacheConfig, Caller, Config, Engine, Error, Extern, Linker, Memory,
    Module, Result, Store, StoreLimits, StoreLimitsBuilder,
};
use witchy_wir::layout::HEAP_REDZONE;

mod compiler;
mod host;

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

/// Directory for validated Binaryen output. This cache stores ordinary wasm,
/// never serialized native code: every hit still enters Wasmtime through the
/// safe `Module::new` validation path and then benefits from Wasmtime's own
/// compilation cache configured below.
fn optimized_wasm_cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    let dir = base.join("witchy").join("optimized-wasm");
    std::fs::create_dir_all(&dir).ok()?;
    // The cache is not a security boundary (`Module::new` validates every hit),
    // but owner-only permissions also prevent another local user from replacing
    // a valid optimized module with different, still-valid wasm.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

const OPT_CACHE_MAGIC: &[u8; 8] = b"WYOPT001";
const OPT_CACHE_HEADER_LEN: usize = OPT_CACHE_MAGIC.len() + 32 + 32 + 8;

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

/// Bind cached optimized wasm to both its original input and its own contents.
/// The envelope detects truncation, corruption, and accidental cache-file swaps
/// before Wasmtime sees the payload. `Module::new` remains the authoritative
/// validator and the memory-safety boundary.
fn encode_optimized_wasm(input_hash: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let payload_len = u64::try_from(payload.len()).expect("a wasm buffer length fits in u64");
    let mut out = Vec::with_capacity(OPT_CACHE_HEADER_LEN + payload.len());
    out.extend_from_slice(OPT_CACHE_MAGIC);
    out.extend_from_slice(&input_hash);
    out.extend_from_slice(&sha256(payload));
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(payload);
    debug_assert_eq!(out.len(), OPT_CACHE_HEADER_LEN + payload.len());
    out
}

fn decode_optimized_wasm<'a>(expected_input_hash: &[u8; 32], bytes: &'a [u8]) -> Option<&'a [u8]> {
    let header = bytes.get(..OPT_CACHE_HEADER_LEN)?;
    if header.get(..OPT_CACHE_MAGIC.len())? != OPT_CACHE_MAGIC {
        return None;
    }
    let input_start = OPT_CACHE_MAGIC.len();
    let payload_hash_start = input_start + 32;
    let len_start = payload_hash_start + 32;
    if header.get(input_start..payload_hash_start)? != expected_input_hash {
        return None;
    }
    let expected_payload_hash: [u8; 32] = header.get(payload_hash_start..len_start)?.try_into().ok()?;
    let payload_len = u64::from_le_bytes(header.get(len_start..OPT_CACHE_HEADER_LEN)?.try_into().ok()?);
    let payload_len = usize::try_from(payload_len).ok()?;
    let end = OPT_CACHE_HEADER_LEN.checked_add(payload_len)?;
    if end != bytes.len() {
        return None;
    }
    let payload = bytes.get(OPT_CACHE_HEADER_LEN..end)?;
    (sha256(payload) == expected_payload_hash).then_some(payload)
}

fn optimized_wasm_cache_path(input_hash: &[u8; 32]) -> Option<std::path::PathBuf> {
    let hex: String = input_hash.iter().map(|b| format!("{b:02x}")).collect();
    optimized_wasm_cache_dir().map(|dir| dir.join(format!("{hex}.wasm-cache")))
}

/// (RFC-0034 L1) Run Binaryen `wasm-opt -O2` on the module — a heavier optimizer
/// than Cranelift's Speed tier (GVN, inlining, DCE, local CSE). It runs ONLY on
/// the cold compile path below (successful output is cached as validated wasm),
/// so warm runs skip Binaryen. Optional + graceful: returns the input unchanged if `wasm-opt` isn't
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

/// Build a `Module`. On the cacheable (non-preempt) engine, successful Binaryen
/// output is cached as validated wasm; every cold and warm path enters Wasmtime
/// through safe `Module::new`. Wasmtime's configured compilation cache handles
/// native-code reuse internally. `cacheable` is false on the preempt engine,
/// whose differing config must not share artifacts.
fn build_module(engine: &Engine, opt_wasm: &[u8], cacheable: bool) -> Result<Module> {
    if !cacheable {
        return Module::new(engine, opt_wasm);
    }
    let input_hash = sha256(opt_wasm);
    let path = binaryen_enabled()
        .then(|| optimized_wasm_cache_path(&input_hash))
        .flatten();
    if let Some(path) = &path {
        if let Ok(envelope) = std::fs::read(path) {
            if let Some(cached_wasm) = decode_optimized_wasm(&input_hash, &envelope) {
                if let Ok(module) = Module::new(engine, cached_wasm) {
                    return Ok(module);
                }
            }
        }
    }
    let optimized = binaryen_optimize(opt_wasm);
    let mut optimized_valid = true;
    let module = match Module::new(engine, optimized.as_ref()) {
        Ok(module) => module,
        Err(err) if matches!(optimized, std::borrow::Cow::Owned(_)) => {
            if std::env::var_os("WIRDIAG").is_some() {
                eprintln!("WIRBAIL wasm-opt-output-rejected: {err}");
            }
            optimized_valid = false;
            Module::new(engine, opt_wasm)?
        }
        Err(err) => return Err(err),
    };
    if let (Some(path), std::borrow::Cow::Owned(optimized_wasm)) = (&path, &optimized)
        && optimized_valid
    {
        let envelope = encode_optimized_wasm(input_hash, optimized_wasm);
        // Write-then-rename so a reader never sees a partial envelope. The temp
        // name carries a random suffix so concurrent compilers do not share a
        // temporary path; atomic rename publishes the complete validated wasm.
        {
            let mut rnd = [0u8; 8];
            getrandom::fill(&mut rnd).ok();
            let suffix: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
            let tmp = path.with_extension(format!("{suffix}.tmp"));
            if std::fs::write(&tmp, envelope).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
    Ok(module)
}

pub type VmId = u32;

/// One named secret backing the `SecretStore` capability: its name, its raw bytes
/// (kept host-side — the guest only ever holds an opaque externref), and whether
/// it is **use-only** (RFC-0060). A use-only secret is still consumable by
/// reference (`crypto.sign`, TLS serving), but `crypto.reveal` on it errors — so
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
    /// Optional per-key restriction for ordinary `Env` host reads. `None`
    /// preserves the coarse grant; `Some(keys)` refuses every unnamed key.
    pub env_allow: Option<Vec<String>>,
    /// The directory subtree backing the first root `Dir` grant.
    /// `None` denies the filesystem entirely; the `dir_read`/`dir_write` flags
    /// pick which operation families are linked within it.
    pub dir_root: Option<std::path::PathBuf>,
    /// Additional root `Dir` grants, for a `main` that takes several `Dir`
    /// parameters. The generated wrapper mints each parameter from its grant
    /// ordinal; the ordinal never becomes the guest's `Dir` value.
    pub dir_roots: Vec<std::path::PathBuf>,
    /// Per-Dir-grant rights, aligned with `dir_root` followed by `dir_roots`.
    /// Empty means "use the coarse `dir_read`/`dir_write` grant for each Dir grant",
    /// preserving older callers that grant one uniform root.
    pub dir_rights: Vec<FsRights>,
    /// Enable authority-free in-memory test capability constructors such as
    /// `testing.mock_dir`. This links read-only Dir/File operation imports for
    /// mock externrefs, but it does not mint or expose any real filesystem root.
    pub test_mocks: bool,
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
    /// executable is named + confined through a `Dir[Read]` capability, so `exec`
    /// without filesystem read is useless — but the flag is tracked separately so
    /// `Exec` appears as its own authority. See rfcs/0004-self-hosted-cli.md.
    pub exec: bool,
    /// Optional per-tool restriction for native subprocess execution. `None`
    /// preserves the ordinary coarse `Exec` grant; `Some(tools)` is used by
    /// BuildExec and refuses every tool not named here.
    pub exec_allow: Option<Vec<String>>,
    /// The `host:port` allowlist backing the root `Net` capability.
    /// `None` denies the network entirely; the verb flags below pick which
    /// operation families are linked within it.
    pub net_allow: Option<Vec<String>>,
    /// Additional root `Net` grants, aligned after `net_allow`, for entrypoints
    /// with several independently bound network parameters.
    pub net_grants: Vec<Vec<String>>,
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
    /// Named secrets backing the `SecretStore` capability. The guest never sees an
    /// index into this vector: `secretstore_lookup` returns an opaque `Secret`
    /// externref, and the root signing-key `Secret` is minted by `mint_secret`.
    /// The `signing` entry (from `--signing-key`) is kept here so
    /// `SecretStore.require("signing")` and bare `Secret` share one identity.
    pub secrets: Vec<SecretGrant>,
    /// The confined output directory backing a build step's `BuildOut`
    /// capability — where `write_out` may write generated source, and nowhere
    /// else. `None` denies build-time writes entirely.
    pub build_out: Option<std::path::PathBuf>,
    /// The confined read roots backing a build step's `BuildRead` capability —
    /// `read_build` resolves a path against the first root that holds it. Empty
    /// denies build-time reads.
    pub build_read_roots: Vec<std::path::PathBuf>,
    /// Immutable snapshot backing `BuildEnv`. Map membership is the allow-list;
    /// `None` means the named variable was allowed but unset. Keeping the values
    /// in the grant makes the size/fill protocol deterministic and avoids mutable
    /// process-global environment access while a VM is running.
    pub build_env: Option<std::collections::BTreeMap<String, Option<String>>>,
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
enum DirBacking {
    Fs(std::path::PathBuf),
    Mock {
        root: String,
        files: Arc<std::collections::BTreeMap<String, String>>,
    },
}

#[derive(Clone, Debug)]
struct DirAuthority {
    backing: DirBacking,
    policy: String,
    rights: FsRights,
}

#[derive(Clone, Debug)]
enum FileBacking {
    Fs(std::path::PathBuf),
    Mock {
        path: String,
        files: Arc<std::collections::BTreeMap<String, String>>,
    },
}

#[derive(Clone, Debug)]
struct FileAuthority {
    backing: FileBacking,
    rights: FsRights,
}

use crate::net::Stream;

#[derive(Clone, Debug)]
struct NetAuthority {
    allow: Vec<String>,
}

#[derive(Clone)]
struct SocketResource {
    stream: Arc<Mutex<std::io::BufReader<Box<dyn Stream>>>>,
}

#[derive(Clone)]
struct ListenerResource {
    listener: std::sync::Arc<std::net::TcpListener>,
    tls: Option<crate::net::ServerTlsConfig>,
}

#[derive(Clone, Debug)]
struct SecretMaterial {
    bytes: Vec<u8>,
    use_only: bool,
}

impl SecretMaterial {
    fn from_grant(grant: &SecretGrant) -> Self {
        Self {
            bytes: grant.bytes.clone(),
            use_only: grant.use_only,
        }
    }
}

/// Host-side state owned by a spawned VM's `Store`: its capability grant, output
/// buffer, and the host-side root-capability/build grant material. One state type
/// means one set of capability host functions.
const HEAP_POISON: u8 = 0xDB;

pub struct VmState {
    id: VmId,
    caps: Capabilities,
    /// The compiler services exposed to trusted programs; injected from above
    /// so `runtime/compiler.rs` depends on the interface, not the
    /// implementation.
    compiler_services: Arc<dyn compiler::CompilerServices>,
    pub(crate) limits: StoreLimits,
    /// Everything the VM has printed (via the `print`/`print_int`
    /// capabilities), so the host can observe a compiled program's output.
    pub(crate) output: Arc<Mutex<Vec<String>>>,
    /// Root `Dir` grants awaiting wrapper-local `mint_dir(i)`. A live guest `Dir`
    /// is an externref carrying a cloned `DirAuthority`, not this index.
    dirs: Vec<DirAuthority>,
    /// Direct `File` grants awaiting wrapper-local `mint_file(i)`. A live guest
    /// `File` is an externref carrying a cloned `FileAuthority`, not this index.
    files: Vec<FileAuthority>,
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
    /// Root `Net` grants awaiting wrapper-local `mint_net(i)`. Live `Net`,
    /// `Socket`, and `Listener` values are externrefs carrying host-side resources,
    /// not guest-forgeable indices into these grants.
    nets: Vec<NetAuthority>,
    /// (RFC-0032) When this VM is a `serve` POOL WORKER, the shared listener (plus
    /// its TLS config) it must reuse instead of binding its own. `listen` returns
    /// this; `serve_pool` sees it set and does NOT spawn another pool (only the
    /// primary spawns workers).
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    /// A build step's confined output directory (`BuildOut`) and read roots
    /// (`BuildRead`) — host-side. Build capabilities are zero-representation at
    /// the host ABI; import linking, not a guest handle, carries the authority.
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
    aborted: bool,
}

impl Vm {
    /// Call the VM's exported `run` function to completion.
    pub fn run(&mut self) -> Result<()> {
        if self.aborted {
            bail!("an aborted Witchy VM cannot be run again");
        }
        let run = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, "run")?;
        if let Err(error) = run.call(&mut self.store, ()) {
            // A trap does not run guest cleanup. Make that terminal: retaining the
            // host handle is harmless, but no later call may observe a partially
            // unwound stack or abandoned guest roots.
            self.aborted = true;
            return Err(self.contextualize_run_error(error));
        }
        if let Err(error) = self.heap_sweep() {
            self.aborted = true;
            return Err(error);
        }
        if std::env::var_os("WITCHY_REGION_STATS").is_some_and(|v| v == "1") {
            if let Some(bytes) = self.region_copy_bytes() {
                eprintln!("region copy-out: {bytes} byte(s)");
            }
        }
        Ok(())
    }

    /// Attach the source site published immediately before a host-backed
    /// operation. Guest-routed aborts already carry their complete diagnostic;
    /// capability host failures arrive as a bare core and are completed here.
    fn contextualize_run_error(&mut self, error: Error) -> Error {
        let core = error.root_cause().to_string();
        if core.starts_with("runtime error: ") {
            return error;
        }
        let site = self
            .instance
            .get_global(&mut self.store, "__witchy_diagnostic_site")
            .and_then(|global| global.get(&mut self.store).i64())
            .unwrap_or(0);
        if site == 0 {
            return error;
        }
        let (func_ptr, line) = witchy_syntax::diag::unpack_site(site);
        let func = if func_ptr == 0 {
            String::new()
        } else {
            self.instance
                .get_memory(&mut self.store, "memory")
                .and_then(|memory| read_wstr(memory.data(&self.store), func_ptr as i32).ok())
                .unwrap_or_default()
        };
        Error::msg(witchy_syntax::diag::runtime_error(&func, line, &core))
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

    fn i64_counter(&mut self, name: &str) -> Option<i64> {
        self.instance
            .get_global(&mut self.store, name)
            .and_then(|g| g.get(&mut self.store).i64())
    }

    pub fn rc_alloc_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_alloc_calls")
    }

    pub fn bump_alloc_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_bump_alloc_calls")
    }

    pub fn rc_reuse_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_reuse_calls")
    }

    pub fn rc_free_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_rc_free_calls")
    }

    pub fn region_rewind_calls(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_region_rewind_calls")
    }

    pub fn extract_searches(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_searches")
    }

    pub fn extract_key_comparisons(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_key_comparisons")
    }

    pub fn extract_copied_bytes(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_copied_bytes")
    }

    pub fn extract_retains(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_retains")
    }

    pub fn extract_drops(&mut self) -> Option<i64> {
        self.i64_counter("__witchy_extract_drops")
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
    /// Injected into every VM's state; the Wasmtime compiler-service adapters
    /// call this interface rather than the compiler implementation directly.
    compiler_services: Arc<dyn compiler::CompilerServices>,
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
        // for the `*_cap` ABI) at their defaults, and explicitly keep reference
        // types / function references / GC on for the externref capability core
        // (RFC-0005 steps 2–6): nullable cap values emit `ref.null extern` /
        // `ref.is_null`, and named cap-carrying records emit GC refs. Cranelift's
        // Spectre mitigations (heap + table access) are ON by default and stay on — do
        // NOT set `signals_based_traps(false)`, which would force them off.
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
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
            compiler_services: Arc::new(compiler::RegistryCompilerServices),
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
        // `wasm-opt` runs inside `build_module` only on an optimized-wasm cache miss;
        // every hit and miss still enters Wasmtime through safe `Module::new`.
        let module = build_module(&self.engine, wasm.as_ref(), !self.preempt)?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let state = vmstate_from_caps(
            id,
            &caps,
            limits,
            None,
            &self.engine,
            &module,
            self.preempt,
            self.compiler_services.clone(),
        );

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
        Ok(Vm { store, instance, aborted: false })
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

    host::console::link_abort(linker)?;
    // --- capability wiring: only granted host functions are defined ---
    if caps.print {
        host::console::link_print(linker)?;
    }
    if caps.print_int {
        host::console::link_print_values(linker)?;
    }
    if caps.rand {
        host::clock_env_rand::link_rand(linker)?;
    }
    if caps.clock {
        host::clock_env_rand::link_clock(linker)?;
    }
    if caps.env {
        host::clock_env_rand::link_env(linker)?;
    }
    // The Dir family is linked per RIGHT, so a module compiled against a
    // write operation cannot even instantiate under a read-only grant.
    let has_real_dir = caps.dir_root.is_some() || !caps.dir_roots.is_empty();
    let dir_read = caps.dir_read || caps.dir_rights.iter().any(|r| r.read);
    let dir_write = caps.dir_write || caps.dir_rights.iter().any(|r| r.write);
    if caps.test_mocks {
        host::filesystem::link_mock_dir(linker)?;
    }
    if has_real_dir {
        host::filesystem::link_mint_dir(linker)?;
    }
    let has_readable_dir = (has_real_dir && dir_read) || caps.test_mocks;
    if has_readable_dir {
        host::filesystem::link_dir_read(linker)?;
    }
    if has_real_dir && dir_write {
        host::filesystem::link_dir_write(linker)?;
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
    if has_readable_dir || direct_file_read {
        host::filesystem::link_file_read(linker)?;
    }
    if (has_real_dir && dir_write) || direct_file_write {
        host::filesystem::link_file_write(linker)?;
    }
    // (RFC-0005 Stage 2) The `run` wrapper mints each `--file` `main` param as an
    // `externref` via `mint_file`; only reachable when a direct File grant exists.
    if has_file_grants {
        host::filesystem::link_mint_file(linker)?;
    }
    // The Exec capability: spawn a confined subprocess. The executable is named
    // through a `Dir[Read]` capability, so `exec_run` reuses `dir_base`/`confine`.
    if caps.exec {
        host::filesystem::link_exec(linker)?;
    }
    let net = caps.net_allow.is_some() || !caps.net_grants.is_empty();
    // (RFC-0005 Stage 3) The `run` wrapper mints each root `Net` param as an
    // `externref` via `mint_net`; only reachable when a Net grant exists.
    if net {
        host::network::link_mint(linker)?;
    }
    // The Net family, linked per VERB right. Socket I/O carries no authority
    // of its own (a socket is only obtainable through a granted connect or
    // accept), so it is linked under either verb.
    if net && caps.net_connect {
        host::network::link_connect(linker)?;
    }
    if net && caps.net_listen {
        host::network::link_listen(linker)?;
        host::vm_worker::link_serve_pool(linker)?;
    }
    if net && (caps.net_connect || caps.net_listen) {
        host::network::link_socket_io(linker)?;
    }
    if caps.signing_key.is_some() {
        host::secret::link_mint(linker)?;
    }
    // Any granted Secret (root signing key or named SecretStore secret) can be
    // consumed by the keyed crypto helpers while bytes stay host-side.
    if caps.signing_key.is_some() || !caps.secrets.is_empty() {
        host::crypto::link_keyed(linker)?;
    }
    // The build-time capabilities. Like Dir, each is linked only when granted
    // — a build step compiled against `write_out` cannot even instantiate
    // without a `BuildOut` grant, and `read_build` without a `BuildRead` one.
    if caps.build_out.is_some() {
        host::build::link_out(linker)?;
    }
    if !caps.build_read_roots.is_empty() {
        host::build::link_read(linker)?;
    }
    if caps.build_env.is_some() {
        host::build::link_env(linker)?;
    }
    if caps.build_net_allow.is_some() {
        host::build::link_fetch(linker)?;
    }
    if caps.exec_allow.is_some() {
        host::build::link_exec(linker)?;
    }
    host::staging::link_staging(linker)?;
    host::vm_worker::link_vm(linker)?;
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
    host::crypto::link_pure(linker)?;
    host::secret::link_lookup(linker)?;
    host::crypto::link_reveal(linker)?;
    // The compiler's footprint analyses are pure functions of their source
    // arguments — the toolchain exposed to witchy, same registry bridge.
    compiler::link(linker)?;
    host::staging::link_pure(linker)?;
    Ok(())
}

fn confine(r: std::result::Result<std::path::PathBuf, crate::confine::ConfineError>) -> Result<std::path::PathBuf> {
    r.map_err(|e| Error::msg(e.0))
}

/// The single `VmState` constructor: build a VM's host state from the capabilities it
/// is granted. The primary VM (`spawn`) and every worker VM (`spawn_worker`) go through
/// here, so the root Dir/File/Net grant material and build grants are derived
/// from `caps` in exactly one place. `worker_listener` is `Some` only for a
/// `serve` pool worker.
#[allow(clippy::too_many_arguments)]
fn vmstate_from_caps(
    id: VmId,
    caps: &Capabilities,
    limits: StoreLimits,
    worker_listener: Option<(std::sync::Arc<std::net::TcpListener>, Option<crate::net::ServerTlsConfig>)>,
    engine: &Engine,
    module: &Module,
    preempt: bool,
    compiler_services: Arc<dyn compiler::CompilerServices>,
) -> VmState {
    let default_dir_rights = FsRights::new(caps.dir_read, caps.dir_write);
    let dirs = caps
        .dir_root
        .iter()
        .cloned()
        .chain(caps.dir_roots.iter().cloned())
        .enumerate()
        .map(|(i, p)| DirAuthority {
            backing: DirBacking::Fs(p),
            policy: String::new(),
            rights: caps.dir_rights.get(i).copied().unwrap_or(default_dir_rights),
        })
        .collect();
    let files = caps
        .file_grants
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, path)| FileAuthority {
            backing: FileBacking::Fs(path),
            rights: caps.file_rights.get(i).copied().unwrap_or_else(FsRights::full),
        })
        .collect();
    let nets = caps
        .net_allow
        .iter()
        .cloned()
        .chain(caps.net_grants.iter().cloned())
        .map(|allow| NetAuthority { allow })
        .collect();
    VmState {
        id,
        caps: caps.clone(),
        compiler_services,
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

/// Read a guest `List((String, String))`. Tuple values are laid out as
/// `[tag/len: i32][field0: i64][field1: i64]`; each field is a string pointer.
fn read_wstr_pair_list(data: &[u8], ptr: i32) -> Result<Vec<(String, String)>> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let mut out = Vec::with_capacity((len.max(0) as usize).min(data.len() / 8));
    for i in 0..len {
        let slot = slice(data, ptr + 4 + 8 * i, 8)?;
        let pair_ptr = i64::from_le_bytes([
            slot[0], slot[1], slot[2], slot[3], slot[4], slot[5], slot[6], slot[7],
        ]) as i32;
        let first_slot = slice(data, pair_ptr + 4, 8)?;
        let second_slot = slice(data, pair_ptr + 12, 8)?;
        let first = i64::from_le_bytes([
            first_slot[0],
            first_slot[1],
            first_slot[2],
            first_slot[3],
            first_slot[4],
            first_slot[5],
            first_slot[6],
            first_slot[7],
        ]) as i32;
        let second = i64::from_le_bytes([
            second_slot[0],
            second_slot[1],
            second_slot[2],
            second_slot[3],
            second_slot[4],
            second_slot[5],
            second_slot[6],
            second_slot[7],
        ]) as i32;
        out.push((read_wstr(data, first)?, read_wstr(data, second)?));
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
