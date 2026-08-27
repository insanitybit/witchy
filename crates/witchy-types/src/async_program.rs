//! Canonical, backend-neutral RFC-0161 async-program schema.
//!
//! This is deliberately constructed only from final typed frame layouts.  It
//! has no generated callable names and no legacy scalar-transition fallback.

use crate::typed_async_lowering::FinalCallableIndex;
use crate::typed_execution::{ExecutionEdgeCondition, ExecutionOperation, ExecutionOutcome, ExecutionShapeId, ExecutionState, ExecutionStateId, ExecutionTransition, FrameLane, FrameLaneId, LaneAction, OperationArm, OperationSite, OperationSiteId, PayloadOwnership, QueueShape, QueueShapeId, TypedExecutionShape};

pub const ASYNC_PROGRAM_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsyncProgramStateId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsyncProgramOperationId(pub u32);

/// Final backend binding inputs. Capture fields are ordered physical fields;
/// lane IDs are canonical semantic fields. Backends must reject layout drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalStateBinding {
    pub state: AsyncProgramStateId,
    pub callable: FinalCallableIndex,
    pub capture_to_lane: Vec<u32>,
    pub edges: Vec<AsyncStateEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncStateEdge {
    pub condition: ExecutionEdgeCondition,
    pub actions: Vec<LaneAction>,
    pub outcome: ExecutionOutcome,
}

/// Canonical operation identity. Queue, wait, and backend code bind this ID;
/// they do not rediscover an operation from a generated continuation name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncOperationBinding {
    pub id: AsyncProgramOperationId,
    pub kind: ExecutionOperation,
    pub input_lanes: Vec<u32>,
    pub result_lanes: Vec<u32>,
    pub pending_move_lanes: Vec<u32>,
    pub fallback_state: Option<AsyncProgramStateId>,
    pub queue: Option<QueueShapeId>,
    pub arms: Vec<AsyncOperationArm>,
}

/// Ordered select-arm/endpoint facts.  This is deliberately not a set: arm
/// priority, endpoint direction, payload movement, and result placement are
/// observable scheduler semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncOperationArm {
    pub ordinal: u32,
    pub operation: ExecutionOperation,
    pub endpoint_lane: u32,
    pub input_lanes: Vec<u32>,
    pub result_lanes: Vec<u32>,
    pub queue: Option<QueueShapeId>,
    pub ownership: PayloadOwnership,
}

/// A typed program either lowers an operation or records a closed, stable
/// boundary for the shared dynamic path. Diagnostic/source text lives in the
/// source map, never in the semantic program fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicAsyncBoundary {
    DynamicAsyncCall,
    DynamicSelectEndpoint,
    OpaqueHostOperation,
    UnsupportedTypedOperation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncExecutionShape {
    pub id: ExecutionShapeId,
    pub closure_layout_fingerprint: [u8; 32],
    pub frame: Vec<FrameLane>,
    pub states: Vec<FinalStateBinding>,
    pub operations: Vec<AsyncOperationBinding>,
    pub queues: Vec<QueueShape>,
    pub dynamic_boundaries: Vec<DynamicAsyncBoundary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncProgram {
    pub schema_version: u32,
    pub semantic_fingerprint: [u8; 32],
    pub execution_shapes: Vec<AsyncExecutionShape>,
}

impl AsyncProgram {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ASYNC_PROGRAM_SCHEMA_VERSION {
            return Err("unsupported async-program schema version".into());
        }
        for (expected, shape) in self.execution_shapes.iter().enumerate() {
            if shape.id.0 != expected as u32 { return Err("async execution shape IDs must be dense".into()); }
            for (lane, field) in shape.frame.iter().enumerate() {
                if field.id.0 != lane as u32 { return Err("async frame lane IDs must be dense".into()); }
            }
            for (expected_state, binding) in shape.states.iter().enumerate() {
                if binding.state.0 != expected_state as u32 { return Err("async state IDs must be dense".into()); }
                for lane in &binding.capture_to_lane {
                    if *lane as usize >= shape.frame.len() { return Err("async capture binding references absent frame lane".into()); }
                }
                if binding.edges.is_empty() { return Err("async state has no canonical edge".into()); }
                for edge in &binding.edges {
                    validate_edge_condition(edge.condition, shape.frame.len(), shape.operations.len())?;
                }
            }
            for (expected_queue, queue) in shape.queues.iter().enumerate() {
                if queue.id.0 != expected_queue as u32 { return Err("async queue IDs must be dense".into()); }
            }
            for (expected_operation, operation) in shape.operations.iter().enumerate() {
                if operation.id.0 != expected_operation as u32 { return Err("async operation IDs must be dense".into()); }
                if operation.input_lanes.iter().chain(operation.result_lanes.iter()).chain(operation.pending_move_lanes.iter()).any(|lane| *lane as usize >= shape.frame.len()) {
                    return Err("async operation references absent frame lane".into());
                }
                if operation.fallback_state.is_some_and(|state| state.0 as usize >= shape.states.len()) {
                    return Err("async operation fallback references absent state".into());
                }
                if operation.queue.is_some_and(|queue| queue.0 as usize >= shape.queues.len()) {
                    return Err("async operation references absent queue shape".into());
                }
                for (ordinal, arm) in operation.arms.iter().enumerate() {
                    if arm.ordinal != ordinal as u32 { return Err("async operation arms must preserve dense source order".into()); }
                    if arm.endpoint_lane as usize >= shape.frame.len() || arm.input_lanes.iter().chain(arm.result_lanes.iter()).any(|lane| *lane as usize >= shape.frame.len()) {
                        return Err("async operation arm references absent frame lane".into());
                    }
                    if arm.queue.is_some_and(|queue| queue.0 as usize >= shape.queues.len()) {
                        return Err("async operation arm references absent queue shape".into());
                    }
                }
            }
        }
        Ok(())
    }
}

impl AsyncExecutionShape {
    /// Bind a canonical shape into the shared interpreter/Wasm execution
    /// contract. The caller supplies the checked entry state; no backend may
    /// infer one from a generated callable name.
    pub fn into_typed_execution(self, entry: AsyncProgramStateId) -> Result<TypedExecutionShape, String> {
        if entry.0 as usize >= self.states.len() { return Err("async execution entry references absent state".into()); }
        let shape = TypedExecutionShape {
            id: self.id,
            entry: ExecutionStateId(entry.0),
            frame: self.frame,
            states: self.states.into_iter().map(|state| ExecutionState {
                id: ExecutionStateId(state.state.0),
                transitions: state.edges.into_iter().map(|edge| ExecutionTransition {
                    condition: edge.condition,
                    actions: edge.actions,
                    outcome: edge.outcome,
                }).collect(),
            }).collect(),
            operations: self.operations.into_iter().map(|operation| OperationSite {
                id: OperationSiteId(operation.id.0),
                kind: operation.kind,
                input_lanes: operation.input_lanes.into_iter().map(FrameLaneId).collect(),
                result_lanes: operation.result_lanes.into_iter().map(FrameLaneId).collect(),
                pending_move_lanes: operation.pending_move_lanes.into_iter().map(crate::typed_execution::FrameLaneId).collect(),
                fallback_state: operation.fallback_state.map(|state| ExecutionStateId(state.0)),
                queue: operation.queue,
                arms: operation.arms.into_iter().map(|arm| OperationArm {
                    ordinal: arm.ordinal,
                    operation: arm.operation,
                    endpoint_lane: FrameLaneId(arm.endpoint_lane),
                    input_lanes: arm.input_lanes.into_iter().map(FrameLaneId).collect(),
                    result_lanes: arm.result_lanes.into_iter().map(FrameLaneId).collect(),
                    queue: arm.queue,
                    ownership: arm.ownership,
                }).collect(),
            }).collect(),
            queues: self.queues,
            cancellation: None,
        };
        shape.validate()?;
        Ok(shape)
    }
}

fn validate_edge_condition(condition: ExecutionEdgeCondition, lane_count: usize, operation_count: usize) -> Result<(), String> {
    match condition {
        ExecutionEdgeCondition::Always => Ok(()),
        ExecutionEdgeCondition::Boolean { lane, .. } | ExecutionEdgeCondition::Variant { lane, .. }
            if (lane.0 as usize) < lane_count => Ok(()),
        ExecutionEdgeCondition::SelectArm { operation, .. } if (operation.0 as usize) < operation_count => Ok(()),
        ExecutionEdgeCondition::Boolean { .. } | ExecutionEdgeCondition::Variant { .. } => Err("async edge condition references absent frame lane".into()),
        ExecutionEdgeCondition::SelectArm { .. } => Err("async edge condition references absent operation".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_execution::{ExecutionOperation, FrameLaneId, LaneOwnership, QueueCapacity};
    use witchy_syntax::ast::Type;

    fn shape() -> AsyncExecutionShape {
        AsyncExecutionShape {
            id: ExecutionShapeId(0),
            closure_layout_fingerprint: [7; 32],
            frame: vec![FrameLane {
                id: FrameLaneId(0),
                ty: Type::Named("Int".to_string(), vec![]),
                ownership: LaneOwnership::Move,
                initially_initialized: true,
            }],
            states: vec![FinalStateBinding {
                state: AsyncProgramStateId(0),
                callable: FinalCallableIndex(3),
                capture_to_lane: vec![0],
                edges: vec![AsyncStateEdge {
                    condition: ExecutionEdgeCondition::SelectArm { operation: OperationSiteId(0), arm: 1 },
                    actions: vec![],
                    outcome: ExecutionOutcome::Complete,
                }],
            }],
            operations: vec![AsyncOperationBinding {
                id: AsyncProgramOperationId(0),
                kind: ExecutionOperation::Select,
                input_lanes: vec![0],
                result_lanes: vec![0],
                pending_move_lanes: vec![0],
                fallback_state: Some(AsyncProgramStateId(0)),
                queue: Some(QueueShapeId(0)),
                arms: vec![
                    AsyncOperationArm { ordinal: 0, operation: ExecutionOperation::ChannelReceive, endpoint_lane: 0, input_lanes: vec![], result_lanes: vec![0], queue: Some(QueueShapeId(0)), ownership: PayloadOwnership::Inline },
                    AsyncOperationArm { ordinal: 1, operation: ExecutionOperation::ChannelSend, endpoint_lane: 0, input_lanes: vec![0], result_lanes: vec![], queue: Some(QueueShapeId(0)), ownership: PayloadOwnership::Inline },
                ],
            }],
            queues: vec![QueueShape { id: QueueShapeId(0), capacity: QueueCapacity::Fixed(4), payload_layout_fingerprint: [9; 32], ownership: PayloadOwnership::Inline }],
            dynamic_boundaries: vec![],
        }
    }

    #[test]
    fn conversion_preserves_ordered_select_and_rollback_bindings() {
        let shape = shape();
        let execution = shape.into_typed_execution(AsyncProgramStateId(0)).expect("canonical shape binds");
        let operation = &execution.operations[0];
        assert_eq!(operation.fallback_state, Some(ExecutionStateId(0)));
        assert_eq!(operation.arms.len(), 2);
        assert_eq!(operation.arms[0].operation, ExecutionOperation::ChannelReceive);
        assert_eq!(operation.arms[1].operation, ExecutionOperation::ChannelSend);
        assert_eq!(execution.states[0].transitions[0].condition, ExecutionEdgeCondition::SelectArm { operation: OperationSiteId(0), arm: 1 });
    }

    #[test]
    fn rejects_a_select_arm_that_reorders_source_priority() {
        let mut shape = shape();
        shape.operations[0].arms[1].ordinal = 2;
        assert!(shape.into_typed_execution(AsyncProgramStateId(0)).expect_err("arm ordering is semantic").contains("dense and ordered"));
    }
}
