//! Typed host-operation wait protocol for RFC-0161 executions.
//!
//! The embedder supplies only an opaque operation identity and completion
//! payload.  Execution, operation, generation, result lanes, and cancellation
//! remain owned by the common typed execution kernel.

use std::collections::BTreeMap;

use witchy_types::typed_execution::{ExecutionOperation, OperationSiteId};

use crate::typed_async::{KernelStatus, KernelTurn, OperationCommit, TypedExecution};

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
    waits: BTreeMap<HostOperationId, HostWait>,
    executions: BTreeMap<u64, TypedExecution>,
}

impl TypedHostRuntime {
    pub fn install_execution(&mut self, id: u64, execution: TypedExecution) -> Result<(), String> {
        if self.executions.insert(id, execution).is_some() {
            return Err("typed host runtime already has this execution ID".into());
        }
        Ok(())
    }

    pub fn execution(&self, id: u64) -> Option<&TypedExecution> { self.executions.get(&id) }

    pub fn submit(
        &mut self,
        execution: u64,
        generation: u64,
        host: HostOperationId,
    ) -> Result<HostSubmission, String> {
        if self.waits.contains_key(&host) {
            return Err("host operation already has a typed wait registration".into());
        }
        let (operation, kind) = self.executions.get(&execution)
            .ok_or_else(|| "typed host runtime has no such execution".to_string())?
            .waiting_operation()?;
        if kind != ExecutionOperation::Host {
            return Err("execution wait is not a typed host operation".into());
        }
        if self.waits.values().any(|wait| wait.execution == execution && wait.operation == operation) {
            return Err("execution already has a typed host wait registration".into());
        }
        self.waits.insert(host, HostWait { execution, operation, generation, host });
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
        let Some(wait) = self.waits.get(&host).copied() else { return Ok(None) };
        if wait.generation != generation { return Ok(None); }
        self.waits.remove(&host);
        let turn = self.executions.get_mut(&wait.execution)
            .ok_or_else(|| "host wait execution vanished".to_string())?
            .commit_operation(OperationCommit::Ready { operation: wait.operation })?;
        Ok(Some(HostCompletion {
            turn,
            delivery: HostDelivery { execution: wait.execution, operation: wait.operation, payload },
        }))
    }

    /// Acknowledge cancellation only when the caller claims the installed
    /// generation.  The operation is unlinked before typed cleanup starts.
    pub fn request_cancellation(&mut self, execution: u64, generation: u64) -> Result<bool, String> {
        let current = self.executions.get(&execution)
            .ok_or_else(|| "typed host runtime has no such execution".to_string())?;
        if !current.has_cancellation_transition() {
            return Err("execution shape has no cancellation transition".into());
        }
        if !matches!(current.status(), KernelStatus::Waiting(_)) {
            return self.executions.get_mut(&execution)
                .expect("execution was checked above")
                .request_cancellation();
        }
        let Some((host, _wait)) = self.waits.iter().find(|(_, wait)| {
            wait.execution == execution && wait.generation == generation
        }).map(|(host, wait)| (*host, *wait)) else {
            return Ok(false);
        };
        self.waits.remove(&host);
        self.executions.get_mut(&execution)
            .expect("execution was checked above")
            .request_cancellation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::ast::Type;
    use witchy_types::typed_execution::{
        CancellationTransition, ExecutionOutcome, ExecutionShapeId, ExecutionState,
        ExecutionStateId, ExecutionTransition, FrameLane, FrameLaneId, LaneOwnership,
        OperationSite, TypedExecutionShape,
    };

    fn execution_with_operation(operation: ExecutionOperation) -> TypedExecution {
        let shape = TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane {
                id: FrameLaneId(0),
                ty: Type::Named("Int".into(), Vec::new()),
                ownership: LaneOwnership::Move,
                initially_initialized: true,
            }],
            result_lanes: vec![FrameLaneId(0)],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition {
                    condition: witchy_types::typed_execution::ExecutionEdgeCondition::Always,
                    actions: vec![],
                    outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) },
                }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition {
                    condition: witchy_types::typed_execution::ExecutionEdgeCondition::Always,
                    actions: vec![], outcome: ExecutionOutcome::Complete,
                }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: operation, input_lanes: vec![], result_lanes: vec![FrameLaneId(0)], pending_move_lanes: vec![], fallback_state: Some(ExecutionStateId(0)), queue: None, arms: vec![] },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: None, queue: None, arms: vec![] },
            ],
            queues: vec![],
            cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(1) }),
        };
        let mut execution = TypedExecution::new(shape).expect("valid host execution");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        execution
    }

    fn execution() -> TypedExecution { execution_with_operation(ExecutionOperation::Host) }

    #[test]
    fn host_adapter_rejects_a_non_host_wait() {
        let mut runtime = TypedHostRuntime::default();
        runtime.install_execution(1, execution_with_operation(ExecutionOperation::Yield)).expect("execution");
        assert_eq!(
            runtime.submit(1, 1, HostOperationId(1)),
            Err("execution wait is not a typed host operation".into()),
        );
    }

    #[test]
    fn host_completion_claims_only_the_matching_generation() {
        let mut runtime = TypedHostRuntime::default();
        runtime.install_execution(1, execution()).expect("execution");
        assert_eq!(runtime.submit(1, 3, HostOperationId(9)), Ok(HostSubmission::Blocked));
        assert_eq!(runtime.complete(HostOperationId(9), 2, "stale"), Ok(None));
        assert_eq!(
            runtime.complete(HostOperationId(9), 3, "typed result"),
            Ok(Some(HostCompletion {
                turn: KernelTurn::Complete,
                delivery: HostDelivery { execution: 1, operation: OperationSiteId(0), payload: "typed result" },
            })),
        );
        assert_eq!(runtime.complete(HostOperationId(9), 3, "duplicate"), Ok(None));
    }

    #[test]
    fn late_completion_cannot_claim_a_reused_host_handle() {
        let mut runtime = TypedHostRuntime::default();
        runtime.install_execution(1, execution()).expect("first execution");
        runtime.install_execution(2, execution()).expect("second execution");
        runtime.submit(1, 3, HostOperationId(9)).expect("first wait");
        runtime.complete(HostOperationId(9), 3, vec![7_u8]).expect("first completion");
        runtime.submit(2, 4, HostOperationId(9)).expect("reused host handle");
        assert_eq!(runtime.complete(HostOperationId(9), 3, vec![8_u8]), Ok(None));
        assert_eq!(runtime.execution(2).expect("second execution").status(), KernelStatus::Waiting(OperationSiteId(0)));
        assert!(matches!(
            runtime.complete(HostOperationId(9), 4, vec![9_u8]),
            Ok(Some(HostCompletion { delivery: HostDelivery { execution: 2, .. }, .. }))
        ));
    }

    #[test]
    fn cancellation_unlinks_host_wait_before_cleanup() {
        let mut runtime = TypedHostRuntime::default();
        runtime.install_execution(1, execution()).expect("execution");
        assert_eq!(runtime.submit(1, 4, HostOperationId(1)), Ok(HostSubmission::Blocked));
        assert!(!runtime.request_cancellation(1, 3).expect("stale cancellation"));
        assert!(runtime.request_cancellation(1, 4).expect("matching cancellation"));
        assert_eq!(runtime.complete(HostOperationId(1), 4, ()), Ok(None));
        assert_eq!(runtime.execution(1).expect("execution").state(), ExecutionStateId(1));
    }
}
