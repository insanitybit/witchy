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

use crate::codegen::{MessageSig, MsgField};

/// One decoded message field, copied OUT of the sender at send time so the
/// receiver never sees sender memory: scalars by value, strings by content.
#[derive(Debug, Clone)]
enum FieldVal {
    Int(i32),
    Float(f64),
    Str(String),
}

/// (target actor id, message tag, decoded field values).
type Queue = Arc<Mutex<VecDeque<(usize, u32, Vec<FieldVal>)>>>;
type Output = Arc<Mutex<Vec<String>>>;

/// Per-actor host state.
struct Host {
    queue: Queue,
    output: Output,
    sigs: Arc<Vec<MessageSig>>,
    /// String state cells, indexed by the field order codegen assigned. State
    /// strings live HOST-side because the guest's no-GC arena resets between
    /// messages; reads stage through `pending` (the fill_pending protocol).
    str_cells: Mutex<Vec<String>>,
    pending: Mutex<Option<Vec<u8>>>,
}

pub struct System {
    engine: Engine,
    sigs: Arc<Vec<MessageSig>>,
    actors: Vec<(Store<Host>, Instance)>,
    queue: Queue,
    output: Output,
}

impl System {
    pub fn new(sigs: Vec<MessageSig>) -> Self {
        Self {
            engine: Engine::default(),
            sigs: Arc::new(sigs),
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
            sigs: Arc::clone(&self.sigs),
            str_cells: Mutex::new(Vec::new()),
            pending: Mutex::new(None),
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
        // String state cells: set copies the value's content out of the guest;
        // len stages a cell's bytes; fill_pending writes them into the fresh
        // guest allocation (the same staging protocol as Dir reads).
        linker.func_wrap(
            "witchy",
            "field_str_set",
            |mut caller: Caller<'_, Host>, idx: i32, ptr: i32| -> Result<()> {
                let mem = caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| Error::msg("actor has no memory"))?;
                let s = {
                    let data = mem.data(&caller);
                    let o = ptr as usize;
                    let len_bytes = data
                        .get(o..o + 4)
                        .ok_or_else(|| Error::msg("field_str_set out of bounds"))?;
                    let len =
                        i32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                            .max(0) as usize;
                    let bytes = data
                        .get(o + 4..o + 4 + len)
                        .ok_or_else(|| Error::msg("field_str_set out of bounds"))?;
                    String::from_utf8_lossy(bytes).into_owned()
                };
                let mut cells = caller.data().str_cells.lock().unwrap();
                if cells.len() <= idx as usize {
                    cells.resize(idx as usize + 1, String::new());
                }
                cells[idx as usize] = s;
                Ok(())
            },
        )?;
        linker.func_wrap(
            "witchy",
            "field_str_len",
            |caller: Caller<'_, Host>, idx: i32| -> Result<i32> {
                let cells = caller.data().str_cells.lock().unwrap();
                let bytes = cells.get(idx as usize).cloned().unwrap_or_default().into_bytes();
                let len = bytes.len() as i32;
                *caller.data().pending.lock().unwrap() = Some(bytes);
                Ok(len)
            },
        )?;
        linker.func_wrap(
            "witchy",
            "fill_pending",
            |mut caller: Caller<'_, Host>, out_ptr: i32| -> Result<()> {
                let staged = caller
                    .data()
                    .pending
                    .lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| Error::msg("fill_pending called with nothing staged"))?;
                let mem = caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| Error::msg("actor has no memory"))?;
                mem.write(&mut caller, out_ptr as usize, &staged)
                    .map_err(|e| Error::msg(format!("writing staged bytes: {e}")))?;
                Ok(())
            },
        )?;
        // Float -> string formatting, byte-identical to the interpreter's
        // Display (same bridge `runtime.rs` links for ordinary modules).
        linker.func_wrap(
            "witchy",
            "float_to_str",
            |mut caller: Caller<'_, Host>, x: f64, out_ptr: i32| -> Result<i32> {
                let bytes = format!("{x}").into_bytes();
                let mem = caller
                    .get_export("memory")
                    .and_then(Extern::into_memory)
                    .ok_or_else(|| Error::msg("actor has no memory"))?;
                mem.write(&mut caller, out_ptr as usize, &bytes)
                    .map_err(|e| Error::msg(format!("writing float string: {e}")))?;
                Ok(bytes.len() as i32)
            },
        )?;
        // The third argument is a pointer into the sender's memory to a field
        // record `[count][f0]..[fN-1]` (the list layout). The fields are
        // DECODED by the message tag's signature and copied now — an Int by
        // value, a String by content (the slot holds a sender-memory pointer
        // to `[len][bytes]`) — so the message carries values, not pointers,
        // and actors stay isolated.
        linker.func_wrap(
            "witchy",
            "send",
            |mut caller: Caller<'_, Host>, target: i32, tag: i32, ptr: i32| -> Result<()> {
                let sig = caller
                    .data()
                    .sigs
                    .get(tag as usize)
                    .map(|(_, fields)| fields.clone())
                    .ok_or_else(|| Error::msg(format!("send with unknown tag {tag}")))?;
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
                        // Each element is an 8-byte slot: an Int, Subject id, or
                        // string pointer lives in the low 4 bytes; a Float is
                        // the full slot's f64 bits.
                        let off = ptr + 4 + 8 * i;
                        match sig.get(i as usize) {
                            Some(MsgField::Str) => {
                                let slot = read(off)?;
                                let len = read(slot)?.max(0) as usize;
                                let o = slot as usize + 4;
                                let bytes = data
                                    .get(o..o + len)
                                    .ok_or_else(|| Error::msg("send string out of bounds"))?;
                                fs.push(FieldVal::Str(String::from_utf8_lossy(bytes).into_owned()));
                            }
                            Some(MsgField::Float) => {
                                let o = off as usize;
                                let b = data
                                    .get(o..o + 8)
                                    .ok_or_else(|| Error::msg("send field out of bounds"))?;
                                fs.push(FieldVal::Float(f64::from_le_bytes(
                                    b.try_into().expect("8-byte slice"),
                                )));
                            }
                            _ => fs.push(FieldVal::Int(read(off)?)),
                        }
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
            .sigs
            .iter()
            .position(|(m, _)| m == message)
            .ok_or_else(|| Error::msg(format!("unknown message `{message}`")))? as u32;
        // Driver-injected message: a single Int field (one-field or, for a
        // zero-field handler, ignored).
        self.queue.lock().unwrap().push_back((target, tag, vec![FieldVal::Int(arg)]));
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

    fn invoke(&mut self, target: usize, tag: u32, fields: &[FieldVal]) -> Result<()> {
        let Some((name, _)) = self.sigs.get(tag as usize) else {
            return Ok(());
        };
        let name = name.clone();
        let (store, instance) = &mut self.actors[target];
        // An actor that doesn't export a handler for this message just drops it.
        let Some(func) = instance.get_func(&mut *store, &name) else {
            return Ok(());
        };
        // Reset the target's no-GC arena before re-allocating message strings —
        // the prep/alloc pair is exported whenever the actor has a heap.
        if let Some(prep) = instance.get_typed_func::<(), ()>(&mut *store, "__msg_prep").ok() {
            prep.call(&mut *store, ())?;
        }
        let nparams = func.ty(&*store).params().len();
        // Pass one Val per handler parameter, in order: an Int by value, a
        // String re-allocated into the TARGET's memory via its `__msg_alloc`.
        let mut args: Vec<Val> = Vec::with_capacity(nparams);
        for f in fields.iter().take(nparams) {
            match f {
                FieldVal::Int(n) => args.push(Val::I32(*n)),
                FieldVal::Float(x) => args.push(Val::F64(x.to_bits())),
                FieldVal::Str(s) => {
                    let alloc =
                        instance.get_typed_func::<i32, i32>(&mut *store, "__msg_alloc")?;
                    let p = alloc.call(&mut *store, s.len() as i32)?;
                    let mem = instance
                        .get_memory(&mut *store, "memory")
                        .ok_or_else(|| Error::msg("actor has no memory"))?;
                    let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
                    bytes.extend_from_slice(s.as_bytes());
                    mem.write(&mut *store, p as usize, &bytes)?;
                    args.push(Val::I32(p));
                }
            }
        }
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

    /// String message fields cross the VM boundary by CONTENT: the host reads
    /// the bytes out of the sender's memory at send time and re-allocates them
    /// in the receiver (via its `__msg_alloc` export) at delivery — a literal,
    /// a runtime-built string, and a mixed String+Int message all arrive intact,
    /// and the receiver can compare and concatenate them as ordinary strings.
    #[test]
    fn string_message_params_cross_vms_by_content() {
        let src = r#"
actor Logger:
    console: Console

impl Logger:
    on Note(text: String, level: Int):
        print(console, (text <> "@" <> int_to_string(level)))
    on Check(word: String):
        print(console, if word == "magic": "yes" else: "no")

actor Producer:
    target: Subject

impl Producer:
    on Go(n: Int):
        send(target, Note("built:" <> int_to_string(n), n))
        send(target, Check("magic"))
        send(target, Check("plain"))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Producer"], "target", ids["Logger"]).unwrap();
        sys.send(ids["Producer"], "Go", 9).unwrap();
        assert_eq!(sys.output(), vec!["built:9@9", "yes", "no"]);
    }

    /// A Subject travels IN a message: the Introducer hands the Hub a
    /// reference to the Printer, and the Hub — which was never configured
    /// with a Printer field — messages it. Capability delegation between
    /// compiled actors, the heart of the actor security model.
    #[test]
    fn subject_message_params_delegate_send_authority() {
        let src = r#"
actor Printer:
    console: Console

impl Printer:
    on Show(n: Int):
        print(console, ("shown " <> int_to_string(n)))

actor Hub:
    console: Console

impl Hub:
    on Route(dest: Subject, n: Int):
        send(dest, Show((n + 1)))

actor Introducer:
    hub: Subject
    printer: Subject

impl Introducer:
    on Go(n: Int):
        send(hub, Route(printer, n))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Introducer"], "hub", ids["Hub"]).unwrap();
        sys.set_subject(ids["Introducer"], "printer", ids["Printer"]).unwrap();
        sys.send(ids["Introducer"], "Go", 41).unwrap();
        assert_eq!(sys.output(), vec!["shown 42"]);
    }

    /// Float message fields cross the VM boundary as the full f64 slot, and
    /// the receiver renders them with the same host formatting bridge ordinary
    /// modules use — arithmetic on the wire value stays exact.
    #[test]
    fn float_message_params_cross_vms() {
        let src = r#"
actor Gauge:
    console: Console

impl Gauge:
    on Reading(x: Float):
        print(console, to_string((x * 2.0)))

actor Sensor:
    target: Subject

impl Sensor:
    on Sample(n: Int):
        send(target, Reading(1.25))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Sensor"], "target", ids["Gauge"]).unwrap();
        sys.send(ids["Sensor"], "Sample", 0).unwrap();
        assert_eq!(sys.output(), vec!["2.5"]);
    }

    /// String STATE persists across messages: the field lives in a host cell
    /// (the guest's no-GC arena resets between messages, so a guest-heap
    /// string could not survive), is initialized at instantiation, reads back
    /// as a fresh arena copy, and is reassigned from a runtime-built value.
    #[test]
    fn string_state_fields_persist_across_messages() {
        let src = r#"
actor Greeter:
    console: Console
    var last: String = "nobody"

impl Greeter:
    on Greet(name: String):
        print(console, ("hello " <> name <> ", after " <> last))
        last = (name <> "!")

actor Caller:
    target: Subject

impl Caller:
    on Go(n: Int):
        send(target, Greet("ada"))
        send(target, Greet("grace"))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Caller"], "target", ids["Greeter"]).unwrap();
        sys.send(ids["Caller"], "Go", 0).unwrap();
        assert_eq!(
            sys.output(),
            vec!["hello ada, after nobody", "hello grace, after ada!"]
        );
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
