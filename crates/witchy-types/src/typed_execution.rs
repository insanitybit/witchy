//! Backend-neutral executable contract for RFC-0161 resumable tasks.
//!
//! This deliberately describes *what* a runtime execution may do, not the
//! closure/CPS representation currently used to implement it. Backends bind
//! these dense IDs only after typed frame and closure-layout lowering succeeds.

use std::collections::VecDeque;

use witchy_syntax::ast::Type;
use witchy_canonical_source::source::SourceExecutionMode;

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
    /// Checked declaration mode retained through task execution lowering.
    pub mode: SourceExecutionMode,
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
            match operation.kind {
                ExecutionOperation::ChannelSend | ExecutionOperation::ChannelReceive
                    if operation.queue.is_none() || !operation.arms.is_empty() => {
                    return Err("direct channel operation must have one queue and no select arms".to_string());
                }
                ExecutionOperation::Select if operation.queue.is_some() || operation.arms.is_empty() => {
                    return Err("select operation must have ordered arms and no direct queue".to_string());
                }
                ExecutionOperation::Spawn | ExecutionOperation::Join | ExecutionOperation::Host
                | ExecutionOperation::Yield | ExecutionOperation::Cancellation
                    if operation.queue.is_some() || !operation.arms.is_empty() => {
                    return Err("non-channel operation cannot carry queue or select-arm bindings".to_string());
                }
                _ => {}
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
                match arm.operation {
                    ExecutionOperation::ChannelSend | ExecutionOperation::ChannelReceive if arm.queue.is_none() => {
                        return Err("channel select arm has no queue binding".to_string());
                    }
                    ExecutionOperation::ChannelSend | ExecutionOperation::ChannelReceive => {
                        let queue = &self.queues[arm.queue.expect("checked above").0 as usize];
                        if queue.ownership != arm.ownership {
                            return Err("channel select arm ownership disagrees with queue shape".to_string());
                        }
                    }
                    _ if arm.queue.is_some() => return Err("non-channel select arm has a queue binding".to_string()),
                    _ => {}
                }
            }
        }
        for state in &self.states {
            for transition in &state.transitions {
                if let ExecutionEdgeCondition::SelectArm { operation, arm } = transition.condition {
                    let operation = &self.operations[operation.0 as usize];
                    if operation.kind != ExecutionOperation::Select
                        || operation.arms.get(arm as usize).is_none()
                    {
                        return Err("select-arm edge does not name a declared select arm".to_string());
                    }
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

    /// Propagate both possible empty and live states of each `must` lane.
    /// Joins retain either possibility: a later transfer must prove the lane
    /// live on every incoming path, and a terminal edge must prove it empty on
    /// every incoming path. The finite two-bit state reaches a fixed point.
    fn validate_must_lane_paths(&self) -> Result<(), String> {
        const EMPTY: u8 = 1;
        const LIVE: u8 = 2;
        let must_lanes = self.frame.iter().map(|lane| {
            lane.ownership == LaneOwnership::MustConsume
        }).collect::<Vec<_>>();
        if !must_lanes.iter().any(|must| *must) {
            return Ok(());
        }
        let mut states = vec![None::<Vec<u8>>; self.states.len()];
        let mut entry = self.frame.iter().zip(&must_lanes).map(|(lane, must)| {
            if !*must { 0 } else if lane.initially_initialized { LIVE } else { EMPTY }
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
                                    let joined = *existing | *incoming;
                                    changed |= joined != *existing;
                                    *existing = joined;
                                }
                                changed
                            }
                            None => {
                                states[target] = Some(next.clone());
                                true
                            }
                        };
                        if changed { pending.push_back(target); }
                    }
                    ExecutionOutcome::Complete => {
                        for (lane, live) in next.iter().enumerate() {
                            let result = self.result_lanes.iter().any(|result| result.0 as usize == lane);
                            if *live & LIVE != 0 && (!result || self.cancellation.is_some_and(|cancel| cancel.state.0 as usize == state_index)) {
                                return Err("execution completion leaves a must-consume frame lane undisposed".to_string());
                            }
                            if result && state_index != self.cancellation.map_or(usize::MAX, |cancel| cancel.state.0 as usize)
                                && must_lanes[lane] && *live != LIVE {
                                return Err("execution completion lacks a produced must-consume result".to_string());
                            }
                        }
                    }
                    ExecutionOutcome::PropagateError => {
                        if next.iter().any(|live| *live & LIVE != 0) {
                            return Err("execution error propagation leaves a must-consume frame lane undisposed".to_string());
                        }
                    }
                    ExecutionOutcome::Trap => {}
                }
                if matches!(transition.outcome, ExecutionOutcome::Blocked { .. }) {
                    if let Some(cancel) = self.cancellation {
                        let target = cancel.state.0 as usize;
                        let changed = match &mut states[target] {
                            Some(existing) => {
                                let mut changed = false;
                                for (current, incoming) in existing.iter_mut().zip(&next) {
                                    let joined = *current | *incoming;
                                    changed |= joined != *current;
                                    *current = joined;
                                }
                                changed
                            }
                            None => { states[target] = Some(next); true }
                        };
                        if changed { pending.push_back(target); }
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_must_actions(
        &self,
        mut initialized: Vec<u8>,
        must_lanes: &[bool],
        actions: &[LaneAction],
    ) -> Result<Vec<u8>, String> {
        const EMPTY: u8 = 1;
        const LIVE: u8 = 2;
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
                    if must_lanes[from.0 as usize] && initialized[from.0 as usize] != LIVE {
                        return Err("execution transition moves a must-consume frame lane not live on every path".to_string());
                    }
                    if must_lanes[from.0 as usize] != must_lanes[to.0 as usize] {
                        return Err("execution transition changes must-consume lane ownership".to_string());
                    }
                    if must_lanes[from.0 as usize] {
                        if from != to && initialized[to.0 as usize] & LIVE != 0 {
                            return Err("execution transition overwrites a live must-consume frame lane".to_string());
                        }
                        initialized[from.0 as usize] = EMPTY;
                        initialized[to.0 as usize] = LIVE;
                    }
                }
                LaneAction::Write { to } if must_lanes[to.0 as usize] => {
                    if initialized[to.0 as usize] & LIVE != 0 {
                        return Err("execution transition overwrites a live must-consume frame lane".to_string());
                    }
                    initialized[to.0 as usize] = LIVE;
                }
                LaneAction::Clear { lane } if must_lanes[lane.0 as usize] => {
                    return Err("execution transition clears a must-consume frame lane without disposition".to_string());
                }
                LaneAction::Discharge { lane } if must_lanes[lane.0 as usize] => {
                    return Err("execution transition has no checked runtime disposition proof for a must-consume frame lane".to_string());
                }
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
