//! Deterministic interpreter kernel for RFC-0161 typed executions.
//!
//! This is intentionally a runtime for [`TypedExecutionShape`], not an adapter
//! for `std/task` closures. Source integration waits for typed frame lowering;
//! the kernel already establishes the one execution/wait ownership rule.

use witchy_types::typed_execution::{
    ExecutionEdgeCondition, ExecutionOutcome, ExecutionShapeId, ExecutionStateId, FrameLaneId, LaneAction,
    OperationSiteId, TypedExecutionShape,
};
use witchy_types::async_program::{AsyncBackendBinding, AsyncExecutionShape};

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

/// A scheduler/host admission result for one operation. The typed frame owns
/// actual values; the kernel records only which result lanes became initialized
/// and which source-ordered select arm committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationCommit {
    Ready { operation: OperationSiteId },
    SelectArm { operation: OperationSiteId, arm: u32 },
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
        binding: &AsyncBackendBinding,
    ) -> Result<Self, String> {
        Self::new(shape.into_typed_execution(binding)?)
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

    /// Execute a deterministic state turn only when its edge is statically
    /// unambiguous. Branching states must use `turn_selected`; silently taking
    /// vector element zero would destroy source control-flow semantics.
    pub fn turn(&mut self) -> Result<KernelTurn, String> {
        let state = self.shape.states.get(self.state.0 as usize)
            .ok_or_else(|| "execution state vanished after validation".to_string())?;
        if state.transitions.len() != 1 || state.transitions[0].condition != ExecutionEdgeCondition::Always {
            return Err("execution state requires an explicit selected edge".to_string());
        }
        self.turn_selected(0)
    }

    /// The typed scheduler selects a ready branch/arm and supplies its dense
    /// transition index. This preserves arm priority and guards from the
    /// canonical program; a later executor evaluates the condition itself.
    pub fn turn_selected(&mut self, transition_index: usize) -> Result<KernelTurn, String> {
        if self.status != KernelStatus::Ready { return Err("execution is not ready".to_string()); }
        let state = self.shape.states.get(self.state.0 as usize)
            .ok_or_else(|| "execution state vanished after validation".to_string())?;
        let transition = state.transitions.get(transition_index)
            .ok_or_else(|| "execution selected absent transition".to_string())?
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

    /// Commit a ready operation and immediately take its semantically matching
    /// retry transition. A select is selected by its declared arm ordinal, not
    /// a raw transition-vector index. Ordinary operations may resume only on
    /// one unconditional edge; branch conditions still require the compiler's
    /// typed condition evaluator.
    pub fn commit_operation(&mut self, commit: OperationCommit) -> Result<KernelTurn, String> {
        let (operation, selected_arm) = match commit {
            OperationCommit::Ready { operation } => (operation, None),
            OperationCommit::SelectArm { operation, arm } => (operation, Some(arm)),
        };
        let waiting = match self.status {
            KernelStatus::Waiting(waiting) => waiting,
            _ => return Err("commit for execution without an installed wait record".to_string()),
        };
        if waiting != operation {
            return Err("commit operation does not own this execution wait record".to_string());
        }
        let operation_site = self.shape.operations.get(operation.0 as usize)
            .ok_or_else(|| "operation vanished after validation".to_string())?;
        let result_lanes = match selected_arm {
            None => {
                if operation_site.kind == witchy_types::typed_execution::ExecutionOperation::Select {
                    return Err("select commit requires a source-ordered arm".to_string());
                }
                operation_site.result_lanes.clone()
            }
            Some(arm) => operation_site.arms.iter().find(|candidate| candidate.ordinal == arm)
                .ok_or_else(|| "commit selected absent operation arm".to_string())?
                .result_lanes.clone(),
        };
        let transition_index = self.matching_commit_transition(operation, selected_arm)?;
        for lane in result_lanes {
            self.initialized[lane.0 as usize] = true;
        }
        self.status = KernelStatus::Ready;
        self.turn_selected(transition_index)
    }

    /// Abort a probe or registration before it commits. Pending moved payloads
    /// are restored at the operation's declared fallback state, so retrying or
    /// taking another source edge cannot observe an emptied frame lane.
    pub fn abort_operation(&mut self, operation: OperationSiteId) -> Result<(), String> {
        match self.status {
            KernelStatus::Waiting(waiting) if waiting == operation => {}
            KernelStatus::Waiting(_) => return Err("abort operation does not own this execution wait record".to_string()),
            _ => return Err("abort for execution without an installed wait record".to_string()),
        }
        let operation_site = self.shape.operations.get(operation.0 as usize)
            .ok_or_else(|| "operation vanished after validation".to_string())?;
        let fallback = operation_site.fallback_state
            .ok_or_else(|| "operation has no fallback state for aborted registration".to_string())?;
        for lane in &operation_site.pending_move_lanes {
            self.initialized[lane.0 as usize] = true;
        }
        self.state = fallback;
        self.status = KernelStatus::Ready;
        Ok(())
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

    fn matching_commit_transition(
        &self,
        operation: OperationSiteId,
        selected_arm: Option<u32>,
    ) -> Result<usize, String> {
        let state = self.shape.states.get(self.state.0 as usize)
            .ok_or_else(|| "execution state vanished after validation".to_string())?;
        let matching = state.transitions.iter().enumerate().filter_map(|(index, transition)| {
            match (transition.condition, selected_arm) {
                (ExecutionEdgeCondition::Always, None) => Some(index),
                (ExecutionEdgeCondition::SelectArm { operation: edge_operation, arm }, Some(selected_arm))
                    if edge_operation == operation && arm == selected_arm => Some(index),
                _ => None,
            }
        }).collect::<Vec<_>>();
        match matching.as_slice() {
            [index] => Ok(*index),
            [] => Err("operation commit has no matching retry transition".to_string()),
            _ => Err("operation commit has ambiguous retry transitions".to_string()),
        }
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
            result_lanes: vec![FrameLaneId(0)],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![], outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![LaneAction::Discharge { lane: FrameLaneId(0) }], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::ChannelReceive, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: Some(ExecutionStateId(0)), queue: None, arms: vec![] },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: vec![], result_lanes: vec![], pending_move_lanes: vec![], fallback_state: None, queue: None, arms: vec![] },
            ],
            queues: vec![],
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

    #[test]
    fn branching_state_requires_scheduler_selected_edge() {
        let mut branching = shape();
        branching.states[0].transitions = vec![
            ExecutionTransition {
                condition: ExecutionEdgeCondition::Boolean { lane: FrameLaneId(0), when: true },
                actions: vec![],
                outcome: ExecutionOutcome::Complete,
            },
            ExecutionTransition {
                condition: ExecutionEdgeCondition::Boolean { lane: FrameLaneId(0), when: false },
                actions: vec![],
                outcome: ExecutionOutcome::PropagateError,
            },
        ];
        let mut execution = TypedExecution::new(branching).expect("branching shape is valid");
        assert!(execution.turn().expect_err("kernel cannot guess branch").contains("explicit selected edge"));
        assert_eq!(execution.turn_selected(1), Ok(KernelTurn::PropagatedError));
    }

    #[test]
    fn aborted_operation_restores_pending_moves_at_its_fallback_state() {
        let mut execution = TypedExecution::new(shape()).expect("valid execution shape");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        execution.abort_operation(OperationSiteId(0)).expect("abort restores pending move");
        assert_eq!(execution.state(), ExecutionStateId(0));
        assert_eq!(execution.status(), KernelStatus::Ready);
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
    }

    #[test]
    fn commit_resumes_only_through_the_matching_operation_edge() {
        let mut execution = TypedExecution::new(shape()).expect("valid execution shape");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        assert_eq!(
            execution.commit_operation(OperationCommit::Ready { operation: OperationSiteId(0) }),
            Ok(KernelTurn::Complete),
        );
        assert_eq!(execution.status(), KernelStatus::Complete);
    }

    #[test]
    fn select_commit_uses_source_arm_identity_not_transition_position() {
        let mut selecting = shape();
        selecting.operations[0].kind = ExecutionOperation::Select;
        selecting.operations[0].arms = vec![
            witchy_types::typed_execution::OperationArm {
                ordinal: 0,
                operation: ExecutionOperation::ChannelReceive,
                endpoint_lane: FrameLaneId(0),
                input_lanes: vec![],
                result_lanes: vec![FrameLaneId(0)],
                queue: None,
                ownership: witchy_types::typed_execution::PayloadOwnership::Inline,
            },
            witchy_types::typed_execution::OperationArm {
                ordinal: 1,
                operation: ExecutionOperation::ChannelReceive,
                endpoint_lane: FrameLaneId(0),
                input_lanes: vec![],
                result_lanes: vec![FrameLaneId(0)],
                queue: None,
                ownership: witchy_types::typed_execution::PayloadOwnership::Inline,
            },
        ];
        selecting.states[1].transitions = vec![
            ExecutionTransition {
                condition: ExecutionEdgeCondition::SelectArm { operation: OperationSiteId(0), arm: 1 },
                actions: vec![],
                outcome: ExecutionOutcome::PropagateError,
            },
            ExecutionTransition {
                condition: ExecutionEdgeCondition::SelectArm { operation: OperationSiteId(0), arm: 0 },
                actions: vec![],
                outcome: ExecutionOutcome::Complete,
            },
        ];
        let mut execution = TypedExecution::new(selecting).expect("valid select shape");
        assert_eq!(execution.turn(), Ok(KernelTurn::Blocked(OperationSiteId(0))));
        assert_eq!(
            execution.commit_operation(OperationCommit::SelectArm { operation: OperationSiteId(0), arm: 0 }),
            Ok(KernelTurn::Complete),
        );
    }
}
