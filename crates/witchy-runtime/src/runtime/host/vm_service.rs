//! Canonical RFC-0032 services. Only sealed admitted site recipes launch a
//! callback; externrefs carry actual Rust resources, never integer handles.
use crate::runtime::GuestAddress;

use std::sync::{Arc, Mutex};
use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted, StoreLimitsBuilder, TypedFunc};
use super::{spawn_worker, Capabilities, Vm, VmState, WorkerModule};
use crate::runtime::VmServiceMetrics;

#[derive(Clone)]
struct Service {
    state: Arc<Mutex<ServiceState>>,
    metrics: Arc<Mutex<VmServiceMetrics>>,
}

enum ServiceState {
    Ready(Box<Worker>),
    Failed,
    Closed,
}

struct Worker {
    vm: Vm,
    callback: TypedFunc<(i32, i32), i32>,
    /// Exactly one host-held owned reference, moved into the next invocation.
    /// None while the callback owns the arguments, including on a guest trap.
    state: Option<StateReference>,
    metrics: Arc<Mutex<VmServiceMetrics>>,
}

struct StateReference {
    pointer: u32,
    reference_counted: bool,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.live_workers -= 1;
            if self.state.take().is_some_and(|state| state.reference_counted) {
                metrics.live_state_roots -= 1;
                metrics.retired_state_roots += 1;
            }
        }
        // Dropping the Store destroys its final host-held state and any
        // aborted guest allocations. This is not a zero-child-leak assertion.
    }
}

#[derive(Clone)]
struct Response(Arc<[u8]>);

pub(super) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "vm_service_start", start::<P>)?;
    linker.func_wrap("witchy", "vm_service_step", step::<P>)?;
    linker.func_wrap("witchy", "vm_service_response_size", response_size)?;
    linker.func_wrap("witchy", "vm_service_response_into", response_into::<P>)?;
    linker.func_wrap("witchy", "vm_service_close", close)?;
    Ok(())
}

fn resource<T: Clone + 'static>(caller: &Caller<'_, VmState>, reference: Option<Rooted<ExternRef>>, kind: &str) -> Result<T> {
    reference.ok_or_else(|| Error::msg(format!("null {kind} reference")))?
        .data(caller)?.and_then(|data| data.downcast_ref::<T>()).cloned()
        .ok_or_else(|| Error::msg(format!("wrong runtime {kind} kind")))
}

fn range<P: GuestAddress>(length: usize, pointer: P, count: i32) -> Result<std::ops::Range<usize>> {
    let start = pointer.offset();
    let end = start.checked_add(count as u32 as usize)
        .filter(|end| *end <= length)
        .ok_or_else(|| Error::msg("service slice outside guest memory"))?;
    Ok(start..end)
}

fn input<P: GuestAddress>(caller: &mut Caller<'_, VmState>, pointer: P, length: i32) -> Result<Vec<u8>> {
    let memory = crate::runtime::memory_of(caller)?;
    let data = memory.data(&*caller);
    Ok(data[range(data.len(), pointer, length)?].to_vec())
}

fn start<P: GuestAddress>(mut caller: Caller<'_, VmState>, site: i64, pointer: P, length: i32) -> Result<Option<Rooted<ExternRef>>> {
    let site = u32::try_from(site).map_err(|_| Error::msg("invalid admitted service site ID"))?;
    let WorkerModule::Admitted(program) = &caller.data().module else {
        return Err(Error::msg("service requires sealed admitted worker metadata"));
    };
    let program = program.clone();
    let mut matches = program.artifact.vm_services().iter().filter(|recipe| recipe.id() == site);
    let callback_export = matches.next().ok_or_else(|| Error::msg("service site is not admitted"))?
        .callback_export().to_owned();
    if matches.next().is_some() { return Err(Error::msg("duplicate admitted service site")); }
    let init = input(&mut caller, pointer, length)?;
    let parent_memory = crate::runtime::memory_of(&mut caller)?.data_ptr(&caller);
    let engine = caller.data().engine.clone();
    let preempt = caller.data().preempt;
    let metrics = caller.data().vm_services.clone();
    let (store, instance) = spawn_worker(&engine, &WorkerModule::Admitted(program), preempt,
        &Capabilities::default(), true, None, None, StoreLimitsBuilder::new().build())?;
    let mut vm = Vm { store, instance, aborted: false, entry: None };
    let child_memory = vm.instance.get_memory(&mut vm.store, "memory")
        .ok_or_else(|| Error::msg("service callback lacks memory"))?;
    if child_memory.data_ptr(&vm.store) == parent_memory {
        return Err(Error::msg("service worker does not have isolated memory"));
    }
    // This is the exact export from the admitted recipe, never a fallback
    // export lookup or guest-selected table index.
    let callback = vm.instance.get_typed_func::<(i32, i32), i32>(&mut vm.store, &callback_export)?;
    let state = vm.materialize_owned_bytes(&init)?;
    {
        let mut observed = metrics.lock().map_err(|_| Error::msg("service metrics poisoned"))?;
        observed.started += 1;
        observed.live_workers += 1;
        observed.live_state_roots += 1;
        observed.isolated_memories += 1;
    }
    let worker = Worker { vm, callback, state: Some(StateReference {
        pointer: state, reference_counted: true,
    }), metrics: metrics.clone() };
    ExternRef::new(&mut caller, Service {
        state: Arc::new(Mutex::new(ServiceState::Ready(Box::new(worker)))), metrics,
    }).map(Some)
}

fn step<P: GuestAddress>(mut caller: Caller<'_, VmState>, reference: Option<Rooted<ExternRef>>, pointer: P, length: i32) -> Result<Option<Rooted<ExternRef>>> {
    let service: Service = resource(&caller, reference, "Service")?;
    let mut state = service.state.lock().map_err(|_| Error::msg("service state poisoned"))?;
    // Remove the live worker before any fallible read, allocation, invocation,
    // or response sealing. Every error leaves this token permanently failed.
    let worker = match std::mem::replace(&mut *state, ServiceState::Failed) {
        ServiceState::Ready(worker) => worker,
        ServiceState::Failed => return Err(Error::msg("service is terminal after failure")),
        ServiceState::Closed => {
            *state = ServiceState::Closed;
            return Err(Error::msg("service is closed"));
        }
    };
    let mut worker = worker;
    let result = (|| {
        let request = input(&mut caller, pointer, length)?;
        let request = worker.vm.materialize_owned_bytes(&request)?;
        let previous = worker.state.take().ok_or_else(|| Error::msg("service state already transferred"))?;
        if previous.reference_counted {
            service.metrics.lock().map_err(|_| Error::msg("service metrics poisoned"))?.live_state_roots -= 1;
        }
        let result = worker.callback.call(&mut worker.vm.store, (previous.pointer as i32, request as i32))
            .map_err(|error| { worker.vm.aborted = true; error })? as u32;
        let (bytes, reference_counted) = worker.vm.copy_owned_bytes(result)?;
        worker.state = Some(StateReference { pointer: result, reference_counted });
        if reference_counted {
            service.metrics.lock().map_err(|_| Error::msg("service metrics poisoned"))?.live_state_roots += 1;
        }
        let response = Response(bytes.into());
        ExternRef::new(&mut caller, response)
    })();
    match result {
        Ok(response) => {
            service.metrics.lock().map_err(|_| Error::msg("service metrics poisoned"))?.successful_steps += 1;
            *state = ServiceState::Ready(worker);
            Ok(Some(response))
        }
        Err(error) => {
            if let Ok(mut metrics) = service.metrics.lock() { metrics.failed += 1; }
            Err(error)
        }
    }
}

fn response_size(caller: Caller<'_, VmState>, reference: Option<Rooted<ExternRef>>) -> Result<i32> {
    let response: Response = resource(&caller, reference, "Response")?;
    Ok(u32::try_from(response.0.len()).map_err(|_| Error::msg("service response exceeds wasm32"))? as i32)
}

fn response_into<P: GuestAddress>(mut caller: Caller<'_, VmState>, reference: Option<Rooted<ExternRef>>, pointer: P, capacity: i32) -> Result<i32> {
    let response: Response = resource(&caller, reference, "Response")?;
    let required = u32::try_from(response.0.len()).map_err(|_| Error::msg("service response exceeds wasm32"))?;
    let memory = crate::runtime::memory_of(&mut caller)?;
    let target = range(memory.data_size(&caller), pointer, capacity)?;
    if (capacity as u32) >= required {
        memory.write(&mut caller, target.start, &response.0)?;
    }
    Ok(required as i32)
}

fn close(caller: Caller<'_, VmState>, reference: Option<Rooted<ExternRef>>) -> Result<()> {
    let service: Service = resource(&caller, reference, "Service")?;
    let mut state = service.state.lock().map_err(|_| Error::msg("service state poisoned"))?;
    if matches!(*state, ServiceState::Closed) {
        return Err(Error::msg("service is already closed"));
    }
    let old = std::mem::replace(&mut *state, ServiceState::Closed);
    service.metrics.lock().map_err(|_| Error::msg("service metrics poisoned"))?.closed += 1;
    drop(old);
    Ok(())
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
#[path = "vm_service_tests.rs"]
pub mod tests;
