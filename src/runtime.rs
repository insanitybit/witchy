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
#[derive(Clone, Copy, Debug, Default)]
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
        let state = ActorState {
            id,
            caps,
            mailbox,
            mailboxes: Arc::clone(&self.mailboxes),
            limits,
            output: Arc::new(Mutex::new(Vec::new())),
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
            .spawn(IMPORTS_PRINT, Capabilities { print: true, send: false, print_int: false, quiet: false }, 4)
            .unwrap();
        actor.run().unwrap();
    }

    /// Two actors with no shared memory exchange a message purely through the
    /// host-mediated mailbox; the receiver observes the delivered byte count.
    #[test]
    fn message_passing_delivers_across_isolated_vms() {
        let mut rt = Runtime::new().unwrap();
        let mut receiver = rt
            .spawn(RECEIVER, Capabilities { print: false, send: false, print_int: false, quiet: false }, 4)
            .unwrap();
        let mut sender = rt
            .spawn(sender(receiver.id, 5), Capabilities { print: false, send: true, print_int: false, quiet: false }, 4)
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
            .spawn(sender(9999, 5), Capabilities { print: false, send: true, print_int: false, quiet: false }, 4)
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
