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
    bail, Caller, Config, Engine, Error, Extern, Linker, Memory, Module, Result, Store,
    StoreLimits, StoreLimitsBuilder,
};

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
    #[allow(dead_code)]
    caps: Capabilities,
    mailbox: Mailbox,
    mailboxes: Arc<Mailboxes>,
    limits: StoreLimits,
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
}

/// The runtime owns the wasm engine and the shared mailbox registry, and hands
/// out actor ids.
pub struct Runtime {
    engine: Engine,
    mailboxes: Arc<Mailboxes>,
    next_id: ActorId,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        // Enable epoch-based interruption so the scheduler can preempt a
        // runaway actor (exercised in M4). Normal runs set a deadline that is
        // never reached.
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            mailboxes: Arc::new(Mailboxes::default()),
            next_id: 1,
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
        };

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        // Deadline of 1 epoch; we never advance the engine epoch during a
        // normal run, so this is never tripped. M4 advances it to preempt.
        store.set_epoch_deadline(1);

        let mut linker: Linker<ActorState> = Linker::new(&self.engine);
        // --- capability wiring: only granted host functions are defined ---
        if caps.print {
            linker.func_wrap("witchy", "print", host_print)?;
        }
        if caps.send {
            linker.func_wrap("witchy", "send", host_send)?;
        }
        // `recv` is intrinsic: reading your *own* mailbox is not authority over
        // anyone else, so every actor may do it.
        linker.func_wrap("witchy", "recv", host_recv)?;

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
    print!("[actor {id}] {text}");
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

// --- small helpers for safe guest-memory access ---

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
            .spawn(IMPORTS_PRINT, Capabilities { print: true, send: false }, 4)
            .unwrap();
        actor.run().unwrap();
    }

    /// Two actors with no shared memory exchange a message purely through the
    /// host-mediated mailbox; the receiver observes the delivered byte count.
    #[test]
    fn message_passing_delivers_across_isolated_vms() {
        let mut rt = Runtime::new().unwrap();
        let mut receiver = rt
            .spawn(RECEIVER, Capabilities { print: false, send: false }, 4)
            .unwrap();
        let mut sender = rt
            .spawn(sender(receiver.id, 5), Capabilities { print: false, send: true }, 4)
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
            .spawn(sender(9999, 5), Capabilities { print: false, send: true }, 4)
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
