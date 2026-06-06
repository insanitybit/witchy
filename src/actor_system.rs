//! A runtime for *compiled* witchy actors that message each other.
//!
//! Each actor is its own WASM module in its own `Store` (its own VM). A compiled
//! `send(subject, Msg(arg))` calls the host `send(target_id, tag, arg)`, which
//! enqueues the message; the system drains the queue, routing each message to
//! the target actor's exported handler (named by the message, looked up by tag).
//! `Subject` fields are exported globals the host sets at spawn.
//!
//! This grants every actor the host capabilities (print/print_int/send) for
//! simplicity; per-actor capability *gating* is enforced by the spike runtime
//! in `runtime.rs`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use wasmtime::{Caller, Engine, Error, Extern, Instance, Linker, Module, Result, Store, Val};

/// (target actor id, message tag, Int field values copied from the sender).
/// Fields are copied at send time, so the receiver never sees sender memory.
type Queue = Arc<Mutex<VecDeque<(usize, u32, Vec<i32>)>>>;
type Output = Arc<Mutex<Vec<String>>>;

/// Per-actor host state.
struct Host {
    queue: Queue,
    output: Output,
}

pub struct System {
    engine: Engine,
    tag_to_message: Vec<String>,
    actors: Vec<(Store<Host>, Instance)>,
    queue: Queue,
    output: Output,
}

impl System {
    pub fn new(tag_to_message: Vec<String>) -> Self {
        Self {
            engine: Engine::default(),
            tag_to_message,
            actors: Vec::new(),
            queue: Arc::new(Mutex::new(VecDeque::new())),
            output: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Everything the actors have printed, in order.
    pub fn output(&self) -> Vec<String> {
        self.output.lock().unwrap().clone()
    }

    /// Instantiate a compiled actor module in its own VM; returns its id.
    pub fn spawn(&mut self, wat: &str) -> Result<usize> {
        let module = Module::new(&self.engine, wat)?;
        let host = Host {
            queue: Arc::clone(&self.queue),
            output: Arc::clone(&self.output),
        };
        let mut store = Store::new(&self.engine, host);
        let mut linker = Linker::new(&self.engine);

        linker.func_wrap(
            "witchy",
            "print",
            |mut caller: Caller<'_, Host>, ptr: i32, len: i32| -> Result<()> {
                let mem = caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| Error::msg("actor has no memory"))?;
                let s = {
                    let data = mem.data(&caller);
                    let bytes = data
                        .get(ptr as usize..(ptr + len) as usize)
                        .ok_or_else(|| Error::msg("print out of bounds"))?;
                    String::from_utf8_lossy(bytes).into_owned()
                };
                caller.data().output.lock().unwrap().push(s);
                Ok(())
            },
        )?;
        linker.func_wrap("witchy", "print_int", |caller: Caller<'_, Host>, n: i64| {
            caller.data().output.lock().unwrap().push(n.to_string());
        })?;
        // The third argument is a pointer into the sender's memory to a field
        // record `[count][f0]..[fN-1]` (the list layout). The fields are read
        // and copied now, so the message carries values, not a pointer — actors
        // stay isolated.
        linker.func_wrap(
            "witchy",
            "send",
            |mut caller: Caller<'_, Host>, target: i32, tag: i32, ptr: i32| -> Result<()> {
                let fields = {
                    let mem = caller
                        .get_export("memory")
                        .and_then(Extern::into_memory)
                        .ok_or_else(|| Error::msg("actor has no memory"))?;
                    let data = mem.data(&caller);
                    let read = |off: i32| -> Result<i32> {
                        let o = off as usize;
                        let b = data
                            .get(o..o + 4)
                            .ok_or_else(|| Error::msg("send field out of bounds"))?;
                        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    };
                    let count = read(ptr)?.max(0);
                    let mut fs = Vec::with_capacity(count as usize);
                    for i in 0..count {
                        // Each element is an 8-byte slot; the Int value is in its
                        // low 4 bytes (the list layout is now 8-byte slots).
                        fs.push(read(ptr + 4 + 8 * i)?);
                    }
                    fs
                };
                caller
                    .data()
                    .queue
                    .lock()
                    .unwrap()
                    .push_back((target as usize, tag as u32, fields));
                Ok(())
            },
        )?;

        let instance = linker.instantiate(&mut store, &module)?;
        let id = self.actors.len();
        self.actors.push((store, instance));
        Ok(id)
    }

    /// Set an exported `Subject` global (e.g. a `target` field) to an actor id.
    pub fn set_subject(&mut self, id: usize, field: &str, target: usize) -> Result<()> {
        let (store, instance) = &mut self.actors[id];
        let global = instance
            .get_global(&mut *store, field)
            .ok_or_else(|| Error::msg(format!("no exported global `{field}`")))?;
        global.set(&mut *store, Val::I32(target as i32))?;
        Ok(())
    }

    /// Deliver a message to an actor by name, then run to quiescence.
    pub fn send(&mut self, target: usize, message: &str, arg: i32) -> Result<()> {
        let tag = self
            .tag_to_message
            .iter()
            .position(|m| m == message)
            .ok_or_else(|| Error::msg(format!("unknown message `{message}`")))? as u32;
        // Driver-injected message: a single Int field (one-field or, for a
        // zero-field handler, ignored).
        self.queue.lock().unwrap().push_back((target, tag, vec![arg]));
        self.run_to_quiescence()
    }

    fn run_to_quiescence(&mut self) -> Result<()> {
        let mut steps = 0u64;
        loop {
            let item = self.queue.lock().unwrap().pop_front();
            let Some((target, tag, fields)) = item else {
                break;
            };
            steps += 1;
            if steps > 1_000_000 {
                return Err(Error::msg("actor system exceeded its step budget"));
            }
            self.invoke(target, tag, &fields)?;
        }
        Ok(())
    }

    fn invoke(&mut self, target: usize, tag: u32, fields: &[i32]) -> Result<()> {
        let Some(name) = self.tag_to_message.get(tag as usize).cloned() else {
            return Ok(());
        };
        let (store, instance) = &mut self.actors[target];
        // An actor that doesn't export a handler for this message just drops it.
        let Some(func) = instance.get_func(&mut *store, &name) else {
            return Ok(());
        };
        let nparams = func.ty(&*store).params().len();
        // Pass one Val per handler parameter, in order.
        let args: Vec<Val> = fields.iter().take(nparams).map(|&f| Val::I32(f)).collect();
        func.call(&mut *store, &args, &mut [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codegen, parser};

    fn build(src: &str) -> (System, std::collections::HashMap<String, usize>) {
        let module = parser::parse_module(src).unwrap();
        let (actors, tags) = codegen::compile_program(&module).unwrap();
        let mut sys = System::new(tags);
        let mut ids = std::collections::HashMap::new();
        for (name, wat) in &actors {
            let id = sys.spawn(wat).unwrap();
            ids.insert(name.clone(), id);
        }
        (sys, ids)
    }

    #[test]
    fn compiled_actors_message_each_other() {
        let src = r#"
actor Printer:
    console: Console

impl Printer:
    on Show(n: Int):
        print(console, ("got " <> int_to_string(n)))

actor Forwarder:
    target: Subject

impl Forwarder:
    on Relay(n: Int):
        send(target, Show(n))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Forwarder"], "target", ids["Printer"]).unwrap();
        sys.send(ids["Forwarder"], "Relay", 42).unwrap();
        assert_eq!(sys.output(), vec!["got 42"]);
    }

    #[test]
    fn multi_field_and_zero_field_messages() {
        // Messages now carry any number of Int fields across the VM boundary
        // (copied by value): a two-field Add and a zero-field Ping, both sent
        // actor-to-actor.
        let src = r#"
actor Worker:
    console: Console

impl Worker:
    on Add(x: Int, y: Int):
        print(console, int_to_string((x + y)))
    on Ping():
        print(console, "pong")

actor Boss:
    target: Subject

impl Boss:
    on Go(n: Int):
        send(target, Add(n, (n * 2)))
        send(target, Ping)
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Boss"], "target", ids["Worker"]).unwrap();
        sys.send(ids["Boss"], "Go", 10).unwrap();
        assert_eq!(sys.output(), vec!["30", "pong"]);
    }

    #[test]
    fn message_chains_drain_to_quiescence() {
        // Relayer forwards to a Printer; deliver two messages.
        let src = r#"
actor Printer:
    console: Console

impl Printer:
    on Show(n: Int):
        print(console, int_to_string(n))

actor Relayer:
    target: Subject

impl Relayer:
    on Relay(n: Int):
        send(target, Show(n))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Relayer"], "target", ids["Printer"]).unwrap();
        sys.send(ids["Relayer"], "Relay", 1).unwrap();
        sys.send(ids["Relayer"], "Relay", 2).unwrap();
        assert_eq!(sys.output(), vec!["1", "2"]);
    }
}
