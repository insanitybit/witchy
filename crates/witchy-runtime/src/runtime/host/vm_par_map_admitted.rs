//! Source-bound scalar parallel map over fresh admitted, zero-grant workers.
use std::sync::Arc;
use wasmtime::{Caller, Error, Linker, Result, StoreLimitsBuilder};

use super::{AdmittedWorkerModule, Capabilities, PendingAdmittedParMap, VmState, WorkerModule,
    spawn_worker};

pub(super) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "vm_par_map_admitted_run", run)?;
    linker.func_wrap("witchy", "vm_par_map_admitted_write", write)?;
    Ok(())
}

fn read_inputs(caller: &mut Caller<'_, VmState>, pointer: i32, length: i32) -> Result<Vec<i64>> {
    let count = usize::try_from(length).map_err(|_| Error::msg("negative parallel-map input count"))?;
    let byte_count = count.checked_mul(8).ok_or_else(|| Error::msg("parallel-map input length overflow"))?;
    let byte_count = i32::try_from(byte_count).map_err(|_| Error::msg("parallel-map input length overflow"))?;
    let memory = crate::runtime::memory_of(caller)?;
    let bytes = crate::runtime::slice(memory.data(&*caller), pointer, byte_count)?;
    let mut result = Vec::new();
    result.try_reserve_exact(count).map_err(|error| Error::msg(format!("parallel-map input allocation: {error}")))?;
    for value in bytes.chunks_exact(8) {
        result.push(i64::from_le_bytes(value.try_into().expect("checked element width")));
    }
    Ok(result)
}

fn run_chunk(program: Arc<AdmittedWorkerModule>, export: &str, inputs: &[i64],
    preempt: bool, parent_memory: usize) -> Result<Vec<i64>> {
    let engine = program.module.engine().clone();
    let (mut store, instance) = spawn_worker(&engine, &WorkerModule::Admitted(program), preempt,
        &Capabilities::none(), true, None, None, StoreLimitsBuilder::new().build())?;
    let worker_memory = instance.get_memory(&mut store, "memory")
        .ok_or_else(|| Error::msg("parallel-map worker lacks memory"))?;
    if worker_memory.data_ptr(&store) as usize == parent_memory {
        return Err(Error::msg("parallel-map worker shares parent memory"));
    }
    let callback = instance.get_typed_func::<i64, i64>(&mut store, export)?;
    let mut results = Vec::with_capacity(inputs.len());
    for &value in inputs { results.push(callback.call(&mut store, value)?); }
    Ok(results)
}

fn run(mut caller: Caller<'_, VmState>, raw_site: i64, pointer: i32, length: i32) -> Result<i32> {
    let site = u32::try_from(raw_site).map_err(|_| Error::msg("invalid admitted parallel-map site"))?;
    if caller.data().pending_admitted_par_map.is_some() {
        return Err(Error::msg("parallel-map result is still pending"));
    }
    let program = match &caller.data().module {
        WorkerModule::Admitted(program) => program.clone(),
        WorkerModule::Unadmitted(_) => return Err(Error::msg("parallel-map requires admitted source site")),
    };
    let mut matches = program.artifact.par_maps().iter().filter(|entry| entry.id() == site);
    let export = matches.next().ok_or_else(|| Error::msg("parallel-map site is not admitted"))?
        .callback_export().to_owned();
    if matches.next().is_some() { return Err(Error::msg("duplicate parallel-map site")); }
    let inputs = read_inputs(&mut caller, pointer, length)?;
    let parent_memory = crate::runtime::memory_of(&mut caller)?.data_ptr(&caller) as usize;
    let preempt = caller.data().preempt;
    let workers = std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1)
        .min(inputs.len()).max(1);
    let chunk_size = inputs.len().div_ceil(workers);
    let mut results = Vec::with_capacity(inputs.len());
    if !inputs.is_empty() {
        std::thread::scope(|scope| -> Result<()> {
            let mut handles = Vec::new();
            for chunk in inputs.chunks(chunk_size) {
                let program = program.clone();
                let export = export.clone();
                handles.push(scope.spawn(move || run_chunk(program, &export, chunk, preempt, parent_memory)));
            }
            for handle in handles {
                results.extend(handle.join().map_err(|_| Error::msg("parallel-map worker panicked"))??);
            }
            Ok(())
        })?;
        caller.data_mut().admitted_zero_grant_worker_launches += workers;
    }
    let count = i32::try_from(results.len()).map_err(|_| Error::msg("parallel-map result count overflow"))?;
    caller.data_mut().pending_admitted_par_map = Some(PendingAdmittedParMap { site, values: results });
    Ok(count)
}

fn write(mut caller: Caller<'_, VmState>, raw_site: i64, pointer: i32, length: i32) -> Result<i32> {
    let site = u32::try_from(raw_site).map_err(|_| Error::msg("invalid admitted parallel-map write site"))?;
    let pending = caller.data_mut().pending_admitted_par_map.take()
        .ok_or_else(|| Error::msg("parallel-map write has no pending run"))?;
    if pending.site != site { return Err(Error::msg("parallel-map write site differs from pending run")) }
    if usize::try_from(length).ok() != Some(pending.values.len()) {
        return Err(Error::msg("parallel-map destination has wrong checked extent"));
    }
    let expected = pending.values.len().checked_mul(8).ok_or_else(|| Error::msg("parallel-map result length overflow"))?;
    let mut bytes = Vec::with_capacity(expected);
    for value in pending.values { bytes.extend_from_slice(&value.to_le_bytes()); }
    let memory = crate::runtime::memory_of(&mut caller)?;
    memory.write(&mut caller, pointer as u32 as usize, &bytes)?;
    Ok(length)
}
