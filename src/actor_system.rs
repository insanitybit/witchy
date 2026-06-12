//! A runtime for *compiled* witchy actors that message each other.
//!
//! Each actor is its own WASM module in its own `Store` (its own VM). A compiled
//! `send(subject, Msg(arg))` calls the host `send(target_id, tag, arg)`, which
//! enqueues the message; the system drains the queue, routing each message to
//! the target actor's exported handler (named by the message, looked up by tag).
//! `Subject` fields are exported globals the host sets at spawn.
//!
//! Capabilities are per-actor and gated exactly like the single-module sandbox
//! (the SAME `link_capability_imports` surface from `runtime.rs`, over the same
//! `ActorState`): each kind's VM links only the import families its declared
//! capability fields entitle it to — an actor without a `Console` field
//! physically has no `print` import. Dir/Net authority transfers at spawn by
//! HANDLE TRANSLATION: the spawner passes its i32 handle, the host resolves it
//! in the spawner's table and installs the payload in the spawnee's own table,
//! so attenuation (`subdir`/`restrict`) carries across VMs.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use wasmtime::{Caller, Engine, Error, Extern, Instance, Linker, Module, Result, Store, Val};

use crate::codegen::{KindGate, MessageSig, MsgField, SpawnArgKind};
use crate::runtime::{link_capability_imports, system_state, ActorState, Capabilities};

/// One decoded message field, copied OUT of the sender at send time so the
/// receiver never sees sender memory: scalars by value, strings by content.
#[derive(Debug, Clone)]
enum FieldVal {
    Int(i32),
    Float(f64),
    Str(String),
    IntList(Vec<i64>),
    StrList(Vec<String>),
    Tuple(Vec<TupleElem>),
}

/// One tuple member, decoded by its declared kind (tuple slots hold the
/// universal i64 rep, so Ints travel at full width).
#[derive(Debug, Clone)]
enum TupleElem {
    Int(i64),
    Float(f64),
    Str(String),
}

/// (target actor id, message tag, decoded field values).
type Queue = Arc<Mutex<VecDeque<(usize, u32, Vec<FieldVal>)>>>;
type Output = Arc<Mutex<Vec<String>>>;

/// One persistent state cell. String/list state lives HOST-side because the
/// guest's no-GC arena resets between messages.
#[derive(Debug, Clone)]
pub(crate) enum StateCell {
    Str(String),
    IntList(Vec<i64>),
    StrList(Vec<String>),
}

/// The actor-system extension carried in `ActorState.sys`: the per-actor
/// state cells (everything else the system needs — queue, signatures, output
/// — is shared and captured by the host-function closures).
pub(crate) struct SysState {
    /// State cells, indexed by the field order codegen assigned; reads stage
    /// through `pending` (fill_pending) or `pending_list` (write_pending_list).
    pub(crate) cells: Vec<Option<StateCell>>,
}

/// The live actor table. Entries are `Option` because delivery TAKES the
/// target out while its handler runs: the table's lock is released during the
/// call, so a handler's `spawn` can register a new actor without deadlocking.
type Actors = Arc<Mutex<Vec<Option<(Store<ActorState>, Instance)>>>>;

/// Everything a VM (or a `spawn_*` host call instantiating a new one) needs:
/// cheap-clone handles to the system's engine, mailboxes, output, message
/// signatures, registered actor kinds, and the live actor table.
#[derive(Clone)]
struct Shared {
    engine: Engine,
    queue: Queue,
    output: Output,
    sigs: Arc<Vec<MessageSig>>,
    /// Spawnable kinds: (name, compiled module, spawn-arg spec, capability gate).
    kinds: Arc<Vec<(String, Module, Vec<(String, SpawnArgKind)>, KindGate)>>,
    actors: Actors,
}

pub struct System {
    shared: Shared,
}

/// The development/differential grant (parity runs, direct test drives):
/// mirrors what the interpreter ambiently allows a dev run — output, clock,
/// env, a Dir rooted at `.` with both rights, and an empty Net allowlist.
/// `witchy sandbox` is the strict path that grants exactly the footprint.
pub(crate) fn dev_caps() -> Capabilities {
    Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        clock: true,
        env: true,
        dir_root: Some(std::path::PathBuf::from(".")),
        dir_read: true,
        dir_write: true,
        net_allow: Some(Vec::new()),
        net_connect: true,
        net_listen: true,
        ..Default::default()
    }
}

/// Convert an actor kind's static gate into a `Capabilities` for LINKING.
/// The payload fields (`dir_root`/`net_allow`) are placeholders that only
/// arm the family's gate — the real authority is the per-VM handle TABLES,
/// seeded from the spawner's translated handles, never from these.
fn gate_caps(g: &KindGate) -> Capabilities {
    Capabilities {
        print: g.print,
        print_int: g.print,
        quiet: true,
        clock: g.clock,
        env: g.env,
        dir_root: if g.dir_read || g.dir_write {
            Some(std::path::PathBuf::new())
        } else {
            None
        },
        dir_read: g.dir_read,
        dir_write: g.dir_write,
        net_allow: if g.net_connect || g.net_listen { Some(Vec::new()) } else { None },
        net_connect: g.net_connect,
        net_listen: g.net_listen,
        ..Default::default()
    }
}

impl System {
    /// Test/driver entry points: production programs construct systems via
    /// `run_program`, but tests drive actors directly.
    #[allow(dead_code)]
    pub fn new(sigs: Vec<MessageSig>) -> Self {
        Self::new_with_kinds(speed_engine(), sigs, Vec::new())
    }

    fn new_with_kinds(
        engine: Engine,
        sigs: Vec<MessageSig>,
        kinds: Vec<(String, Module, Vec<(String, SpawnArgKind)>, KindGate)>,
    ) -> Self {
        Self {
            shared: Shared {
                engine,
                queue: Arc::new(Mutex::new(VecDeque::new())),
                output: Arc::new(Mutex::new(Vec::new())),
                sigs: Arc::new(sigs),
                kinds: Arc::new(kinds),
                actors: Arc::new(Mutex::new(Vec::new())),
            },
        }
    }

    /// Everything the actors have printed, in order.
    pub fn output(&self) -> Vec<String> {
        self.shared.output.lock().unwrap().clone()
    }

    /// Instantiate a compiled actor module in its own VM under the
    /// development grant; returns its id. (Tests drive actors directly.)
    #[allow(dead_code)]
    pub fn spawn(&mut self, wat: &str) -> Result<usize> {
        let module = Module::new(&self.shared.engine, crate::runtime::optimize_module(wat.as_bytes()))?;
        let caps = dev_caps();
        let dirs = caps.dir_root.iter().cloned().collect();
        let nets = caps.net_allow.iter().cloned().collect();
        let (store, instance) = link_vm(&self.shared, &module, &caps, dirs, nets)?;
        let mut actors = self.shared.actors.lock().unwrap();
        actors.push(Some((store, instance)));
        Ok(actors.len() - 1)
    }
}

/// Build a VM (store + instance) wired to the system: the capability imports
/// its grant entitles it to (and NO others — the shared gated surface from
/// `runtime.rs`), the actor-system surface (typed `send`, state cells), plus
/// one `spawn_{Kind}` import per registered actor kind — a guest `spawn`
/// instantiates the kind's own VM under ITS gate, translates Dir/Net handles
/// into its tables, sets its value arguments, and returns the new actor's id.
fn link_vm(
    shared: &Shared,
    module: &Module,
    caps: &Capabilities,
    dirs: Vec<std::path::PathBuf>,
    nets: Vec<Vec<String>>,
) -> Result<(Store<ActorState>, Instance)> {
    let state = system_state(
        caps,
        Arc::clone(&shared.output),
        dirs,
        nets,
        SysState { cells: Vec::new() },
    );
    let mut store = Store::new(&shared.engine, state);
    store.limiter(|s| &mut s.limits);
    let mut linker: Linker<ActorState> = Linker::new(&shared.engine);
    // The gated capability surface — print/clock/env/dir/net families exactly
    // as granted, plus the authority-free staples (fill_pending,
    // write_pending_list, float_to_str, pure crypto/encoding).
    link_capability_imports(&mut linker, caps)?;

    // --- actor STATE cells: no authority (actor-local state); reads stage
    // through the same pending-transfer protocol as Dir reads. ---
    linker.func_wrap(
        "witchy",
        "field_str_set",
        |mut caller: Caller<'_, ActorState>, idx: i32, ptr: i32| -> Result<()> {
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
            set_cell(&mut caller, idx, StateCell::Str(s));
            Ok(())
        },
    )?;
    linker.func_wrap(
        "witchy",
        "field_str_len",
        |mut caller: Caller<'_, ActorState>, idx: i32| -> Result<i32> {
            let s = match get_cell(&mut caller, idx) {
                Some(StateCell::Str(s)) => s,
                None => String::new(),
                Some(other) => {
                    return Err(Error::msg(format!("cell {idx} is not a String: {other:?}")))
                }
            };
            let bytes = s.into_bytes();
            let len = bytes.len() as i32;
            caller.data_mut().pending = Some(bytes);
            Ok(len)
        },
    )?;
    linker.func_wrap(
        "witchy",
        "field_intlist_set",
        |mut caller: Caller<'_, ActorState>, idx: i32, ptr: i32| -> Result<()> {
            let mem = caller
                .get_export("memory")
                .and_then(Extern::into_memory)
                .ok_or_else(|| Error::msg("actor has no memory"))?;
            let xs = {
                let data = mem.data(&caller);
                read_i64_list(data, ptr)?
            };
            set_cell(&mut caller, idx, StateCell::IntList(xs));
            Ok(())
        },
    )?;
    linker.func_wrap(
        "witchy",
        "field_intlist_len",
        |mut caller: Caller<'_, ActorState>, idx: i32| -> Result<i32> {
            let xs = match get_cell(&mut caller, idx) {
                Some(StateCell::IntList(xs)) => xs,
                None => Vec::new(),
                Some(other) => {
                    return Err(Error::msg(format!("cell {idx} is not a List(Int): {other:?}")))
                }
            };
            let mut bytes = (xs.len() as i32).to_le_bytes().to_vec();
            for x in &xs {
                bytes.extend_from_slice(&x.to_le_bytes());
            }
            let size = bytes.len() as i32;
            caller.data_mut().pending = Some(bytes);
            Ok(size)
        },
    )?;
    linker.func_wrap(
        "witchy",
        "field_strlist_set",
        |mut caller: Caller<'_, ActorState>, idx: i32, ptr: i32| -> Result<()> {
            let mem = caller
                .get_export("memory")
                .and_then(Extern::into_memory)
                .ok_or_else(|| Error::msg("actor has no memory"))?;
            let xs = {
                let data = mem.data(&caller);
                read_str_list(data, ptr)?
            };
            set_cell(&mut caller, idx, StateCell::StrList(xs));
            Ok(())
        },
    )?;
    linker.func_wrap(
        "witchy",
        "field_strlist_size",
        |mut caller: Caller<'_, ActorState>, idx: i32| -> Result<i32> {
            let xs = match get_cell(&mut caller, idx) {
                Some(StateCell::StrList(xs)) => xs,
                None => Vec::new(),
                Some(other) => {
                    return Err(Error::msg(format!(
                        "cell {idx} is not a List(String): {other:?}"
                    )))
                }
            };
            let size = 4 + 8 * xs.len() + xs.iter().map(|s| 4 + s.len()).sum::<usize>();
            caller.data_mut().pending_list = Some(xs);
            Ok(size as i32)
        },
    )?;

    // --- typed send: possession of a Subject id IS the send authority, so it
    // is always linked. The third argument is a pointer into the sender's
    // memory to a field record `[count][f0]..[fN-1]` (the list layout). The
    // fields are DECODED by the message tag's signature and copied now — an
    // Int by value, a String by content — so the message carries values, not
    // pointers, and actors stay isolated. ---
    let send_sigs = Arc::clone(&shared.sigs);
    let send_queue = Arc::clone(&shared.queue);
    linker.func_wrap(
        "witchy",
        "send",
        move |mut caller: Caller<'_, ActorState>, target: i32, tag: i32, ptr: i32| -> Result<()> {
            let sig = send_sigs
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
                        Some(MsgField::IntList) => {
                            let list = read(off)?;
                            let n = read(list)?.max(0);
                            let mut xs = Vec::with_capacity(n as usize);
                            for j in 0..n {
                                let o = (list + 4 + 8 * j) as usize;
                                let b = data
                                    .get(o..o + 8)
                                    .ok_or_else(|| Error::msg("send list out of bounds"))?;
                                xs.push(i64::from_le_bytes(b.try_into().expect("8 bytes")));
                            }
                            fs.push(FieldVal::IntList(xs));
                        }
                        Some(MsgField::StrList) => {
                            let list = read(off)?;
                            let n = read(list)?.max(0);
                            let mut xs = Vec::with_capacity(n as usize);
                            for j in 0..n {
                                let sp = read(list + 4 + 8 * j)?;
                                let len = read(sp)?.max(0) as usize;
                                let o = sp as usize + 4;
                                let bytes = data
                                    .get(o..o + len)
                                    .ok_or_else(|| Error::msg("send string out of bounds"))?;
                                xs.push(String::from_utf8_lossy(bytes).into_owned());
                            }
                            fs.push(FieldVal::StrList(xs));
                        }
                        Some(MsgField::Tuple(elems)) => {
                            let tup = read(off)?;
                            let read64 = |o: i32| -> Result<i64> {
                                let o = o as usize;
                                let b = data
                                    .get(o..o + 8)
                                    .ok_or_else(|| Error::msg("send tuple out of bounds"))?;
                                Ok(i64::from_le_bytes(b.try_into().expect("8 bytes")))
                            };
                            let mut xs = Vec::with_capacity(elems.len());
                            for (j, e) in elems.iter().enumerate() {
                                let slot = tup + 4 + 8 * j as i32;
                                match e {
                                    MsgField::Float => {
                                        xs.push(TupleElem::Float(f64::from_bits(
                                            read64(slot)? as u64,
                                        )));
                                    }
                                    MsgField::Str => {
                                        let sp = read(slot)?;
                                        let len = read(sp)?.max(0) as usize;
                                        let o = sp as usize + 4;
                                        let bytes = data.get(o..o + len).ok_or_else(|| {
                                            Error::msg("send string out of bounds")
                                        })?;
                                        xs.push(TupleElem::Str(
                                            String::from_utf8_lossy(bytes).into_owned(),
                                        ));
                                    }
                                    _ => xs.push(TupleElem::Int(read64(slot)?)),
                                }
                            }
                            fs.push(FieldVal::Tuple(xs));
                        }
                        _ => fs.push(FieldVal::Int(read(off)?)),
                    }
                }
                fs
            };
            send_queue.lock().unwrap().push_back((target as usize, tag as u32, fields));
            Ok(())
        },
    )?;

    // One spawn import per registered actor kind: translate the spawner's
    // Dir/Net handles into payloads for the new VM's own tables, instantiate
    // the kind's VM under ITS capability gate, set its value (Subject) and
    // capability-handle globals, and hand back the new id. Console/Clock/Env
    // arguments were erased at the call site — the gate carries them.
    for (kname, kmodule, spec, gate) in shared.kinds.iter() {
        let nvals = spec.iter().filter(|(_, k)| *k != SpawnArgKind::Erased).count();
        let ty = wasmtime::FuncType::new(
            &shared.engine,
            std::iter::repeat(wasmtime::ValType::I32).take(nvals),
            [wasmtime::ValType::I32],
        );
        let shared2 = shared.clone();
        let kmodule = kmodule.clone();
        let spec = spec.clone();
        let kcaps = gate_caps(gate);
        linker.func_new(
            "witchy",
            &format!("spawn_{kname}"),
            ty,
            move |caller: Caller<'_, ActorState>, params, results| {
                // Resolve capability handles in the SPAWNER's tables first;
                // the payloads seed the spawnee's own tables in field order.
                let mut dirs: Vec<std::path::PathBuf> = Vec::new();
                let mut nets: Vec<Vec<String>> = Vec::new();
                let mut sets: Vec<(String, Val)> = Vec::new();
                let mut pi = 0;
                for (field, kind) in &spec {
                    match kind {
                        SpawnArgKind::Erased => {}
                        SpawnArgKind::Value => {
                            sets.push((field.clone(), params[pi].clone()));
                            pi += 1;
                        }
                        SpawnArgKind::Dir => {
                            let h = params[pi].i32().unwrap_or(-1);
                            pi += 1;
                            let path = caller
                                .data()
                                .dirs
                                .get(h as usize)
                                .cloned()
                                .ok_or_else(|| {
                                    Error::msg(format!("spawn {field}: unknown Dir handle {h}"))
                                })?;
                            sets.push((field.clone(), Val::I32(dirs.len() as i32)));
                            dirs.push(path);
                        }
                        SpawnArgKind::Net => {
                            let h = params[pi].i32().unwrap_or(-1);
                            pi += 1;
                            let allow = caller
                                .data()
                                .nets
                                .get(h as usize)
                                .cloned()
                                .ok_or_else(|| {
                                    Error::msg(format!("spawn {field}: unknown Net handle {h}"))
                                })?;
                            sets.push((field.clone(), Val::I32(nets.len() as i32)));
                            nets.push(allow);
                        }
                    }
                }
                let (mut store, instance) = link_vm(&shared2, &kmodule, &kcaps, dirs, nets)?;
                for (field, val) in sets {
                    let g = instance.get_global(&mut store, &field).ok_or_else(|| {
                        Error::msg(format!("spawned actor has no exported global `{field}`"))
                    })?;
                    g.set(&mut store, val)?;
                }
                let mut actors = shared2.actors.lock().unwrap();
                actors.push(Some((store, instance)));
                results[0] = Val::I32((actors.len() - 1) as i32);
                Ok(())
            },
        )?;
    }

    let instance = linker.instantiate(&mut store, module)?;
    Ok((store, instance))
}

impl System {
    /// Set an exported `Subject` global (e.g. a `target` field) to an actor id.
    #[allow(dead_code)]
    pub fn set_subject(&mut self, id: usize, field: &str, target: usize) -> Result<()> {
        let mut actors = self.shared.actors.lock().unwrap();
        let (store, instance) = actors[id]
            .as_mut()
            .ok_or_else(|| Error::msg("actor is mid-delivery"))?;
        let global = instance
            .get_global(&mut *store, field)
            .ok_or_else(|| Error::msg(format!("no exported global `{field}`")))?;
        global.set(&mut *store, Val::I32(target as i32))?;
        Ok(())
    }

    /// Deliver a message to an actor by name, then run to quiescence.
    #[allow(dead_code)]
    pub fn send(&mut self, target: usize, message: &str, arg: i32) -> Result<()> {
        let tag = self
            .shared
            .sigs
            .iter()
            .position(|(m, _)| m == message)
            .ok_or_else(|| Error::msg(format!("unknown message `{message}`")))? as u32;
        // Driver-injected message: a single Int field (one-field or, for a
        // zero-field handler, ignored).
        self.shared.queue.lock().unwrap().push_back((target, tag, vec![FieldVal::Int(arg)]));
        self.run_to_quiescence()
    }

    /// Run a whole compiled actor PROGRAM: register every actor kind, run the
    /// driver's `main` in its own VM (its `spawn`s instantiate actor VMs, its
    /// `send`s enqueue), then drain the mailboxes to quiescence and return
    /// everything printed.
    pub fn run_program(
        driver_wat: &str,
        actor_wats: &[(String, String)],
        sigs: Vec<MessageSig>,
        specs: Vec<(String, Vec<(String, SpawnArgKind)>, KindGate)>,
        driver_caps: &Capabilities,
    ) -> Result<Vec<String>> {
        // WITCHY_PARALLEL_ACTORS=N drains with N worker threads (shared-
        // nothing parallelism across actor VMs); the default stays the
        // deterministic single-threaded drain, matching the interpreter's
        // global FIFO schedule.
        let workers = std::env::var("WITCHY_PARALLEL_ACTORS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        Self::run_program_with_workers(driver_wat, actor_wats, sigs, specs, driver_caps, workers)
    }

    /// `run_program` with an explicit drain width: `workers > 1` uses the
    /// parallel scheduler (cross-actor output interleaving is relaxed;
    /// per-actor delivery order is preserved).
    pub fn run_program_with_workers(
        driver_wat: &str,
        actor_wats: &[(String, String)],
        sigs: Vec<MessageSig>,
        specs: Vec<(String, Vec<(String, SpawnArgKind)>, KindGate)>,
        driver_caps: &Capabilities,
        workers: usize,
    ) -> Result<Vec<String>> {
        let engine = speed_engine();
        let mut kinds = Vec::new();
        for (name, wat) in actor_wats {
            let module = Module::new(&engine, crate::runtime::optimize_module(wat.as_bytes()))?;
            let (spec, gate) = specs
                .iter()
                .find(|(n, _, _)| n == name)
                .map(|(_, s, g)| (s.clone(), *g))
                .unwrap_or_default();
            kinds.push((name.clone(), module, spec, gate));
        }
        let mut sys = System::new_with_kinds(engine, sigs, kinds);
        let driver = Module::new(&sys.shared.engine, crate::runtime::optimize_module(driver_wat.as_bytes()))?;
        // The driver lives OUTSIDE the actor table (it has no handlers and
        // must not hold the table's lock while running, since its spawns
        // take that lock). Its grant comes from the host: the dev/differential
        // grant for `run`/`parity`, the computed footprint for `sandbox`.
        let ddirs = driver_caps.dir_root.iter().cloned().collect();
        let dnets = driver_caps.net_allow.iter().cloned().collect();
        let (mut dstore, dinstance) = link_vm(&sys.shared, &driver, driver_caps, ddirs, dnets)?;
        let run = dinstance.get_typed_func::<(), ()>(&mut dstore, "run")?;
        run.call(&mut dstore, ())?;
        if workers > 1 {
            sys.run_to_quiescence_parallel(workers)?;
        } else {
            sys.run_to_quiescence()?;
        }
        Ok(sys.output())
    }

    /// Drain the mailboxes with `workers` OS threads. Actors are isolated VMs,
    /// so cross-actor parallelism is safe by construction; what this relaxes
    /// is the GLOBAL print interleaving (per-actor delivery order is still
    /// FIFO: a message for a busy actor parks in a pending queue that splices
    /// back, in order, when the actor is returned to the table). Opt-in —
    /// the default drain stays deterministic, matching the interpreter.
    fn run_to_quiescence_parallel(&mut self, workers: usize) -> Result<()> {
        use std::collections::HashMap;
        use std::sync::Condvar;
        type Msg = (usize, u32, Vec<FieldVal>);
        struct Sched {
            pending: HashMap<usize, VecDeque<Msg>>,
            inflight: usize,
        }
        let sched = Arc::new((Mutex::new(Sched { pending: HashMap::new(), inflight: 0 }), Condvar::new()));
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for _ in 0..workers.max(1) {
                let shared = self.shared.clone();
                let sched = Arc::clone(&sched);
                let errors = Arc::clone(&errors);
                scope.spawn(move || {
                    loop {
                        // Lock order everywhere: sched > queue > actors.
                        let job = {
                            let (lock, cv) = &*sched;
                            let mut st = lock.lock().unwrap();
                            loop {
                                let mut grabbed: Option<(Msg, (Store<ActorState>, Instance))> = None;
                                {
                                    let mut q = shared.queue.lock().unwrap();
                                    while let Some(msg) = q.pop_front() {
                                        let mut actors = shared.actors.lock().unwrap();
                                        match actors.get_mut(msg.0) {
                                            Some(slot) if slot.is_some() => {
                                                let vm = slot.take().unwrap();
                                                grabbed = Some((msg, vm));
                                                break;
                                            }
                                            Some(_) => {
                                                // Target busy: park in arrival order.
                                                st.pending.entry(msg.0).or_default().push_back(msg);
                                            }
                                            None => {
                                                errors.lock().unwrap().push(format!(
                                                    "send to unknown actor id {}",
                                                    msg.0
                                                ));
                                            }
                                        }
                                    }
                                }
                                match grabbed {
                                    Some(j) => {
                                        st.inflight += 1;
                                        break Some(j);
                                    }
                                    None => {
                                        let queue_empty = shared.queue.lock().unwrap().is_empty();
                                        if st.inflight == 0 && queue_empty {
                                            cv.notify_all();
                                            break None;
                                        }
                                        st = cv.wait(st).unwrap();
                                    }
                                }
                            }
                        };
                        let Some(((target, tag, fields), (mut store, instance))) = job else {
                            return;
                        };
                        let name = shared.sigs.get(tag as usize).map(|(n, _)| n.clone());
                        let result = match name {
                            Some(n) => System::deliver(&mut store, &instance, &n, &fields),
                            None => Ok(()),
                        };
                        {
                            let (lock, cv) = &*sched;
                            let mut st = lock.lock().unwrap();
                            // Release parked messages to the FRONT (their
                            // arrival order preserved), then return the VM.
                            if let Some(mut parked) = st.pending.remove(&target) {
                                let mut q = shared.queue.lock().unwrap();
                                while let Some(m) = parked.pop_back() {
                                    q.push_front(m);
                                }
                            }
                            shared.actors.lock().unwrap()[target] = Some((store, instance));
                            st.inflight -= 1;
                            if let Err(e) = result {
                                errors.lock().unwrap().push(e.to_string());
                            }
                            cv.notify_all();
                        }
                    }
                });
            }
        });
        let errs = errors.lock().unwrap();
        if let Some(first) = errs.first() {
            return Err(Error::msg(first.clone()));
        }
        Ok(())
    }

    fn run_to_quiescence(&mut self) -> Result<()> {
        let mut steps = 0u64;
        loop {
            let item = self.shared.queue.lock().unwrap().pop_front();
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
        let Some((name, _)) = self.shared.sigs.get(tag as usize) else {
            return Ok(());
        };
        let name = name.clone();
        // TAKE the target out of the table and release the lock for the
        // duration of the call: a handler's `spawn` re-enters the table to
        // register the new VM, which would deadlock against a held lock. The
        // drain is single-threaded, so the same actor can never be delivered
        // to re-entrantly; sends during the call only touch the queue.
        let taken = {
            let mut actors = self.shared.actors.lock().unwrap();
            actors
                .get_mut(target)
                .and_then(|slot| slot.take())
                .ok_or_else(|| Error::msg(format!("no actor with id {target}")))?
        };
        let (mut store_owned, instance_owned) = taken;
        let result = Self::deliver(&mut store_owned, &instance_owned, &name, fields);
        let mut actors = self.shared.actors.lock().unwrap();
        actors[target] = Some((store_owned, instance_owned));
        result
    }

    /// Call the handler on a taken-out actor (no table lock held).
    fn deliver(
        store: &mut Store<ActorState>,
        instance: &Instance,
        name: &str,
        fields: &[FieldVal],
    ) -> Result<()> {
        // An actor that doesn't export a handler for this message just drops it.
        let Some(func) = instance.get_func(&mut *store, name) else {
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
                    let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
                    bytes.extend_from_slice(s.as_bytes());
                    args.push(Val::I32(write_block(store, instance, &bytes)?));
                }
                FieldVal::IntList(xs) => {
                    let mut bytes = (xs.len() as i32).to_le_bytes().to_vec();
                    for x in xs {
                        bytes.extend_from_slice(&x.to_le_bytes());
                    }
                    args.push(Val::I32(write_block(store, instance, &bytes)?));
                }
                FieldVal::Tuple(xs) => {
                    // `[0 tag][i64 slots]`, string members appended after the
                    // slots with absolute pointers computed from the base.
                    let n = xs.len();
                    let strings: usize = xs
                        .iter()
                        .map(|e| if let TupleElem::Str(s) = e { 4 + s.len() } else { 0 })
                        .sum();
                    let total = 4 + 8 * n + strings;
                    let alloc =
                        instance.get_typed_func::<i32, i32>(&mut *store, "__msg_alloc")?;
                    let base = alloc.call(&mut *store, (total - 4) as i32)?;
                    let mut bytes = Vec::with_capacity(total);
                    bytes.extend_from_slice(&0i32.to_le_bytes());
                    let mut str_off = base as i64 + 4 + 8 * n as i64;
                    let mut tail: Vec<u8> = Vec::with_capacity(strings);
                    for e in xs {
                        match e {
                            TupleElem::Int(v) => bytes.extend_from_slice(&v.to_le_bytes()),
                            TupleElem::Float(x) => {
                                bytes.extend_from_slice(&x.to_bits().to_le_bytes())
                            }
                            TupleElem::Str(s) => {
                                bytes.extend_from_slice(&str_off.to_le_bytes());
                                tail.extend_from_slice(&(s.len() as u32).to_le_bytes());
                                tail.extend_from_slice(s.as_bytes());
                                str_off += 4 + s.len() as i64;
                            }
                        }
                    }
                    bytes.extend_from_slice(&tail);
                    let mem = instance
                        .get_memory(&mut *store, "memory")
                        .ok_or_else(|| Error::msg("actor has no memory"))?;
                    mem.write(&mut *store, base as usize, &bytes)?;
                    args.push(Val::I32(base));
                }
                FieldVal::StrList(xs) => {
                    // The guest list layout with absolute pointers, computed
                    // from the allocation base (like `write_pending_list`).
                    let n = xs.len();
                    let total =
                        4 + 8 * n + xs.iter().map(|s| 4 + s.len()).sum::<usize>();
                    let alloc =
                        instance.get_typed_func::<i32, i32>(&mut *store, "__msg_alloc")?;
                    // __msg_alloc reserves `n + 4`; our block already counts its
                    // own 4-byte header, so ask for total - 4 content bytes.
                    let base = alloc.call(&mut *store, (total - 4) as i32)?;
                    let mut bytes = Vec::with_capacity(total);
                    bytes.extend_from_slice(&(n as i32).to_le_bytes());
                    let strings_start = base as i64 + 4 + 8 * n as i64;
                    let mut offset = 0i64;
                    for s in xs {
                        bytes.extend_from_slice(&(strings_start + offset).to_le_bytes());
                        offset += 4 + s.len() as i64;
                    }
                    for s in xs {
                        bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                        bytes.extend_from_slice(s.as_bytes());
                    }
                    let mem = instance
                        .get_memory(&mut *store, "memory")
                        .ok_or_else(|| Error::msg("actor has no memory"))?;
                    mem.write(&mut *store, base as usize, &bytes)?;
                    args.push(Val::I32(base));
                }
            }
        }
        func.call(&mut *store, &args, &mut [])?;
        Ok(())
    }
}

/// An engine at Cranelift's Speed tier (matching `runtime.rs`).
fn speed_engine() -> Engine {
    let mut config = wasmtime::Config::new();
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);
    Engine::new(&config).unwrap_or_default()
}

fn set_cell(caller: &mut Caller<'_, ActorState>, idx: i32, cell: StateCell) {
    let cells = &mut caller.data_mut().sys.as_mut().expect("system actor").cells;
    if cells.len() <= idx as usize {
        cells.resize(idx as usize + 1, None);
    }
    cells[idx as usize] = Some(cell);
}

fn get_cell(caller: &mut Caller<'_, ActorState>, idx: i32) -> Option<StateCell> {
    caller
        .data()
        .sys
        .as_ref()
        .expect("system actor")
        .cells
        .get(idx as usize)
        .cloned()
        .flatten()
}

/// Read a guest `[count][i64 slots]` list.
fn read_i64_list(data: &[u8], ptr: i32) -> Result<Vec<i64>> {
    let count = read_le_i32(data, ptr)?.max(0);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let o = (ptr + 4 + 8 * i) as usize;
        let b = data.get(o..o + 8).ok_or_else(|| Error::msg("list out of bounds"))?;
        out.push(i64::from_le_bytes(b.try_into().expect("8 bytes")));
    }
    Ok(out)
}

/// Read a guest `[count][string-pointer slots]` list by content.
fn read_str_list(data: &[u8], ptr: i32) -> Result<Vec<String>> {
    let count = read_le_i32(data, ptr)?.max(0);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let sp = read_le_i32(data, ptr + 4 + 8 * i)?;
        let len = read_le_i32(data, sp)?.max(0) as usize;
        let o = sp as usize + 4;
        let bytes = data.get(o..o + len).ok_or_else(|| Error::msg("string out of bounds"))?;
        out.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(out)
}

fn read_le_i32(data: &[u8], off: i32) -> Result<i32> {
    let o = off as usize;
    let b = data.get(o..o + 4).ok_or_else(|| Error::msg("read out of bounds"))?;
    Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reserve guest memory via the actor's `__msg_alloc` (which adds the 4-byte
/// header to its argument) and write a complete `[len/count][payload]` block.
fn write_block(store: &mut Store<ActorState>, instance: &Instance, block: &[u8]) -> Result<i32> {
    let alloc = instance.get_typed_func::<i32, i32>(&mut *store, "__msg_alloc")?;
    let p = alloc.call(&mut *store, (block.len() - 4) as i32)?;
    let mem = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| Error::msg("actor has no memory"))?;
    mem.write(&mut *store, p as usize, block)?;
    Ok(p)
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
        print(console, ("got " + __render(n)))

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
        print(console, (text + "@" + __render(level)))
    on Check(word: String):
        print(console, if word == "magic": "yes" else: "no")

actor Producer:
    target: Subject

impl Producer:
    on Go(n: Int):
        send(target, Note("built:" + __render(n), n))
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
        print(console, ("shown " + __render(n)))

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
        print(console, __render((x * 2.0)))

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
        print(console, ("hello " + name + ", after " + last))
        last = (name + "!")

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

    /// Float STATE is a real (mut f64) global: it persists across messages and
    /// accumulates with f64 arithmetic.
    #[test]
    fn float_state_fields_accumulate_across_messages() {
        let src = r#"
actor Tally:
    console: Console
    var total: Float = 0.5

impl Tally:
    on Bump(n: Int):
        total = (total + 1.25)
        print(console, __render(total))
"#;
        let (mut sys, ids) = build(src);
        sys.send(ids["Tally"], "Bump", 0).unwrap();
        sys.send(ids["Tally"], "Bump", 0).unwrap();
        assert_eq!(sys.output(), vec!["1.75", "3"]);
    }

    /// List message fields cross the VM boundary by content: a List(Int) is
    /// re-laid out slot-for-slot in the receiver, and a List(String) arrives
    /// with absolute pointers into the receiver's own memory — the receiver
    /// iterates, indexes, and compares them as ordinary lists.
    #[test]
    fn list_message_params_cross_vms_by_content() {
        let src = r#"
actor Stats:
    console: Console

impl Stats:
    on Nums(xs: List(Int)):
        var total = 0
        for x in xs:
            total = (total + x)
        print(console, __render(total))
    on Names(names: List(String)):
        var joined = ""
        for n in names:
            joined = (joined + n + ";")
        print(console, joined)

actor Feeder:
    target: Subject

impl Feeder:
    on Go(n: Int):
        send(target, Nums([10, 20, (n + 5)]))
        send(target, Names(["ada", ("x" + __render(n)), "grace"]))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Feeder"], "target", ids["Stats"]).unwrap();
        sys.send(ids["Feeder"], "Go", 7).unwrap();
        assert_eq!(sys.output(), vec!["42", "ada;x7;grace;"]);
    }

    /// A scalar tuple crosses the VM boundary by content: each member travels
    /// at its own kind (Int as the full i64 slot, Float as f64 bits, String by
    /// its bytes), re-laid out in the receiver, and the handler destructures
    /// it like any tuple.
    #[test]
    fn tuple_message_params_cross_vms_by_content() {
        let src = r#"
actor Sink:
    console: Console

impl Sink:
    on Entry(row: (Int, String, Float)):
        let (n, label, x) = row
        print(console, label + "=" + __render(n) + "/" + __render(x))

actor Source:
    target: Subject

impl Source:
    on Go(n: Int):
        send(target, Entry((n * 6, "answer", 0.5)))
"#;
        let (mut sys, ids) = build(src);
        sys.set_subject(ids["Source"], "target", ids["Sink"]).unwrap();
        sys.send(ids["Source"], "Go", 7).unwrap();
        assert_eq!(sys.output(), vec!["answer=42/0.5"]);
    }

    /// GUEST-CALLABLE SPAWN: the program's own `main` runs in a driver VM,
    /// `spawn Logger(console)` instantiates the actor in ITS own VM through a
    /// host import (the capability argument is erased — the system grants
    /// printing), `spawn Forwarder(logger)` passes a Subject id as a value
    /// argument, and `send` routes through the system. Byte-identical output
    /// to the interpreter running the same program.
    #[test]
    fn compiled_program_spawns_actors_from_main() {
        for src in [
            include_str!("../examples/actors.witchy"),
            // The full message model from a compiled main: Float/String/List
            // fields, Float state, and a Subject delivered IN a message.
            include_str!("../examples/dispatch.witchy"),
        ] {
            let module = parser::parse_module(src).expect("parse");
            let (driver, actors, sigs, specs) =
                codegen::compile_system(&module).expect("compile system");
            let out = System::run_program(&driver, &actors, sigs, specs, &dev_caps()).expect("run program");
            let interp = crate::interpreter::run_with(src, ".", Vec::new()).expect("interp");
            assert_eq!(out, interp, "compiled actor system must match the interpreter");
        }
    }

    /// The PARALLEL drain: worker threads deliver to distinct actors
    /// concurrently (shared-nothing VMs), preserving per-actor FIFO via
    /// parked pending queues. Cross-actor print interleaving is relaxed, so
    /// the assertion compares SORTED output against the deterministic drain,
    /// plus each actor's own messages must arrive in send order.
    #[test]
    fn parallel_drain_preserves_per_actor_order() {
        let src = r#"
actor Echo:
    console: Console

impl Echo:
    on Work(tag: String, n: Int):
        print(console, tag + ":" + __render(n))

actor Fan:
    a: Subject
    b: Subject
    c: Subject

impl Fan:
    on Go(count: Int):
        var i = 0
        for i in 0..count:
            send(a, Work("a", i))
            send(b, Work("b", i))
            send(c, Work("c", i))

fn main(console: Console):
    let e1 = spawn Echo(console)
    let e2 = spawn Echo(console)
    let e3 = spawn Echo(console)
    let fan = spawn Fan(e1, e2, e3)
    send(fan, Go(50))
"#;
        let module = parser::parse_module(src).expect("parse");
        let (driver, actors, sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let out =
            System::run_program_with_workers(&driver, &actors, sigs.clone(), specs.clone(), &dev_caps(), 4)
                .expect("parallel run");
        let serial =
            System::run_program(&driver, &actors, sigs, specs, &dev_caps()).expect("serial run");
        let mut sorted_out = out.clone();
        sorted_out.sort();
        let mut sorted_serial = serial.clone();
        sorted_serial.sort();
        assert_eq!(sorted_out, sorted_serial, "same multiset of outputs");
        for tag in ["a", "b", "c"] {
            let seq: Vec<&String> =
                out.iter().filter(|l| l.starts_with(&format!("{tag}:"))).collect();
            let nums: Vec<i64> = seq
                .iter()
                .map(|l| l.split(':').nth(1).unwrap().parse().unwrap())
                .collect();
            let mut expected = nums.clone();
            expected.sort();
            assert_eq!(nums, expected, "per-actor FIFO must hold for {tag}");
            assert_eq!(nums.len(), 50, "all {tag} messages delivered");
        }
    }

    /// CAPABILITY-HOLDING ACTORS: an actor's `Dir` field is a real handle
    /// into its own VM's host-side table. Spawn TRANSLATES the spawner's
    /// handle (here an attenuated `subdir`), so the spawned worker reads
    /// inside the subtree but cannot even see the parent's files — identical
    /// to the interpreter, which threads the capability value itself.
    #[test]
    fn actor_dir_fields_transfer_attenuated_through_spawn() {
        let tmp = std::env::temp_dir().join(format!("witchy_actorcap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("note.txt"), "outer note").unwrap();
        std::fs::write(tmp.join("sub/inner.txt"), "inner secret").unwrap();
        let src = r#"
actor Confined:
    console: Console
    dir: Dir
    on Read(name: String):
        print(console, read(dir, name))
    on Probe():
        if exists(dir, "note.txt"):
            print(console, "sees outer note.txt")
        else:
            print(console, "outer note.txt invisible")

actor Spawner:
    console: Console
    dir: Dir
    on Go():
        let worker = spawn Confined(console, subdir(dir, "sub"))
        send(worker, Read("inner.txt"))
        send(worker, Probe())

fn main(console: Console, dir: Dir):
    let s = spawn Spawner(console, dir)
    send(s, Go())
"#;
        let module = parser::parse_module(src).expect("parse");
        let (driver, actors, sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let mut caps = dev_caps();
        caps.dir_root = Some(tmp.clone());
        let out = System::run_program(&driver, &actors, sigs, specs, &caps)
            .expect("run program");
        let interp = crate::interpreter::run_with(src, &tmp, Vec::new()).expect("interp");
        assert_eq!(out, interp, "compiled actor system must match the interpreter");
        assert_eq!(out, vec!["inner secret", "outer note.txt invisible"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Clock fields work in compiled actors (`now(clock)` under the gated
    /// import), and a console-less actor (Subject field only) participates
    /// without ever holding a `print` import.
    #[test]
    fn actor_clock_field_and_consoleless_actor() {
        let src = r#"
actor Timer:
    console: Console
    clock: Clock
    on Tick():
        if now(clock) > 0:
            print(console, "ticked")
        else:
            print(console, "no time")

actor Quiet:
    out: Subject
    on Relay(n: Int):
        send(out, Tick())

fn main(console: Console, clock: Clock):
    let t = spawn Timer(console, clock)
    let q = spawn Quiet(t)
    send(q, Relay(1))
"#;
        let module = parser::parse_module(src).expect("parse");
        let (driver, actors, sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let out = System::run_program(&driver, &actors, sigs, specs, &dev_caps())
            .expect("run program");
        let interp = crate::interpreter::run_with(src, ".", Vec::new()).expect("interp");
        assert_eq!(out, interp);
        assert_eq!(out, vec!["ticked"]);
    }

    /// THE GATE, at the module level: each actor's compiled module imports
    /// only the host families its own code (and so its own fields) uses — a
    /// console-only actor carries no `dir_*`/`now`/`net_*` import for the
    /// host to even consider, and the link gate (derived from its fields)
    /// wouldn't define them anyway.
    #[test]
    fn actor_modules_import_only_their_own_capability_families() {
        let src = r#"
actor Logger:
    console: Console
    on Log(msg: String):
        print(console, msg)

actor Reader:
    console: Console
    dir: Dir
    on Read(name: String):
        print(console, read(dir, name))

fn main(console: Console, dir: Dir):
    let l = spawn Logger(console)
    let r = spawn Reader(console, dir)
    send(l, Log("hi"))
"#;
        let module = parser::parse_module(src).expect("parse");
        let (_driver, actors, _sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let logger = &actors.iter().find(|(n, _)| n == "Logger").unwrap().1;
        let reader = &actors.iter().find(|(n, _)| n == "Reader").unwrap().1;
        for forbidden in ["dir_read", "dir_write", "\"now\"", "net_connect", "net_listen"] {
            assert!(
                !logger.contains(forbidden),
                "Logger must not import {forbidden}"
            );
        }
        assert!(reader.contains("dir_read"), "Reader uses its Dir");
        let (_, _, lgate) = specs.iter().find(|(n, _, _)| n == "Logger").unwrap();
        assert!(lgate.print && !lgate.dir_read && !lgate.dir_write && !lgate.clock);
        let (_, _, rgate) = specs.iter().find(|(n, _, _)| n == "Reader").unwrap();
        assert!(rgate.print && rgate.dir_read && rgate.dir_write);
    }

    /// A RECORD message field travels on the tuple wire — `[0 tag][slots]`,
    /// each field at its own kind, strings by content — and the receiving
    /// handler reads it with ordinary field access. Driven through a full
    /// program (main spawns both actors) and diffed against the interpreter.
    #[test]
    fn record_message_params_cross_vms_by_content() {
        let src = r#"
type Reading:
    sensor: String
    value: Float
    count: Int

actor Sink:
    console: Console

impl Sink:
    on Entry(r: Reading):
        print(console, r.sensor + "=" + __render(r.value) + "/" + __render(r.count))

actor Source:
    target: Subject

impl Source:
    on Go(n: Int):
        send(target, Entry(Reading(sensor: "temp", value: 1.5, count: n)))

fn main(console: Console):
    let collector = spawn Sink(console)
    let source = spawn Source(collector)
    send(source, Go(7))
"#;
        let module = parser::parse_module(src).expect("parse");
        let (driver, actors, sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let out = System::run_program(&driver, &actors, sigs, specs, &dev_caps()).expect("run program");
        let interp = crate::interpreter::run_with(src, ".", Vec::new()).expect("interp");
        assert_eq!(out, interp, "record messages must match the interpreter");
        assert_eq!(out, vec!["temp=1.5/7"]);
    }

    /// `spawn` INSIDE a handler: delivery takes the spawning actor out of the
    /// table for the duration of the call, so its spawn host import registers
    /// the new VM without deadlocking — a supervisor spawns a fresh Worker
    /// per job, hands it the payload, and the output matches the interpreter.
    #[test]
    fn handlers_spawn_actors_without_deadlock() {
        let src = r#"
actor Worker:
    console: Console

impl Worker:
    on Job(label: String):
        print(console, "worker did " + label)

actor Supervisor:
    console: Console

impl Supervisor:
    on Assign(n: Int):
        let w = spawn Worker(console)
        send(w, Job("task " + __render(n)))

fn main(console: Console):
    let sup = spawn Supervisor(console)
    send(sup, Assign(1))
    send(sup, Assign(2))
"#;
        let module = parser::parse_module(src).expect("parse");
        let (driver, actors, sigs, specs) =
            codegen::compile_system(&module).expect("compile system");
        let out = System::run_program(&driver, &actors, sigs, specs, &dev_caps()).expect("run program");
        let interp = crate::interpreter::run_with(src, ".", Vec::new()).expect("interp");
        assert_eq!(out, interp, "handler spawns must match the interpreter");
        assert_eq!(out, vec!["worker did task 1", "worker did task 2"]);
    }

    /// LIST state persists across messages in host cells: a List(Int) and a
    /// List(String) field both start empty, accumulate via push on each
    /// message (read back as a fresh arena copy, re-stored by content), and
    /// survive the per-message arena reset.
    #[test]
    fn list_state_fields_accumulate_across_messages() {
        let src = r#"
actor Journal:
    console: Console
    var values: List(Int) = [5]
    var labels: List(String) = ["seed"]

impl Journal:
    on Note(n: Int):
        values = list.push(values, n * 10)
        labels = list.push(labels, "v" + __render(n))
        var total = 0
        for v in values:
            total = total + v
        var joined = ""
        for l in labels:
            joined = joined + l + ","
        print(console, __render(total) + " " + joined)
"#;
        let (mut sys, ids) = build(src);
        sys.send(ids["Journal"], "Note", 1).unwrap();
        sys.send(ids["Journal"], "Note", 2).unwrap();
        sys.send(ids["Journal"], "Note", 3).unwrap();
        assert_eq!(
            sys.output(),
            vec!["15 seed,v1,", "35 seed,v1,v2,", "65 seed,v1,v2,v3,"]
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
        print(console, __render((x + y)))
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
        print(console, __render(n))

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
