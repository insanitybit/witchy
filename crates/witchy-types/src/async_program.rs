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

/// Cooperative cancellation is an ordinary operation-to-state binding in the
/// canonical program. It cannot be inferred from a backend's cleanup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncCancellationTransition {
    pub operation: AsyncProgramOperationId,
    pub state: AsyncProgramStateId,
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
    pub entry: AsyncProgramStateId,
    /// Ordered result lanes define the completed-value ABI. They are semantic
    /// even when a backend can physically return a scalar directly.
    pub result_lanes: Vec<u32>,
    pub states: Vec<FinalStateBinding>,
    pub operations: Vec<AsyncOperationBinding>,
    pub queues: Vec<QueueShape>,
    pub cancellation: Option<AsyncCancellationTransition>,
    pub dynamic_boundaries: Vec<DynamicAsyncBoundary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncProgram {
    pub schema_version: u32,
    pub semantic_fingerprint: [u8; 32],
    pub execution_shapes: Vec<AsyncExecutionShape>,
}

impl AsyncProgram {
    /// Construct a versioned program and bind its semantic fingerprint to the
    /// complete canonical encoding.  Backends compare this fingerprint with
    /// their final-layout binding before accepting an execution shape.
    pub fn new(execution_shapes: Vec<AsyncExecutionShape>) -> Result<Self, String> {
        let mut program = Self {
            schema_version: ASYNC_PROGRAM_SCHEMA_VERSION,
            semantic_fingerprint: [0; 32],
            execution_shapes,
        };
        program.validate_structure()?;
        program.semantic_fingerprint = program.computed_fingerprint()?;
        Ok(program)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_structure()?;
        if self.semantic_fingerprint != self.computed_fingerprint()? {
            return Err("async-program semantic fingerprint does not match canonical bytes".into());
        }
        Ok(())
    }

    /// Stable, source-name-independent bytes for the semantic program. Types
    /// are represented by the already-final `closure_layout_fingerprint`; no
    /// generated symbol or diagnostic text participates in this encoding.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate_structure()?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.schema_version.to_le_bytes());
        push_len(&mut bytes, self.execution_shapes.len())?;
        for shape in &self.execution_shapes {
            push_u32(&mut bytes, shape.id.0);
            bytes.extend_from_slice(&shape.closure_layout_fingerprint);
            push_len(&mut bytes, shape.frame.len())?;
            for lane in &shape.frame {
                push_u32(&mut bytes, lane.id.0);
                bytes.push(lane_ownership_tag(lane.ownership));
                bytes.push(u8::from(lane.initially_initialized));
            }
            push_u32(&mut bytes, shape.entry.0);
            push_u32s(&mut bytes, &shape.result_lanes)?;
            push_len(&mut bytes, shape.states.len())?;
            for state in &shape.states {
                push_u32(&mut bytes, state.state.0);
                push_u32(&mut bytes, state.callable.0);
                push_u32s(&mut bytes, &state.capture_to_lane)?;
                push_len(&mut bytes, state.edges.len())?;
                for edge in &state.edges {
                    encode_condition(&mut bytes, edge.condition);
                    encode_actions(&mut bytes, &edge.actions)?;
                    encode_outcome(&mut bytes, edge.outcome);
                }
            }
            push_len(&mut bytes, shape.queues.len())?;
            for queue in &shape.queues {
                push_u32(&mut bytes, queue.id.0);
                match queue.capacity {
                    crate::typed_execution::QueueCapacity::Rendezvous => bytes.push(0),
                    crate::typed_execution::QueueCapacity::Fixed(capacity) => { bytes.push(1); push_u32(&mut bytes, capacity); }
                    crate::typed_execution::QueueCapacity::Dynamic => bytes.push(2),
                }
                bytes.extend_from_slice(&queue.payload_layout_fingerprint);
                bytes.push(payload_ownership_tag(queue.ownership));
            }
            push_len(&mut bytes, shape.operations.len())?;
            for operation in &shape.operations {
                push_u32(&mut bytes, operation.id.0);
                bytes.push(operation_tag(operation.kind));
                push_u32s(&mut bytes, &operation.input_lanes)?;
                push_u32s(&mut bytes, &operation.result_lanes)?;
                push_u32s(&mut bytes, &operation.pending_move_lanes)?;
                encode_optional_u32(&mut bytes, operation.fallback_state.map(|state| state.0));
                encode_optional_u32(&mut bytes, operation.queue.map(|queue| queue.0));
                push_len(&mut bytes, operation.arms.len())?;
                for arm in &operation.arms {
                    push_u32(&mut bytes, arm.ordinal);
                    bytes.push(operation_tag(arm.operation));
                    push_u32(&mut bytes, arm.endpoint_lane);
                    push_u32s(&mut bytes, &arm.input_lanes)?;
                    push_u32s(&mut bytes, &arm.result_lanes)?;
                    encode_optional_u32(&mut bytes, arm.queue.map(|queue| queue.0));
                    bytes.push(payload_ownership_tag(arm.ownership));
                }
            }
            match shape.cancellation {
                Some(cancellation) => {
                    bytes.push(1);
                    push_u32(&mut bytes, cancellation.operation.0);
                    push_u32(&mut bytes, cancellation.state.0);
                }
                None => bytes.push(0),
            }
            push_len(&mut bytes, shape.dynamic_boundaries.len())?;
            bytes.extend(shape.dynamic_boundaries.iter().map(|boundary| match boundary {
                DynamicAsyncBoundary::DynamicAsyncCall => 0,
                DynamicAsyncBoundary::DynamicSelectEndpoint => 1,
                DynamicAsyncBoundary::OpaqueHostOperation => 2,
                DynamicAsyncBoundary::UnsupportedTypedOperation => 3,
            }));
        }
        Ok(bytes)
    }

    pub fn computed_fingerprint(&self) -> Result<[u8; 32], String> {
        Ok(*blake3::hash(&self.canonical_bytes()?).as_bytes())
    }

    fn validate_structure(&self) -> Result<(), String> {
        if self.schema_version != ASYNC_PROGRAM_SCHEMA_VERSION {
            return Err("unsupported async-program schema version".into());
        }
        for (expected, shape) in self.execution_shapes.iter().enumerate() {
            if shape.id.0 != expected as u32 { return Err("async execution shape IDs must be dense".into()); }
            for (lane, field) in shape.frame.iter().enumerate() {
                if field.id.0 != lane as u32 { return Err("async frame lane IDs must be dense".into()); }
            }
            if shape.entry.0 as usize >= shape.states.len() {
                return Err("async execution entry references absent state".into());
            }
            if shape.result_lanes.iter().any(|lane| *lane as usize >= shape.frame.len()) {
                return Err("async result binding references absent frame lane".into());
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
            if let Some(cancellation) = shape.cancellation {
                if cancellation.operation.0 as usize >= shape.operations.len()
                    || cancellation.state.0 as usize >= shape.states.len()
                {
                    return Err("async cancellation references an absent operation or state".into());
                }
                if shape.operations[cancellation.operation.0 as usize].kind
                    != ExecutionOperation::Cancellation
                {
                    return Err("async cancellation must name a cancellation operation".into());
                }
            }
        }
        Ok(())
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) { bytes.extend_from_slice(&value.to_le_bytes()); }
fn push_len(bytes: &mut Vec<u8>, value: usize) -> Result<(), String> {
    push_u32(bytes, u32::try_from(value).map_err(|_| "async-program canonical length exceeds u32")?);
    Ok(())
}
fn push_u32s(bytes: &mut Vec<u8>, values: &[u32]) -> Result<(), String> {
    push_len(bytes, values.len())?;
    for value in values { push_u32(bytes, *value); }
    Ok(())
}
fn encode_optional_u32(bytes: &mut Vec<u8>, value: Option<u32>) {
    match value { Some(value) => { bytes.push(1); push_u32(bytes, value); }, None => bytes.push(0) }
}
fn lane_ownership_tag(ownership: crate::typed_execution::LaneOwnership) -> u8 {
    match ownership { crate::typed_execution::LaneOwnership::Copy => 0, crate::typed_execution::LaneOwnership::Move => 1, crate::typed_execution::LaneOwnership::MustConsume => 2 }
}
fn payload_ownership_tag(ownership: PayloadOwnership) -> u8 {
    match ownership { PayloadOwnership::Inline => 0, PayloadOwnership::BufferRoot => 1, PayloadOwnership::Boxed => 2 }
}
fn operation_tag(operation: ExecutionOperation) -> u8 {
    match operation { ExecutionOperation::ChannelSend => 0, ExecutionOperation::ChannelReceive => 1, ExecutionOperation::Select => 2, ExecutionOperation::Spawn => 3, ExecutionOperation::Join => 4, ExecutionOperation::Host => 5, ExecutionOperation::Yield => 6, ExecutionOperation::Cancellation => 7 }
}
fn encode_condition(bytes: &mut Vec<u8>, condition: ExecutionEdgeCondition) {
    match condition {
        ExecutionEdgeCondition::Always => bytes.push(0),
        ExecutionEdgeCondition::Boolean { lane, when } => { bytes.push(1); push_u32(bytes, lane.0); bytes.push(u8::from(when)); }
        ExecutionEdgeCondition::Variant { lane, tag } => { bytes.push(2); push_u32(bytes, lane.0); push_u32(bytes, tag); }
        ExecutionEdgeCondition::SelectArm { operation, arm } => { bytes.push(3); push_u32(bytes, operation.0); push_u32(bytes, arm); }
    }
}
fn encode_actions(bytes: &mut Vec<u8>, actions: &[LaneAction]) -> Result<(), String> {
    push_len(bytes, actions.len())?;
    for action in actions {
        match action {
            LaneAction::Copy { from, to } => { bytes.push(0); push_u32(bytes, from.0); push_u32(bytes, to.0); }
            LaneAction::Move { from, to } => { bytes.push(1); push_u32(bytes, from.0); push_u32(bytes, to.0); }
            LaneAction::Write { to } => { bytes.push(2); push_u32(bytes, to.0); }
            LaneAction::Clear { lane } => { bytes.push(3); push_u32(bytes, lane.0); }
            LaneAction::Discharge { lane } => { bytes.push(4); push_u32(bytes, lane.0); }
        }
    }
    Ok(())
}
fn encode_outcome(bytes: &mut Vec<u8>, outcome: ExecutionOutcome) {
    match outcome {
        ExecutionOutcome::Continue(state) => { bytes.push(0); push_u32(bytes, state.0); }
        ExecutionOutcome::Ready(state) => { bytes.push(1); push_u32(bytes, state.0); }
        ExecutionOutcome::Blocked { operation, retry } => { bytes.push(2); push_u32(bytes, operation.0); push_u32(bytes, retry.0); }
        ExecutionOutcome::Complete => bytes.push(3),
        ExecutionOutcome::PropagateError => bytes.push(4),
        ExecutionOutcome::Trap => bytes.push(5),
    }
}

impl AsyncExecutionShape {
    /// Bind a canonical shape into the shared interpreter/Wasm execution
    /// contract. The caller supplies the checked entry state; no backend may
    /// infer one from a generated callable name.
    pub fn into_typed_execution(self) -> Result<TypedExecutionShape, String> {
        let entry = self.entry;
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
            cancellation: self.cancellation.map(|cancellation| crate::typed_execution::CancellationTransition {
                operation: OperationSiteId(cancellation.operation.0),
                state: ExecutionStateId(cancellation.state.0),
            }),
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
            entry: AsyncProgramStateId(0),
            result_lanes: vec![0],
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
            cancellation: None,
            dynamic_boundaries: vec![],
        }
    }

    #[test]
    fn conversion_preserves_ordered_select_and_rollback_bindings() {
        let shape = shape();
        let execution = shape.into_typed_execution().expect("canonical shape binds");
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
        assert!(shape.into_typed_execution().expect_err("arm ordering is semantic").contains("dense and ordered"));
    }

    #[test]
    fn semantic_fingerprint_covers_arm_operation_and_queue_ownership() {
        let program = AsyncProgram::new(vec![shape()]).expect("canonical program");
        program.validate().expect("fresh fingerprint matches canonical bytes");
        let mut changed = program.clone();
        changed.execution_shapes[0].operations[0].arms[0].operation = ExecutionOperation::ChannelSend;
        assert!(changed.validate().expect_err("arm operation changed").contains("fingerprint"));

        let mut queue_changed = program;
        queue_changed.execution_shapes[0].queues[0].ownership = PayloadOwnership::BufferRoot;
        assert!(queue_changed.validate().expect_err("queue ownership changed").contains("fingerprint"));
    }

    #[test]
    fn semantic_fingerprint_covers_entry_result_and_cancellation_bindings() {
        let mut shape = shape();
        shape.operations[0].kind = ExecutionOperation::Cancellation;
        shape.cancellation = Some(AsyncCancellationTransition {
            operation: AsyncProgramOperationId(0),
            state: AsyncProgramStateId(0),
        });
        let program = AsyncProgram::new(vec![shape]).expect("canonical program");
        let mut changed = program.clone();
        changed.execution_shapes[0].result_lanes.clear();
        assert!(changed.validate().expect_err("result ABI changed").contains("fingerprint"));
        let mut changed = program;
        changed.execution_shapes[0].entry = AsyncProgramStateId(1);
        assert!(changed.validate().expect_err("entry changed").contains("absent state"));
    }
}
