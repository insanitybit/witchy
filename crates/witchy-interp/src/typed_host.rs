//! Typed host-operation wait protocol for RFC-0161 executions.
//!
//! The embedder supplies only an opaque operation identity and completion
//! payload.  Execution, operation, generation, result lanes, and cancellation
//! remain owned by the common typed execution kernel.

use witchy_types::typed_execution::OperationSiteId;

use crate::typed_async::{KernelStatus, KernelTurn, TypedExecution};
use crate::typed_queue::TypedQueueRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostOperationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostWait {
    pub execution: u64,
    pub operation: OperationSiteId,
    pub generation: u64,
    pub host: HostOperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDelivery<P> {
    pub execution: u64,
    pub operation: OperationSiteId,
    /// The typed frame executor owns representation and placement of this
    /// value. The wait kernel intentionally does not erase it to a scalar or
    /// a universal message carrier.
    pub payload: P,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSubmission {
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCompletion<P> {
    pub turn: KernelTurn,
    pub delivery: HostDelivery<P>,
}

/// The interpreter adapter for host readiness.  One host token owns at most
/// one typed wait and an execution owns at most one token, so stale/duplicate
/// completion cannot resume an unrelated continuation.
#[derive(Debug, Default)]
pub struct TypedHostRuntime {
    runtime: TypedQueueRuntime,
}

impl TypedHostRuntime {
    pub fn install_execution(&mut self, id: u64, execution: TypedExecution) -> Result<(), String> {
        self.runtime.install_execution(id, execution)
    }

    pub fn execution(&self, id: u64) -> Option<&TypedExecution> { self.runtime.execution(id) }

    pub fn submit(
        &mut self,
        execution: u64,
        generation: u64,
        host: HostOperationId,
    ) -> Result<HostSubmission, String> {
        self.runtime.submit_host(execution, generation, host.0)?;
        Ok(HostSubmission::Blocked)
    }

    /// Finish an exact host generation.  Stale/duplicate completions return
    /// `Ok(None)` and never alter an execution state.
    pub fn complete<P>(
        &mut self,
        host: HostOperationId,
        generation: u64,
        payload: P,
    ) -> Result<Option<HostCompletion<P>>, String> {
        let Some((execution, operation, turn)) = self.runtime.complete_host(host.0, generation)? else { return Ok(None) };
        Ok(Some(HostCompletion {
            turn,
            delivery: HostDelivery { execution, operation, payload },
        }))
    }

    /// Acknowledge cancellation only when the caller claims the installed
    /// generation.  The operation is unlinked before typed cleanup starts.
    pub fn request_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let current = self.runtime.execution(execution)
            .ok_or_else(|| "typed host runtime has no such execution".to_string())?;
        if !current.has_cancellation_transition() {
            return Err("execution shape has no cancellation transition".into());
        }
        if !matches!(current.status(), KernelStatus::Waiting(_)) {
            return self.runtime.request_cancellation(execution, generation);
        }
        self.runtime.request_host_cancellation(execution, generation)
    }
}
