//! Witchy runtime spike.
//!
//! Each actor runs in its own wasmtime `Store` (its own linear memory, its own
//! stack). An actor can only reach the outside world through host functions
//! that the runtime explicitly links into *that actor's* `Linker`. Those host
//! functions ARE the capabilities: if a capability was not granted, the import
//! is simply absent and the actor fails to instantiate. There is no ambient
//! authority anywhere.

use std::collections::{HashMap, VecDeque};
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

pub type ActorId = u32;

/// The set of capabilities granted to an actor at spawn time. Each `true` flag
/// causes the corresponding host function to be linked into the actor's VM.
/// Everything defaults to denied.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// May write to the host's stdout via `witchy.print`.
    pub print: bool,
    /// May deliver a message to another actor's mailbox via `witchy.send`.
    pub send: bool,
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
    /// May read within `dir_root` (read/exists/is_dir/list/subdir).
    pub dir_read: bool,
    /// May write within `dir_root` (write/make_dir).
    pub dir_write: bool,
    /// The `host:port` allowlist backing the root `Net` capability (handle 0).
    /// `None` denies the network entirely; the verb flags below pick which
    /// operation families are linked within it.
    pub net_allow: Option<Vec<String>>,
    /// May dial out (`connect`/`restrict`) to allowlisted addresses.
    pub net_connect: bool,
    /// May bind and accept (`listen`/`accept`) on allowlisted addresses.
    pub net_listen: bool,
}

impl Capabilities {
    /// No authority at all.
    pub fn none() -> Self {
        Self::default()
    }
}

/// A single actor's inbound message queue. Shared (`Arc`) so other actors'
/// `send` host calls can push into it.
type Mailbox = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// The shared registry of every actor's mailbox. This is the *only* shared
/// state between actors, and it is reachable only through the `send`/`recv`
/// host functions — never through guest memory.
#[derive(Default)]
struct Mailboxes {
    boxes: Mutex<HashMap<ActorId, Mailbox>>,
}

impl Mailboxes {
    fn get_or_create(&self, id: ActorId) -> Mailbox {
        self.boxes.lock().unwrap().entry(id).or_default().clone()
    }

    /// Deliver `msg` to `target`'s mailbox. Returns false if no such actor.
    fn deliver(&self, target: ActorId, msg: Vec<u8>) -> bool {
        match self.boxes.lock().unwrap().get(&target) {
            Some(mb) => {
                mb.lock().unwrap().push_back(msg);
                true
            }
            None => false,
        }
    }
}

/// Host-side state owned by each actor's `Store`.
pub struct ActorState {
    id: ActorId,
    caps: Capabilities,
    mailbox: Mailbox,
    mailboxes: Arc<Mailboxes>,
    limits: StoreLimits,
    /// Everything the actor has printed (via the `print`/`print_int`
    /// capabilities), so the host can observe a compiled program's output.
    output: Arc<Mutex<Vec<String>>>,
    /// The `Dir` capability handle table: index 0 is the granted root, and each
    /// `subdir` mints a new confined entry. Guest code only ever holds the i32
    /// index — the paths live host-side, so a module cannot forge a directory.
    dirs: Vec<std::path::PathBuf>,
    /// A host->guest transfer staged by a size-probing call (`dir_read_len`,
    /// `net_recv_*_len`, ...) and consumed by the matching `fill_pending`, so
    /// the data is read once with no time-of-check/time-of-use gap.
    pending: Option<Vec<u8>>,
    /// A staged directory listing (`dir_list_size` -> `dir_list_write`).
    pending_list: Option<Vec<String>>,
    /// The `Net` capability handle table: index 0 is the granted allowlist,
    /// and each `restrict` mints a narrower entry — host-side, unforgeable.
    nets: Vec<Vec<String>>,
    /// Open sockets, indexed by the guest's i32 Socket handles.
    sockets: Vec<std::io::BufReader<std::net::TcpStream>>,
    /// Listening server sockets, indexed by the guest's i32 Listener handles.
    listeners: Vec<std::net::TcpListener>,
}

/// A spawned actor: an isolated VM plus the entrypoint we can drive.
pub struct Actor {
    pub id: ActorId,
    store: Store<ActorState>,
    instance: wasmtime::Instance,
}

impl Actor {
    /// Call the actor's exported `run` function to completion.
    pub fn run(&mut self) -> Result<()> {
        let run = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, "run")?;
        run.call(&mut self.store, ())?;
        Ok(())
    }

    /// Invoke a no-argument exported message handler (used by compiled actors).
    pub fn invoke(&mut self, handler: &str) -> Result<()> {
        let func = self
            .instance
            .get_typed_func::<(), ()>(&mut self.store, handler)?;
        func.call(&mut self.store, ())?;
        Ok(())
    }

    /// Everything this actor has printed so far, in order. (Used by tests to
    /// assert a compiled program's behavior end to end.)
    #[allow(dead_code)]
    pub fn output(&self) -> Vec<String> {
        self.store.data().output.lock().unwrap().clone()
    }
}

/// The runtime owns the wasm engine and the shared mailbox registry, and hands
/// out actor ids.
pub struct Runtime {
    engine: Engine,
    mailboxes: Arc<Mailboxes>,
    next_id: ActorId,
    preempt: bool,
}

impl Runtime {
    /// A runtime whose actors can be preempted by the scheduler advancing the
    /// engine epoch (M4). Epoch interruption makes the JIT insert a check at
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
        // Epoch-based interruption lets the scheduler preempt a runaway actor
        // (exercised in M4). It is only worth its per-backedge cost when a
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
            mailboxes: Arc::new(Mailboxes::default()),
            next_id: 1,
            preempt,
        })
    }

    /// Spawn an actor from WAT/wasm source, granting it exactly `caps` and
    /// capping its linear memory at `memory_pages_max` 64KiB pages.
    ///
    /// The capability check is structural: a granted host function is linked,
    /// an ungranted one is absent, so an actor that imports an ungranted
    /// capability fails right here at `instantiate`.
    pub fn spawn(
        &mut self,
        wasm: impl AsRef<[u8]>,
        caps: Capabilities,
        memory_pages_max: usize,
    ) -> Result<Actor> {
        let id = self.next_id;
        let module = Module::new(&self.engine, wasm)?;

        let mailbox = self.mailboxes.get_or_create(id);
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_pages_max * 64 * 1024)
            .build();
        let dirs = caps.dir_root.iter().cloned().collect();
        let nets = caps.net_allow.iter().cloned().collect();
        let state = ActorState {
            id,
            caps: caps.clone(),
            mailbox,
            mailboxes: Arc::clone(&self.mailboxes),
            limits,
            output: Arc::new(Mutex::new(Vec::new())),
            dirs,
            pending: None,
            pending_list: None,
            nets,
            sockets: Vec::new(),
            listeners: Vec::new(),
        };

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        // Deadline of 1 epoch; we never advance the engine epoch during a
        // normal run, so this is never tripped. M4 advances it to preempt.
        // Only meaningful when the engine has epoch interruption enabled.
        if self.preempt {
            store.set_epoch_deadline(1);
        }

        let mut linker: Linker<ActorState> = Linker::new(&self.engine);
        // --- capability wiring: only granted host functions are defined ---
        if caps.print {
            linker.func_wrap("witchy", "print", host_print)?;
        }
        if caps.send {
            linker.func_wrap("witchy", "send", host_send)?;
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
            linker.func_wrap("witchy", "dir_list_write", host_dir_list_write)?;
        }
        if caps.dir_root.is_some() && caps.dir_write {
            linker.func_wrap("witchy", "dir_write", host_dir_write)?;
            linker.func_wrap("witchy", "dir_make_dir", host_dir_make_dir)?;
        }
        // The Net family, linked per VERB right. Socket I/O carries no authority
        // of its own (a socket is only obtainable through a granted connect or
        // accept), so it is linked under either verb.
        let net = caps.net_allow.is_some();
        if net && caps.net_connect {
            linker.func_wrap("witchy", "net_restrict", host_net_restrict)?;
            linker.func_wrap("witchy", "net_connect", host_net_connect)?;
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
        // `fill_pending` only writes out data already staged by a granted size
        // call — it carries no authority of its own, so it is always available.
        linker.func_wrap("witchy", "fill_pending", host_fill_pending)?;
        // `recv` is intrinsic: reading your *own* mailbox is not authority over
        // anyone else, so every actor may do it.
        linker.func_wrap("witchy", "recv", host_recv)?;
        // Native-stdlib functions are pure (no authority), so they're always
        // available — the same `crypto` module the interpreter exposes, here as a
        // host import that bridges to the shared `native` registry.
        linker.func_wrap("witchy", "crypto.ed25519_verify", host_ed25519_verify)?;
        linker.func_wrap("witchy", "crypto.sha256", host_sha256)?;
        // Float -> string formatting is pure; done in the host so it is byte-
        // identical to the interpreter's `Display` (no float formatter in WAT).
        linker.func_wrap("witchy", "float_to_str", host_float_to_str)?;
        // hex/base64 transforms are pure; bridged to the same native registry the
        // interpreter uses (byte-for-byte parity, no byte-level work in WAT).
        linker.func_wrap("witchy", "encoding", host_encoding)?;

        // Ungranted capability imports are rejected here.
        let instance = linker.instantiate(&mut store, &module)?;

        // Only commit the id once the actor actually came up.
        self.next_id += 1;
        Ok(Actor { id, store, instance })
    }

    /// Run an actor, but preempt it if it runs longer than `budget`. A watchdog
    /// advances the engine epoch once the budget elapses; the actor traps at
    /// its next loop back-edge or call. This is how the scheduler reclaims a
    /// runaway or malicious actor that refuses to yield.
    pub fn run_with_budget(&self, actor: &mut Actor, budget: Duration) -> Result<()> {
        let engine = self.engine.clone();
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(budget);
            engine.increment_epoch();
        });
        let result = actor.run();
        watchdog.join().ok();
        result
    }
}

// ---------------------------------------------------------------------------
// Host functions = capabilities. Each reads/writes the *calling* actor's own
// linear memory via `Caller`; none can touch another actor's memory.
// ---------------------------------------------------------------------------

fn host_print(mut caller: Caller<'_, ActorState>, ptr: i32, len: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let bytes = slice(data, ptr, len)?;
    let text = String::from_utf8_lossy(bytes);
    let id = caller.data().id;
    if !caller.data().caps.quiet {
        print!("[actor {id}] {text}");
    }
    caller
        .data()
        .output
        .lock()
        .unwrap()
        .push(text.trim_end_matches('\n').to_string());
    Ok(())
}

fn host_print_int(caller: Caller<'_, ActorState>, n: i64) -> Result<()> {
    if !caller.data().caps.quiet {
        println!("[actor {}] {n}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(n.to_string());
    Ok(())
}

fn host_print_float(caller: Caller<'_, ActorState>, x: f64) -> Result<()> {
    // `f64::to_string` is Rust's `{}` Display — the same formatting the
    // interpreter uses for a Float (Value::Float Display), so the two backends
    // agree on float output.
    if !caller.data().caps.quiet {
        println!("[actor {}] {x}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(x.to_string());
    Ok(())
}

fn host_send(mut caller: Caller<'_, ActorState>, target: i32, ptr: i32, len: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let (data, state) = mem.data_and_store_mut(&mut caller);
    let msg = slice(data, ptr, len)?.to_vec();
    if !state.mailboxes.deliver(target as ActorId, msg) {
        bail!("send to unknown actor id {target}");
    }
    Ok(())
}

fn host_recv(mut caller: Caller<'_, ActorState>, ptr: i32, cap: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let msg = caller.data().mailbox.lock().unwrap().pop_front();
    let Some(msg) = msg else {
        return Ok(-1); // mailbox empty
    };
    if msg.len() > cap as usize {
        bail!(
            "recv buffer too small: message is {} bytes, buffer is {cap}",
            msg.len()
        );
    }
    mem.write(&mut caller, ptr as usize, &msg)
        .map_err(|e| Error::msg(format!("writing received message into actor memory: {e}")))?;
    Ok(msg.len() as i32)
}

/// `crypto.ed25519_verify(pk, msg, sig) -> Bool`, bridged to the shared native
/// registry (the same implementation the interpreter uses). Each argument is a
/// pointer to a witchy string header (`[i32 len][bytes]`).
fn host_ed25519_verify(
    mut caller: Caller<'_, ActorState>,
    pk: i32,
    msg: i32,
    sig: i32,
) -> Result<i32> {
    use crate::interpreter::Value;
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
fn host_float_to_str(mut caller: Caller<'_, ActorState>, x: f64, out_ptr: i32) -> Result<i32> {
    let s = format!("{x}");
    let bytes = s.into_bytes();
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing float string into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

fn host_sha256(mut caller: Caller<'_, ActorState>, in_ptr: i32, out_ptr: i32) -> Result<()> {
    use crate::interpreter::Value;
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

/// `encoding.*(op, in_header_ptr, out_data_ptr) -> byte length`: read the input
/// string, run the selected hex/base64 transform through the shared native
/// registry (the same implementation the interpreter uses, so the backends agree
/// byte-for-byte), write the result bytes at `out_data_ptr`, and return their
/// length. The guest reserves a sufficient buffer (`2*len + slack`) beforehand.
/// `op`: 0 = hex_encode, 1 = hex_decode, 2 = base64_encode, 3 = base64_decode.
fn host_encoding(mut caller: Caller<'_, ActorState>, op: i32, in_ptr: i32, out_ptr: i32) -> Result<i32> {
    use crate::interpreter::Value;
    let name = match op {
        0 => "encoding.hex_encode",
        1 => "encoding.hex_decode",
        2 => "encoding.base64_encode",
        3 => "encoding.base64_decode",
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
/// the interpreter's `now(Clock)` produces. Linked only when the actor was
/// granted a `Clock` capability.
fn host_now(_caller: Caller<'_, ActorState>) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `env_len(name_ptr) -> Int`: the byte length of the named environment
/// variable's value, or -1 when unset (or not valid Unicode — matching the
/// interpreter's `std::env::var`, which errors on both). The guest sizes its
/// buffer from this, then calls `env_fill`. Linked only under an `Env` grant.
fn host_env_len(mut caller: Caller<'_, ActorState>, name_ptr: i32) -> Result<i32> {
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
fn host_env_fill(mut caller: Caller<'_, ActorState>, name_ptr: i32, out_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let value = std::env::var(&name).unwrap_or_default();
    mem.write(&mut caller, out_ptr as usize, value.as_bytes())
        .map_err(|e| Error::msg(format!("writing env value into guest memory: {e}")))
}

// --- the Dir capability family ---
//
// A guest `Dir` value is an i32 HANDLE into the actor's host-side path table
// (`ActorState::dirs`); the paths never enter guest memory, so a module cannot
// forge or widen one. Handle 0 is the granted root. Every operation resolves
// through the SAME `resolve`/`resolve_write` confinement the interpreter uses
// (lexical `..`/absolute rejection + symlink-aware canonicalization), so the
// two backends agree on exactly which paths are reachable.

/// Look up a Dir handle's base path (trap on a forged/out-of-range handle).
fn dir_base(caller: &Caller<'_, ActorState>, h: i32) -> Result<std::path::PathBuf> {
    caller
        .data()
        .dirs
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Dir handle {h}")))
}

fn confine(r: std::result::Result<std::path::PathBuf, crate::interpreter::RuntimeError>) -> Result<std::path::PathBuf> {
    r.map_err(|e| Error::msg(e.message))
}

/// `dir_subdir(h, name) -> handle`: attenuate to a confined subdirectory,
/// minting a new handle.
fn host_dir_subdir(mut caller: Caller<'_, ActorState>, h: i32, name_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let base = dir_base(&caller, h)?;
    let sub = confine(crate::interpreter::resolve(&base, &name))?;
    let dirs = &mut caller.data_mut().dirs;
    dirs.push(sub);
    Ok((dirs.len() - 1) as i32)
}

/// `dir_read_len(h, rel) -> byte length`: read the confined file NOW, stage its
/// bytes, and report the length; the guest allocates and calls `fill_pending`.
/// A failed read traps — the interpreter errors on it too.
fn host_dir_read_len(mut caller: Caller<'_, ActorState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::interpreter::resolve(&base, &rel))?;
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| Error::msg(format!("read failed for `{}`: {e}", path.display())))?;
    let len = contents.len() as i32;
    caller.data_mut().pending = Some(contents.into_bytes());
    Ok(len)
}

/// `fill_pending(out_ptr)`: write the bytes staged by the matching size call.
fn host_fill_pending(mut caller: Caller<'_, ActorState>, out_ptr: i32) -> Result<()> {
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
fn host_dir_exists(mut caller: Caller<'_, ActorState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let ok = crate::interpreter::resolve(&base, &rel)
        .map(|p| p.exists())
        .unwrap_or(false);
    Ok(ok as i32)
}

/// `dir_is_dir(h, rel) -> bool`: total, like `dir_exists`.
fn host_dir_is_dir(mut caller: Caller<'_, ActorState>, h: i32, rel_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let rel = read_wstr(mem.data(&caller), rel_ptr)?;
    let base = dir_base(&caller, h)?;
    let ok = crate::interpreter::resolve(&base, &rel)
        .map(|p| p.is_dir())
        .unwrap_or(false);
    Ok(ok as i32)
}

/// `dir_list_size(h) -> bytes`: read the directory NOW (sorted names, matching
/// the interpreter), stage the listing, and report the total byte size of the
/// witchy `List(String)` structure the guest must reserve.
fn host_dir_list_size(mut caller: Caller<'_, ActorState>, h: i32) -> Result<i32> {
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

/// `dir_list_write(base_ptr)`: lay the staged listing out at `base_ptr` in the
/// guest's own list format — `[count][count x i64 slots][string objects...]`,
/// each slot holding the absolute guest pointer of its `[len][bytes]` string.
fn host_dir_list_write(mut caller: Caller<'_, ActorState>, base_ptr: i32) -> Result<()> {
    let names = caller
        .data_mut()
        .pending_list
        .take()
        .ok_or_else(|| Error::msg("dir_list_write called with nothing staged"))?;
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
    mut caller: Caller<'_, ActorState>,
    h: i32,
    rel_ptr: i32,
    contents_ptr: i32,
) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let rel = read_wstr(data, rel_ptr)?;
    let contents = read_wstr(data, contents_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::interpreter::resolve_write(&base, &rel))?;
    std::fs::write(&path, contents)
        .map_err(|e| Error::msg(format!("write failed for `{}`: {e}", path.display())))
}

/// `dir_make_dir(h, name)`: create a confined subdirectory (idempotent).
fn host_dir_make_dir(mut caller: Caller<'_, ActorState>, h: i32, name_ptr: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let base = dir_base(&caller, h)?;
    let path = confine(crate::interpreter::resolve_write(&base, &name))?;
    std::fs::create_dir_all(&path)
        .map_err(|e| Error::msg(format!("make_dir failed for `{}`: {e}", path.display())))
}

// --- the Net capability family ---
//
// Same shape as Dir: a guest `Net` is an i32 handle into the actor's host-side
// allowlist table (`restrict` mints narrower entries); `Socket`/`Listener` are
// handles into host-side connection tables. The allowlist check is the SAME
// exact-match rule the interpreter applies, so the backends agree on which
// addresses are reachable.

fn net_allow(caller: &Caller<'_, ActorState>, h: i32) -> Result<Vec<String>> {
    caller
        .data()
        .nets
        .get(h as usize)
        .cloned()
        .ok_or_else(|| Error::msg(format!("invalid Net handle {h}")))
}

/// `net_restrict(h, addr) -> handle`: attenuate to a single allowlisted address.
fn host_net_restrict(mut caller: Caller<'_, ActorState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    if !allow.contains(&addr) {
        bail!("restrict: `{addr}` is not in this Net capability");
    }
    let nets = &mut caller.data_mut().nets;
    nets.push(vec![addr]);
    Ok((nets.len() - 1) as i32)
}

/// `net_connect(h, addr) -> Socket handle`: dial an allowlisted address.
fn host_net_connect(mut caller: Caller<'_, ActorState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    if !allow.contains(&addr) {
        bail!("connect: `{addr}` is not permitted by this Net capability");
    }
    let stream = std::net::TcpStream::connect(&addr)
        .map_err(|e| Error::msg(format!("connect to `{addr}` failed: {e}")))?;
    let sockets = &mut caller.data_mut().sockets;
    sockets.push(std::io::BufReader::new(stream));
    Ok((sockets.len() - 1) as i32)
}

/// `net_listen(h, addr) -> Listener handle`: bind an allowlisted address.
fn host_net_listen(mut caller: Caller<'_, ActorState>, h: i32, addr_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let addr = read_wstr(mem.data(&caller), addr_ptr)?;
    let allow = net_allow(&caller, h)?;
    if !allow.contains(&addr) {
        bail!("listen: `{addr}` is not permitted by this Net capability");
    }
    let listener = std::net::TcpListener::bind(&addr)
        .map_err(|e| Error::msg(format!("listen on `{addr}` failed: {e}")))?;
    let listeners = &mut caller.data_mut().listeners;
    listeners.push(listener);
    Ok((listeners.len() - 1) as i32)
}

/// `net_accept(listener) -> Socket handle`: block for a client connection.
fn host_net_accept(mut caller: Caller<'_, ActorState>, lid: i32) -> Result<i32> {
    let state = caller.data_mut();
    let listener = state
        .listeners
        .get(lid as usize)
        .ok_or_else(|| Error::msg("invalid listener"))?;
    let (stream, _peer) = listener
        .accept()
        .map_err(|e| Error::msg(format!("accept failed: {e}")))?;
    state.sockets.push(std::io::BufReader::new(stream));
    Ok((state.sockets.len() - 1) as i32)
}

fn socket_of(
    state: &mut ActorState,
    sid: i32,
) -> Result<&mut std::io::BufReader<std::net::TcpStream>> {
    state
        .sockets
        .get_mut(sid as usize)
        .ok_or_else(|| Error::msg("invalid socket"))
}

/// `net_send_line(sock, s)`: write the string and a trailing newline.
fn host_net_send_line(mut caller: Caller<'_, ActorState>, sid: i32, line_ptr: i32) -> Result<()> {
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
fn host_net_send_bytes(mut caller: Caller<'_, ActorState>, sid: i32, ptr: i32) -> Result<()> {
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
fn host_net_recv_line_len(mut caller: Caller<'_, ActorState>, sid: i32) -> Result<i32> {
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
fn host_net_recv_all_len(mut caller: Caller<'_, ActorState>, sid: i32) -> Result<i32> {
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
fn host_net_recv_bytes_len(mut caller: Caller<'_, ActorState>, sid: i32, n: i64) -> Result<i32> {
    use std::io::Read;
    let state = caller.data_mut();
    let sock = socket_of(state, sid)?;
    let want = n.max(0) as usize;
    let mut buf = vec![0u8; want];
    let mut read = 0;
    while read < want {
        match sock.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(k) => read += k,
            Err(e) => bail!("recv failed: {e}"),
        }
    }
    buf.truncate(read);
    let s = String::from_utf8_lossy(&buf).into_owned();
    let len = s.len() as i32;
    state.pending = Some(s.into_bytes());
    Ok(len)
}

/// `net_close(sock)`: shut the connection down (idempotent).
fn host_net_close(mut caller: Caller<'_, ActorState>, sid: i32) -> Result<()> {
    if let Some(sock) = caller.data_mut().sockets.get_mut(sid as usize) {
        let _ = sock.get_mut().shutdown(std::net::Shutdown::Both);
    }
    Ok(())
}

// --- small helpers for safe guest-memory access ---

/// Read a witchy string value (a `[i32 len][bytes...]` header) at `ptr`.
fn read_wstr(data: &[u8], ptr: i32) -> Result<String> {
    let len_bytes = slice(data, ptr, 4)?;
    let len = i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
    let bytes = slice(data, ptr + 4, len)?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn memory_of(caller: &mut Caller<'_, ActorState>) -> Result<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(Error::msg("actor does not export a linear `memory`")),
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
mod tests {
    use super::*;

    const IMPORTS_PRINT: &str = r#"
        (module
          (import "witchy" "print" (func $print (param i32 i32)))
          (memory (export "memory") 1)
          (func (export "run")))
    "#;

    const RECEIVER: &str = r#"
        (module
          (import "witchy" "recv" (func $recv (param i32 i32) (result i32)))
          (global $got (export "got") (mut i32) (i32.const -2))
          (memory (export "memory") 1)
          (func (export "run")
            (global.set $got (call $recv (i32.const 0) (i32.const 256)))))
    "#;

    fn sender(target: ActorId, len: i32) -> String {
        format!(
            r#"
            (module
              (import "witchy" "send" (func $send (param i32 i32 i32)))
              (memory (export "memory") 1)
              (func (export "run")
                (call $send (i32.const {target}) (i32.const 0) (i32.const {len}))))
            "#
        )
    }

    const SPINNER: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "run") (loop $l (br $l))))
    "#;

    const GREEDY: &str = r#"
        (module (memory (export "memory") 4) (func (export "run")))
    "#;

    /// The core thesis: a capability that was not granted simply does not exist
    /// for the actor, so it cannot even be instantiated.
    #[test]
    fn ungranted_capability_is_unreachable() {
        let mut rt = Runtime::new().unwrap();
        let err = rt
            .spawn(IMPORTS_PRINT, Capabilities::none(), 4)
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown import"),
            "expected an unknown-import error, got: {err}"
        );
    }

    #[test]
    fn granted_capability_instantiates() {
        let mut rt = Runtime::new().unwrap();
        let mut actor = rt
            .spawn(IMPORTS_PRINT, Capabilities { print: true, ..Default::default() }, 4)
            .unwrap();
        actor.run().unwrap();
    }

    /// Two actors with no shared memory exchange a message purely through the
    /// host-mediated mailbox; the receiver observes the delivered byte count.
    #[test]
    fn message_passing_delivers_across_isolated_vms() {
        let mut rt = Runtime::new().unwrap();
        let mut receiver = rt
            .spawn(RECEIVER, Capabilities::none(), 4)
            .unwrap();
        let mut sender = rt
            .spawn(sender(receiver.id, 5), Capabilities { send: true, ..Default::default() }, 4)
            .unwrap();

        sender.run().unwrap();
        receiver.run().unwrap();

        let got = receiver
            .instance
            .get_global(&mut receiver.store, "got")
            .unwrap()
            .get(&mut receiver.store)
            .i32()
            .unwrap();
        assert_eq!(got, 5, "receiver should have read a 5-byte message");
    }

    #[test]
    fn send_to_unknown_actor_traps() {
        let mut rt = Runtime::new().unwrap();
        let mut sender = rt
            .spawn(sender(9999, 5), Capabilities { send: true, ..Default::default() }, 4)
            .unwrap();
        assert!(sender.run().is_err(), "sending to a nonexistent actor must fail");
    }

    /// Each actor's linear memory is its own; the runtime hands out separate
    /// `Store`s, so one actor's memory is never visible to another.
    #[test]
    fn actors_have_independent_memories() {
        let mut rt = Runtime::new().unwrap();
        let mut a = rt.spawn(RECEIVER, Capabilities::default(), 4).unwrap();
        let mut b = rt.spawn(RECEIVER, Capabilities::default(), 4).unwrap();
        let mem_a = a.instance.get_memory(&mut a.store, "memory").unwrap();
        let mem_b = b.instance.get_memory(&mut b.store, "memory").unwrap();
        // Distinct backing allocations.
        assert_ne!(
            mem_a.data_ptr(&a.store),
            mem_b.data_ptr(&b.store),
            "actors must not share a linear memory"
        );
    }

    #[test]
    fn memory_budget_is_enforced() {
        let mut rt = Runtime::new().unwrap();
        let err = rt.spawn(GREEDY, Capabilities::none(), 1).map(|_| ()).unwrap_err();
        assert!(
            err.to_string().contains("memory"),
            "expected a memory-limit error, got: {err}"
        );
    }

    /// A runaway actor that never yields is forcibly preempted by the scheduler.
    #[test]
    fn runaway_actor_is_preempted() {
        let mut rt = Runtime::new().unwrap();
        let mut spinner = rt.spawn(SPINNER, Capabilities::none(), 4).unwrap();
        let err = rt
            .run_with_budget(&mut spinner, Duration::from_millis(20))
            .unwrap_err();
        assert!(
            matches!(err.downcast_ref::<wasmtime::Trap>(), Some(wasmtime::Trap::Interrupt)),
            "expected an interrupt trap, got: {err}"
        );
    }
}
