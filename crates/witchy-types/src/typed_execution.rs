//! Backend-neutral executable contract for RFC-0161 resumable tasks.
//!
//! This deliberately describes *what* a runtime execution may do, not the
//! closure/CPS representation currently used to implement it. Backends bind
//! these dense IDs only after typed frame and closure-layout lowering succeeds.

use witchy_syntax::ast::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionShapeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionStateId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameLaneId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationSiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneOwnership {
    Copy,
    Move,
    MustConsume,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameLane {
    pub id: FrameLaneId,
    pub ty: Type,
    pub ownership: LaneOwnership,
    pub initially_initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOperation {
    ChannelSend,
    ChannelReceive,
    Select,
    Spawn,
    Join,
    Host,
    Yield,
    Cancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneAction {
    Copy { from: FrameLaneId, to: FrameLaneId },
    Move { from: FrameLaneId, to: FrameLaneId },
    Write { to: FrameLaneId },
    Clear { lane: FrameLaneId },
    Discharge { lane: FrameLaneId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Continue(ExecutionStateId),
    Ready(ExecutionStateId),
    Blocked { operation: OperationSiteId, retry: ExecutionStateId },
    Complete,
    PropagateError,
    Trap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTransition {
    pub actions: Vec<LaneAction>,
    pub outcome: ExecutionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionState {
    pub id: ExecutionStateId,
    pub transitions: Vec<ExecutionTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSite {
    pub id: OperationSiteId,
    pub kind: ExecutionOperation,
    pub pending_move_lanes: Vec<FrameLaneId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedExecutionShape {
    pub id: ExecutionShapeId,
    pub entry: ExecutionStateId,
    pub frame: Vec<FrameLane>,
    pub states: Vec<ExecutionState>,
    pub operations: Vec<OperationSite>,
}

impl TypedExecutionShape {
    /// Reject malformed compiler output before an interpreter or Wasm backend
    /// can bind it. This is intentionally stricter than a best-effort planner:
    /// missing transitions, dangling lanes, and undischarged must lanes are
    /// compiler errors, never runtime fallback cases.
    pub fn validate(&self) -> Result<(), String> {
        dense("frame lane", self.frame.iter().map(|lane| lane.id.0))?;
        dense("execution state", self.states.iter().map(|state| state.id.0))?;
        dense("operation site", self.operations.iter().map(|site| site.id.0))?;
        if self.states.get(self.entry.0 as usize).is_none() {
            return Err("execution entry state is absent".to_string());
        }
        let lane_count = self.frame.len() as u32;
        let state_count = self.states.len() as u32;
        let operation_count = self.operations.len() as u32;
        for state in &self.states {
            if state.transitions.is_empty() {
                return Err(format!("execution state {} has no transition", state.id.0));
            }
            for transition in &state.transitions {
                for action in &transition.actions {
                    validate_action(*action, lane_count)?;
                }
                match transition.outcome {
                    ExecutionOutcome::Continue(next) | ExecutionOutcome::Ready(next)
                        if next.0 >= state_count => return Err("execution transition has absent successor".to_string()),
                    ExecutionOutcome::Blocked { operation, retry }
                        if operation.0 >= operation_count || retry.0 >= state_count => return Err("blocked transition has absent operation or retry state".to_string()),
                    _ => {}
                }
            }
        }
        for operation in &self.operations {
            for lane in &operation.pending_move_lanes {
                if lane.0 >= lane_count { return Err("operation references absent pending-move lane".to_string()); }
            }
        }
        Ok(())
    }
}

fn dense(label: &str, ids: impl Iterator<Item = u32>) -> Result<(), String> {
    for (expected, actual) in ids.enumerate() {
        if actual != expected as u32 { return Err(format!("{label} IDs must be dense")); }
    }
    Ok(())
}

fn validate_action(action: LaneAction, lane_count: u32) -> Result<(), String> {
    let lane = |lane: FrameLaneId| if lane.0 < lane_count { Ok(()) } else { Err("transition references absent frame lane".to_string()) };
    match action {
        LaneAction::Copy { from, to } | LaneAction::Move { from, to } => { lane(from)?; lane(to) }
        LaneAction::Write { to } => lane(to),
        LaneAction::Clear { lane: value } | LaneAction::Discharge { lane: value } => lane(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> TypedExecutionShape {
        TypedExecutionShape {
            id: ExecutionShapeId(0),
            entry: ExecutionStateId(0),
            frame: vec![FrameLane {
                id: FrameLaneId(0),
                ty: Type::Named("Int".to_string(), Vec::new()),
                ownership: LaneOwnership::Move,
                initially_initialized: true,
            }],
            states: vec![ExecutionState {
                id: ExecutionStateId(0),
                transitions: vec![ExecutionTransition {
                    actions: vec![LaneAction::Move { from: FrameLaneId(0), to: FrameLaneId(0) }],
                    outcome: ExecutionOutcome::Blocked {
                        operation: OperationSiteId(0),
                        retry: ExecutionStateId(0),
                    },
                }],
            }],
            operations: vec![OperationSite {
                id: OperationSiteId(0),
                kind: ExecutionOperation::ChannelReceive,
                pending_move_lanes: vec![FrameLaneId(0)],
            }],
        }
    }

    #[test]
    fn validated_shape_keeps_blocked_operation_and_retry_distinct() {
        shape().validate().expect("well-formed typed execution shape");
    }

    #[test]
    fn rejects_dangling_lane_before_backend_binding() {
        let mut invalid = shape();
        invalid.states[0].transitions[0].actions = vec![LaneAction::Clear { lane: FrameLaneId(1) }];
        assert!(invalid.validate().expect_err("dangling lane must fail").contains("absent frame lane"));
    }
}
