//! Backend-neutral executable contract for RFC-0161 resumable tasks.
//!
//! This deliberately describes *what* a runtime execution may do, not the
//! closure/CPS representation currently used to implement it. Backends bind
//! these dense IDs only after typed frame and closure-layout lowering succeeds.

use std::collections::VecDeque;

use witchy_syntax::ast::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionShapeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionStateId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameLaneId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationSiteId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueShapeId(pub u32);

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

/// The outcome edge selected by a state is a semantic part of the program,
/// rather than an incidental position in a transition vector.  The compiler
/// computes these tests from typed values; the runtime only observes frame
/// lanes and never re-evaluates source expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionEdgeCondition {
    Always,
    Boolean { lane: FrameLaneId, when: bool },
    Variant { lane: FrameLaneId, tag: u32 },
    SelectArm { operation: OperationSiteId, arm: u32 },
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
    pub condition: ExecutionEdgeCondition,
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
    /// Input lanes are ordered operands, never a source-name lookup.
    pub input_lanes: Vec<FrameLaneId>,
    /// Result lanes receive a committed operation result before its outcome
    /// edge runs.  A send has no result lane; a receive and select normally do.
    pub result_lanes: Vec<FrameLaneId>,
    pub pending_move_lanes: Vec<FrameLaneId>,
    /// Failed probe/registration restores pending moves at this state before
    /// source execution can continue.
    pub fallback_state: Option<ExecutionStateId>,
    pub queue: Option<QueueShapeId>,
    /// Select arms retain source priority and endpoint/payload bindings.  An
    /// empty list means this is not a select operation.
    pub arms: Vec<OperationArm>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueCapacity {
    Rendezvous,
    Fixed(u32),
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadOwnership {
    Inline,
    BufferRoot,
    Boxed,
}

/// A queue shape is a typed runtime object. `payload_layout_fingerprint` is
/// generated after final layout, so source spelling cannot choose a queue ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueShape {
    pub id: QueueShapeId,
    pub capacity: QueueCapacity,
    pub payload_layout_fingerprint: [u8; 32],
    pub ownership: PayloadOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationArm {
    pub ordinal: u32,
    pub operation: ExecutionOperation,
    pub endpoint_lane: FrameLaneId,
    pub input_lanes: Vec<FrameLaneId>,
    pub result_lanes: Vec<FrameLaneId>,
    pub queue: Option<QueueShapeId>,
    pub ownership: PayloadOwnership,
}

/// Cooperative cancellation enters an ordinary typed cleanup state. The state
/// owns its normal lane transitions; cancellation never means dropping a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancellationTransition {
    pub operation: OperationSiteId,
    pub state: ExecutionStateId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedExecutionShape {
    pub id: ExecutionShapeId,
    pub entry: ExecutionStateId,
    pub frame: Vec<FrameLane>,
    /// Ordered completed-value ABI. Runtime bindings may use a direct scalar
    /// return physically, but cannot erase this semantic lane mapping.
    pub result_lanes: Vec<FrameLaneId>,
    pub states: Vec<ExecutionState>,
    pub operations: Vec<OperationSite>,
    pub queues: Vec<QueueShape>,
    pub cancellation: Option<CancellationTransition>,
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
        dense("queue shape", self.queues.iter().map(|queue| queue.id.0))?;
        if self.states.get(self.entry.0 as usize).is_none() {
            return Err("execution entry state is absent".to_string());
        }
        let lane_count = self.frame.len() as u32;
        let state_count = self.states.len() as u32;
        let operation_count = self.operations.len() as u32;
        if self.result_lanes.iter().any(|lane| lane.0 >= lane_count) {
            return Err("execution result binding references absent frame lane".to_string());
        }
        for state in &self.states {
            if state.transitions.is_empty() {
                return Err(format!("execution state {} has no transition", state.id.0));
            }
            for transition in &state.transitions {
                for action in &transition.actions {
                    validate_action(*action, lane_count)?;
                }
                validate_condition(transition.condition, lane_count, &self.operations)?;
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
            for lane in operation.input_lanes.iter().chain(operation.result_lanes.iter()).chain(operation.pending_move_lanes.iter()) {
                if lane.0 >= lane_count { return Err("operation references absent frame lane".to_string()); }
            }
            if operation.fallback_state.is_some_and(|state| state.0 >= state_count) {
                return Err("operation fallback references absent state".to_string());
            }
            if operation.queue.is_some_and(|queue| queue.0 as usize >= self.queues.len()) {
                return Err("operation references absent queue shape".to_string());
            }
            let mut expected_arm = 0_u32;
            for arm in &operation.arms {
                if arm.ordinal != expected_arm { return Err("operation arm IDs must be dense and ordered".to_string()); }
                expected_arm += 1;
                if arm.endpoint_lane.0 >= lane_count || arm.input_lanes.iter().chain(arm.result_lanes.iter()).any(|lane| lane.0 >= lane_count) {
                    return Err("operation arm references absent frame lane".to_string());
                }
                if arm.queue.is_some_and(|queue| queue.0 as usize >= self.queues.len()) {
                    return Err("operation arm references absent queue shape".to_string());
                }
            }
        }
        if let Some(cancellation) = self.cancellation {
            if cancellation.operation.0 >= operation_count || cancellation.state.0 >= state_count {
                return Err("cancellation transition has absent operation or cleanup state".to_string());
            }
            if self.operations[cancellation.operation.0 as usize].kind != ExecutionOperation::Cancellation {
                return Err("cancellation transition must name a cancellation operation".to_string());
            }
        }
        self.validate_must_lane_paths()?;
        Ok(())
    }

    /// Conservatively propagate the initialized subset of only `must` lanes.
    /// A union at a join means an obligation may still be live on at least one
    /// incoming path, so a later terminal edge must transfer or discharge it.
    /// This is monotone and bounded by the number of states times frame lanes;
    /// it does not enumerate execution paths.
    fn validate_must_lane_paths(&self) -> Result<(), String> {
        let must_lanes = self.frame.iter().map(|lane| {
            lane.ownership == LaneOwnership::MustConsume
        }).collect::<Vec<_>>();
        if !must_lanes.iter().any(|must| *must) {
            return Ok(());
        }
        let mut states = vec![None::<Vec<bool>>; self.states.len()];
        let mut entry = self.frame.iter().zip(&must_lanes).map(|(lane, must)| {
            *must && lane.initially_initialized
        }).collect::<Vec<_>>();
        states[self.entry.0 as usize] = Some(std::mem::take(&mut entry));
        let mut pending = VecDeque::from([self.entry.0 as usize]);
        while let Some(state_index) = pending.pop_front() {
            let initialized = states[state_index].as_ref()
                .expect("reachable execution state has a must-lane mask").clone();
            for transition in &self.states[state_index].transitions {
                let next = self.apply_must_actions(initialized.clone(), &must_lanes, &transition.actions)?;
                match transition.outcome {
                    ExecutionOutcome::Continue(state)
                    | ExecutionOutcome::Ready(state)
                    | ExecutionOutcome::Blocked { retry: state, .. } => {
                        let target = state.0 as usize;
                        let changed = match &mut states[target] {
                            Some(existing) => {
                                let mut changed = false;
                                for (existing, incoming) in existing.iter_mut().zip(&next) {
                                    let joined = *existing || *incoming;
                                    changed |= joined != *existing;
                                    *existing = joined;
                                }
                                changed
                            }
                            None => {
                                states[target] = Some(next);
                                true
                            }
                        };
                        if changed { pending.push_back(target); }
                    }
                    ExecutionOutcome::Complete => {
                        for (lane, live) in next.iter().enumerate() {
                            if *live && !self.result_lanes.iter().any(|result| result.0 as usize == lane) {
                                return Err("execution completion leaves a must-consume frame lane undisposed".to_string());
                            }
                        }
                    }
                    ExecutionOutcome::PropagateError => {
                        if next.iter().any(|live| *live) {
                            return Err("execution error propagation leaves a must-consume frame lane undisposed".to_string());
                        }
                    }
                    ExecutionOutcome::Trap => {}
                }
            }
        }
        Ok(())
    }

    fn apply_must_actions(
        &self,
        mut initialized: Vec<bool>,
        must_lanes: &[bool],
        actions: &[LaneAction],
    ) -> Result<Vec<bool>, String> {
        for action in actions {
            match *action {
                LaneAction::Copy { from, to } => {
                    if must_lanes[from.0 as usize] {
                        return Err("execution transition copies a must-consume frame lane".to_string());
                    }
                    if must_lanes[to.0 as usize] {
                        return Err("execution transition writes a non-must value into a must-consume frame lane".to_string());
                    }
                }
                LaneAction::Move { from, to } => {
                    if must_lanes[from.0 as usize] && !initialized[from.0 as usize] {
                        return Err("execution transition moves an uninitialized must-consume frame lane".to_string());
                    }
                    if must_lanes[from.0 as usize] != must_lanes[to.0 as usize] {
                        return Err("execution transition changes must-consume lane ownership".to_string());
                    }
                    if must_lanes[from.0 as usize] {
                        initialized[from.0 as usize] = false;
                        initialized[to.0 as usize] = true;
                    }
                }
                LaneAction::Write { to } if must_lanes[to.0 as usize] => initialized[to.0 as usize] = true,
                LaneAction::Clear { lane } | LaneAction::Discharge { lane }
                    if must_lanes[lane.0 as usize] => initialized[lane.0 as usize] = false,
                LaneAction::Write { .. } | LaneAction::Clear { .. } | LaneAction::Discharge { .. } => {}
            }
        }
        Ok(initialized)
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

fn validate_condition(
    condition: ExecutionEdgeCondition,
    lane_count: u32,
    operations: &[OperationSite],
) -> Result<(), String> {
    match condition {
        ExecutionEdgeCondition::Always => Ok(()),
        ExecutionEdgeCondition::Boolean { lane, .. } | ExecutionEdgeCondition::Variant { lane, .. }
            if lane.0 < lane_count => Ok(()),
        ExecutionEdgeCondition::SelectArm { operation, arm } => {
            let operation = operations.get(operation.0 as usize)
                .ok_or_else(|| "execution edge condition references absent operation".to_string())?;
            if operation.kind != ExecutionOperation::Select {
                return Err("execution select-arm condition names a non-select operation".to_string());
            }
            if arm as usize >= operation.arms.len() {
                return Err("execution select-arm condition references absent select arm".to_string());
            }
            Ok(())
        }
        ExecutionEdgeCondition::Boolean { .. } | ExecutionEdgeCondition::Variant { .. } => Err("execution edge condition references absent frame lane".to_string()),
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
            result_lanes: vec![FrameLaneId(0)],
            states: vec![ExecutionState {
                id: ExecutionStateId(0),
                transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
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
                input_lanes: vec![],
                result_lanes: vec![],
                pending_move_lanes: vec![FrameLaneId(0)],
                fallback_state: Some(ExecutionStateId(0)),
                queue: None,
                arms: vec![],
            }],
            queues: vec![],
            cancellation: None,
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

    #[test]
    fn rejects_a_result_lane_outside_the_execution_frame() {
        let mut invalid = shape();
        invalid.result_lanes = vec![FrameLaneId(1)];
        assert!(invalid.validate().expect_err("result lane must exist").contains("result binding references absent"));
    }

    #[test]
    fn rejects_a_terminal_must_lane_without_a_transfer_or_discharge() {
        let mut invalid = shape();
        invalid.frame[0].ownership = LaneOwnership::MustConsume;
        invalid.result_lanes.clear();
        invalid.states[0].transitions[0] = ExecutionTransition {
            condition: ExecutionEdgeCondition::Always,
            actions: vec![],
            outcome: ExecutionOutcome::Complete,
        };
        assert!(invalid.validate().expect_err("must lane must leave terminal").contains("must-consume frame lane undisposed"));
    }

    #[test]
    fn rejects_copying_a_must_lane() {
        let mut invalid = shape();
        invalid.frame[0].ownership = LaneOwnership::MustConsume;
        invalid.states[0].transitions[0].actions = vec![LaneAction::Copy {
            from: FrameLaneId(0),
            to: FrameLaneId(0),
        }];
        assert!(invalid.validate().expect_err("must values are affine").contains("copies a must-consume"));
    }

    #[test]
    fn rejects_a_select_edge_that_does_not_name_a_select_operation_or_arm() {
        let mut invalid = shape();
        invalid.states[0].transitions[0].condition = ExecutionEdgeCondition::SelectArm {
            operation: OperationSiteId(0),
            arm: 0,
        };
        assert!(invalid.validate().expect_err("operation kind is semantic")
            .contains("non-select operation"));

        invalid.operations[0].kind = ExecutionOperation::Select;
        assert!(invalid.validate().expect_err("select arm must exist")
            .contains("absent select arm"));
    }
}
