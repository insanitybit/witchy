//! Deterministic interpreter kernel for RFC-0161 typed executions.
//!
//! This is intentionally a runtime for [`TypedExecutionShape`], not an adapter
//! for `std/task` closures. Source integration waits for typed frame lowering;
//! the kernel already establishes the one execution/wait ownership rule.

use witchy_types::typed_execution::{
    ExecutionOutcome, ExecutionShapeId, ExecutionStateId, FrameLaneId, LaneAction,
    OperationSiteId, TypedExecutionShape,
};
use witchy_types::async_program::{AsyncExecutionShape, AsyncProgramStateId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelStatus {
    Ready,
    Waiting(OperationSiteId),
    Complete,
    PropagatedError,
    Trapped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelTurn {
    Continued(ExecutionStateId),
    BecameReady(ExecutionStateId),
    Blocked(OperationSiteId),
    Complete,
    PropagatedError,
    Trapped,
}

#[derive(Debug, Clone)]
pub struct TypedExecution {
    shape: TypedExecutionShape,
    state: ExecutionStateId,
    initialized: Vec<bool>,
    status: KernelStatus,
    cancellation: Option<(OperationSiteId, ExecutionStateId)>,
}

impl TypedExecution {
    /// Start from the canonical RFC-0161 program shape. This keeps the
    /// interpreter on the same semantic IDs and edge contract as Wasm instead
    /// of accepting a parallel fixture-only scheduler representation.
    pub fn from_async_shape(
        shape: AsyncExecutionShape,
        entry: AsyncProgramStateId,
    ) -> Result<Self, String> {
        Self::new(shape.into_typed_execution(entry)?)
    }

    pub fn new(shape: TypedExecutionShape) -> Result<Self, String> {
        shape.validate()?;
        let initialized = shape.frame.iter().map(|lane| lane.initially_initialized).collect();
        let cancellation = shape.cancellation.map(|transition| (transition.operation, transition.state));
        Ok(Self { state: shape.entry, shape, initialized, status: KernelStatus::Ready, cancellation })
    }

    pub fn shape_id(&self) -> ExecutionShapeId { self.shape.id }
    pub fn state(&self) -> ExecutionStateId { self.state }
    pub fn status(&self) -> KernelStatus { self.status }

    /// Execute one deterministic state turn. The typed lowering must provide
    /// exactly one selected transition for a state before calling this kernel;
    /// the current first-transition policy is therefore a deliberate temporary
    /// binding seam, not source-order guessing.
    pub fn turn(&mut self) -> Result<KernelTurn, String> {
        if self.status != KernelStatus::Ready { return Err("execution is not ready".to_string()); }
        let state = self.shape.states.get(self.state.0 as usize)
            .ok_or_else(|| "execution state vanished after validation".to_string())?;
        let transition = state.transitions.first()
            .ok_or_else(|| "execution state has no selected transition".to_string())?
            .clone();
        for action in &transition.actions { self.apply(*action)?; }
        match transition.outcome {
            ExecutionOutcome::Continue(next) => { self.state = next; Ok(KernelTurn::Continued(next)) }
            ExecutionOutcome::Ready(next) => { self.state = next; Ok(KernelTurn::BecameReady(next)) }
            ExecutionOutcome::Blocked { operation, retry } => {
                self.state = retry;
                self.status = KernelStatus::Waiting(operation);
                Ok(KernelTurn::Blocked(operation))
            }
            ExecutionOutcome::Complete => { self.status = KernelStatus::Complete; Ok(KernelTurn::Complete) }
            ExecutionOutcome::PropagateError => { self.status = KernelStatus::PropagatedError; Ok(KernelTurn::PropagatedError) }
            ExecutionOutcome::Trap => { self.status = KernelStatus::Trapped; Ok(KernelTurn::Trapped) }
        }
    }

    /// Wake the one installed wait record. Stale or duplicate wakeups are
    /// rejected before they can execute an unrelated state.
    pub fn wake(&mut self, operation: OperationSiteId) -> Result<(), String> {
        match self.status {
            KernelStatus::Waiting(waiting) if waiting == operation => { self.status = KernelStatus::Ready; Ok(()) }
            KernelStatus::Waiting(_) => Err("wake operation does not own this execution wait record".to_string()),
            _ => Err("wake for execution without an installed wait record".to_string()),
        }
    }

    /// Request cooperative cancellation. A valid request selects the declared
    /// cancellation operation and cleanup state; it never clears frame lanes.
    pub fn request_cancellation(&mut self) -> Result<bool, String> {
        let Some((_operation, cleanup)) = self.cancellation else {
            return Err("execution shape has no cancellation transition".to_string());
        };
        if self.status == KernelStatus::Complete || self.status == KernelStatus::PropagatedError || self.status == KernelStatus::Trapped {
            return Ok(false);
        }
        if self.state == cleanup && self.status == KernelStatus::Ready { return Ok(false); }
        self.state = cleanup;
        self.status = KernelStatus::Ready;
        Ok(true)
    }

    fn apply(&mut self, action: LaneAction) -> Result<(), String> {
        let lane = |id: FrameLaneId, lanes: &Vec<bool>| lanes.get(id.0 as usize).copied().ok_or_else(|| "execution lane vanished after validation".to_string());
        match action {
            LaneAction::Copy { from, to } => { let _ = lane(from, &self.initialized)?; self.initialized[to.0 as usize] = true; }
            LaneAction::Move { from, to } => { if !lane(from, &self.initialized)? { return Err("move from uninitialized frame lane".to_string()); } self.initialized[from.0 as usize] = false; self.initialized[to.0 as usize] = true; }
            LaneAction::Write { to } => self.initialized[to.0 as usize] = true,
            LaneAction::Clear { lane: id } | LaneAction::Discharge { lane: id } => self.initialized[id.0 as usize] = false,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::ast::Type;
    use witchy_types::typed_execution::{
        CancellationTransition,
        ExecutionOperation, ExecutionState, ExecutionTransition, FrameLane,
        LaneOwnership, OperationSite,
    };

    fn shape() -> TypedExecutionShape {
        TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane { id: FrameLaneId(0), ty: Type::Named("Int".into(), Vec::new()), ownership: LaneOwnership::Move, initially_initialized: true }],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { actions: vec![], outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { actions: vec![LaneAction::Discharge { lane: FrameLaneId(0) }], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::ChannelReceive, pending_move_lanes: vec![] },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, pending_move_lanes: vec![] },
            ],
            cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(1) }),
        }
    }

    #[test]
    fn wake_reuses_the_single_execution_wait_record() {
        let mut execution = TypedExecution::new(shape()).expect("valid execution shape");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        assert_eq!(execution.status(), KernelStatus::Waiting(OperationSiteId(0)));
        assert!(execution.wake(OperationSiteId(1)).is_err(), "stale wake cannot claim another wait");
        execution.wake(OperationSiteId(0)).expect("matching wake claims wait record");
        assert_eq!(execution.turn(), Ok(KernelTurn::Complete));
        assert!(execution.wake(OperationSiteId(0)).is_err(), "completed execution owns no wait record");
    }

    #[test]
    fn cancellation_wakes_into_declared_cleanup_state_without_dropping_lanes() {
        let mut execution = TypedExecution::new(shape()).expect("valid cancellable execution shape");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        assert!(execution.request_cancellation().expect("cancellation is declared"));
        assert_eq!(execution.state(), ExecutionStateId(1));
        assert_eq!(execution.status(), KernelStatus::Ready);
        assert_eq!(execution.turn(), Ok(KernelTurn::Complete));
        assert!(!execution.request_cancellation().expect("completed request is idempotent"));
    }
}
