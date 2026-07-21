use wasmtime::{
    Caller, Engine, Error, ExternRef, Instance, Linker, Module, Result, Rooted, Store,
    StoreLimits, StoreLimitsBuilder,
};

use super::super::{
    link_capability_imports, memory_of, read_wbytes, read_wbytes_list, slice,
    vmstate_from_caps, Capabilities, DirAuthority, ListenerResource, VmState,
};

/// Register the always-available `vm.*` worker imports (RFC-0032).
/// The par_map staging pair computes re-entrant closure calls and writes the
/// results out — no authority of their own, the closure runs with the module's
/// existing caps. The capability-passing spawns grant the worker only the
/// `Dir` the caller already holds and explicitly passes — a re-grant, no new
/// authority.
pub(in crate::runtime) fn link_vm(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "vm_par_map_run", host_vm_par_map_run)?;
    linker.func_wrap("witchy", "vm_par_map_write", host_vm_par_map_write)?;
    linker.func_wrap("witchy", "vm_par_map_bytes_run", host_vm_par_map_bytes_run)?;
    linker.func_wrap("witchy", "vm_par_map_bytes_write", host_vm_par_map_bytes_write)?;
    linker.func_wrap("witchy", "vm_with_dir_run", host_vm_with_dir_run)?;
    linker.func_wrap("witchy", "vm_serve_run", host_vm_serve_run)?;
    Ok(())
}

/// Register the `server.serve` worker pool spawn (RFC-0032): one worker VM per
/// core, all sharing the bound listener. No authority of its own — workers get
/// the same caps the server already holds.
pub(in crate::runtime) fn link_serve_pool(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "serve_pool", host_serve_pool)?;
    Ok(())
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

/// (RFC-0032) `vm_par_map_run(xs_ptr, code_idx) -> byte_size`: map the capture-free
/// function at `code_idx` over the `List(Int)` at `xs_ptr` across worker VMs
/// (`par_fan_out` + `run_par_chunk`),
/// staging the results for `vm_par_map_write` and returning the byte size of the resulting
/// flat `List(Int)` (`[count][count x i64]`).
fn host_vm_par_map_run(
    mut caller: Caller<'_, VmState>,
    xs_ptr: i32,
    code_idx: i32,
) -> Result<i32> {
    let inputs = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let lb = slice(data, xs_ptr, 4)?;
        let n = i32::from_le_bytes([lb[0], lb[1], lb[2], lb[3]]);
        let mut v = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let s = slice(data, xs_ptr + 4 + 8 * i, 8)?;
            v.push(i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]));
        }
        v
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

/// (RFC-0032) `vm_with_dir_run(dir, code_idx, input_ptr) -> byte_size`: run the
/// capture-free function at `code_idx` on `input` inside an isolated worker VM granted
/// EXACTLY `dir` (its
/// `read`/`write` rights inherited from the parent) and NOTHING else — every other host
/// import traps. Stages the result `Bytes` (`[len][bytes]`) for `fill_pending`. This is
/// the capability-PASSING (Tier B) primitive: a sandboxed worker with attenuated authority.
fn host_vm_with_dir_run(
    mut caller: Caller<'_, VmState>,
    dir_ref: Option<Rooted<ExternRef>>,
    code_idx: i32,
    input_ptr: i32,
) -> Result<i32> {
    let (dir, input) = {
        let dir = super::filesystem::dir_authority_ref(&caller, dir_ref)?;
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let input = read_wbytes(data, input_ptr)?;
        (dir, input)
    };
    let engine = caller.data().engine.clone();
    let module = caller.data().module.clone();
    let preempt = caller.data().preempt;
    let result =
        run_with_dir_worker(&engine, &module, preempt, dir, code_idx, &input)?;
    let mut staged = Vec::with_capacity(4 + result.len());
    staged.extend_from_slice(&(result.len() as i32).to_le_bytes());
    staged.extend_from_slice(&result);
    let size = staged.len() as i32;
    caller.data_mut().pending = Some(staged);
    Ok(size)
}

/// Run `f(dir, input)` on a fresh worker VM granted exactly the one `Dir`. `f` is a
/// two-argument closure (the Dir minted from grant ordinal `0` + the input
/// `Bytes` pointer), invoked through the `__call2` trampoline; the result
/// `Bytes` is read raw out of the worker's memory.
#[allow(clippy::too_many_arguments)]
fn run_with_dir_worker(
    engine: &Engine,
    module: &Module,
    preempt: bool,
    dir: DirAuthority,
    code_idx: i32,
    input: &[u8],
) -> Result<Vec<u8>> {
    let caps = Capabilities {
        // The real authority is installed into `store.data_mut().dirs[0]` below.
        // This synthetic root only makes the worker linker expose the Dir imports.
        dir_root: Some(std::path::PathBuf::from(".")),
        dir_read: dir.rights.read,
        dir_write: dir.rights.write,
        dir_rights: vec![dir.rights],
        ..Capabilities::default()
    };
    let (mut store, instance) =
        spawn_worker(engine, module, preempt, &caps, true, None, StoreLimitsBuilder::new().build())?;
    // Attach the exact granted Dir (including entry policy and backing) to the
    // worker's sole root grant.
    if let Some(d) = store.data_mut().dirs.get_mut(0) {
        *d = dir;
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

/// (RFC-0032) `vm_serve_run(init_ptr, requests_ptr, code_idx) -> byte_size`: a
/// stateful SERVICE on a single long-lived isolated worker VM. The worker is created
/// once and processes the request stream IN ORDER, threading the accumulator `state`
/// through the capture-free handler at `code_idx` and emitting each new state as the response.
/// This is the deterministic, parity-safe realization of cross-VM channels: a worker that
/// processes a message stream with persistent state, lock-step (no nondeterministic
/// interleaving), so the interpreter's sequential scan reproduces the result exactly.
fn host_vm_serve_run(
    mut caller: Caller<'_, VmState>,
    init_ptr: i32,
    requests_ptr: i32,
    code_idx: i32,
) -> Result<i32> {
    let (init, requests) = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        let init = read_wbytes(data, init_ptr)?;
        let requests = read_wbytes_list(data, requests_ptr)?;
        (init, requests)
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

/// (RFC-0032) `vm_par_map_bytes_run(xs_ptr, code_idx) -> byte_size`: the
/// `Bytes` variant of `vm.par_map`. Identical to the `String` variant but the payload
/// is kept as RAW bytes (`Vec<u8>`, no UTF-8 decode), so arbitrary binary survives.
fn host_vm_par_map_bytes_run(
    mut caller: Caller<'_, VmState>,
    xs_ptr: i32,
    code_idx: i32,
) -> Result<i32> {
    let inputs = {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        read_wbytes_list(data, xs_ptr)?
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

pub(in crate::runtime) fn listener_resource_ref(
    caller: &Caller<'_, VmState>,
    l: Option<Rooted<ExternRef>>,
) -> Result<ListenerResource> {
    let l = l.ok_or_else(|| Error::msg("Listener externref is null"))?;
    l.data(caller)?
        .ok_or_else(|| Error::msg("Listener externref has no host data"))?
        .downcast_ref::<ListenerResource>()
        .cloned()
        .ok_or_else(|| Error::msg("Listener externref has wrong host data"))
}

/// (RFC-0032) `serve_pool(listener)`: the `server.serve` worker pool. On the
/// PRIMARY VM it spawns one worker VM per remaining core, each re-running the program
/// (rebuilding the same routes with the same capabilities) but SHARING this bound
/// listener. Every worker `accept`s from the one socket and the kernel load-balances
/// connections, so the server uses all cores. A pool worker (its `worker_listener`
/// already set) is a no-op here — only the primary spawns the pool.
fn host_serve_pool(caller: Caller<'_, VmState>, listener_ref_: Option<Rooted<ExternRef>>) -> Result<()> {
    if caller.data().worker_listener.is_some() {
        return Ok(());
    }
    let ListenerResource { listener, tls } = listener_resource_ref(&caller, listener_ref_)?;
    let listener = (listener, tls);
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
