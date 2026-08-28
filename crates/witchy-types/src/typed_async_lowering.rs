//! Final typed-frame input for RFC-0161 execution lowering.
//!
//! The legacy suspension carrier catalog remains useful historical layout
//! evidence, but it identifies states by generated function spelling and feeds
//! scalar transitions. This module deliberately does neither. It retains the
//! linked source callable identity, final typed callable table index, and exact
//! parameter types as the narrow input from which a canonical `AsyncProgram`
//! can assign frame lanes and backend bindings.

use witchy_syntax::ast::{Convention, Expr, Function, Item, Type};
use std::collections::BTreeMap;
use witchy_syntax::async_surface::AsyncSurfaceId;
use witchy_syntax::suspension::{
    async_surface, async_surface_module, frame_bindings, frame_state, source_suspension, FRAME_ENTRY_ATTRIBUTE,
    FRAME_FUNCTION_ATTRIBUTE,
};

use crate::async_control_flow::{
    AsyncCallableId, ControlPathStep, SourceAsyncControlFlow, SourceAsyncControlFlowInventory,
    SourceAsyncOperationKind, SourceBindingId, SourceBindingKind, SourceExpressionId, source_expression,
};
use crate::pipeline::CheckedModule;
use crate::typeck::{self, TypedModule};
use crate::async_program::{AsyncProgram, FinalTypedAsyncExecution, FinalTypedAsyncStateBinding};
use crate::typed_execution::{CancellationTransition, ExecutionEdgeCondition, ExecutionOperation, ExecutionOutcome,
    ExecutionShapeId, ExecutionState, ExecutionStateId, ExecutionTransition, FrameLane,
    FrameLaneId, LaneOwnership, OperationArm, OperationSite, OperationSiteId, QueueShape, TypedExecutionShape};

/// A final callable-table position. This is an internal backend binding input;
/// it is intentionally not a generated function name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FinalCallableIndex(pub u32);

/// The role of a compiler-formed async state before executable edges are built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypedAsyncStateKind {
    Entry,
    Segment,
}

/// Semantic class of a source async operation after ordinary linking and type
/// checking. This is deliberately a language-operation catalog, not a
/// benchmark recognizer: every call site of one of these public operations is
/// classified uniformly, while ordinary static and dynamic async calls retain
/// their own explicit classes for the shared runtime or a typed rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedAsyncOperationKind {
    ChannelCreate,
    ChannelSend,
    ChannelReceive,
    ChannelSelect,
    Spawn,
    Join,
    Yield,
    CallOperand,
    DynamicCallOperand,
    StaticAsyncCall,
    DynamicAsyncCall,
    Return,
}

/// One typed value carried into a final state. Lane IDs and ownership actions
/// are assigned only when the complete closure layout is known.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncFrameInput {
    pub parameter: u32,
    /// The final callable's declared parameter name. This is layout metadata
    /// from the compiler-produced state, retained so a later lane builder can
    /// map checked source places to actual capture/parameter fields without
    /// inspecting a generated callable name or reconstructing a CPS body.
    pub name: String,
    /// The source declaration carried by this final frame field when the
    /// lowering contract proves one. Entry parameters are structurally aligned
    /// with the source callable; continuation captures remain `None` until
    /// async lowering stamps their lexical binding IDs explicitly.
    pub source_binding: Option<SourceBindingId>,
    pub ty: Type,
    pub convention: Convention,
}

/// A source-addressable final typed async state.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncStateInput {
    pub callable: AsyncCallableId,
    pub local_state: u32,
    pub kind: TypedAsyncStateKind,
    /// The lexical source suspension that resumes into this state. Entry states
    /// and compiler-formed joins have no source suspension identity.
    pub source_suspension: Option<u32>,
    pub final_callable: FinalCallableIndex,
    pub frame: Vec<TypedAsyncFrameInput>,
}

/// A lossless link from one source `await` operation to the compiler-generated
/// continuation state entered after that operation completes. The source
/// operation ordinal is diagnostic/source identity; `final_state` is the typed
/// backend binding. Neither is a generated function name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedAsyncSuspensionBinding {
    pub callable: AsyncCallableId,
    pub source_operation: u32,
    pub final_state: FinalCallableIndex,
}

/// One source operation retained in the final typed-lowering input.  This is
/// deliberately complete, not merely a list of suspension sites: a producer
/// must account for calls and terminal source edges as well as awaits.  Only
/// an await has a continuation state at this stage; the later executable
/// lowering supplies its resolved operands, queue/host binding, and state
/// body without rediscovering source order from CPS expressions.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncSourceOperationBinding {
    pub callable: AsyncCallableId,
    pub source_operation: u32,
    pub kind: SourceAsyncOperationKind,
    pub runtime_kind: TypedAsyncOperationKind,
    pub path: Vec<ControlPathStep>,
    /// A source-coordinate handle for the exact pre-CPS operation expression.
    /// It is intentionally noncanonical transport; final typed lowering uses
    /// it to obtain operands from the retained source item, never from a
    /// generated closure name or scalar transition recognizer.
    pub expression: Option<SourceExpressionId>,
    /// Ordered lexical bindings read by this operation expression. The IDs are
    /// source facts, not variable spellings, so shadowed locals remain distinct
    /// when final closure layout later assigns frame lanes.
    pub operand_bindings: Vec<SourceBindingId>,
    /// Concrete types of source call arguments, in lexical argument order.
    /// These are retained before CPS lowering so executable producers can form
    /// endpoint, payload, and result lanes without rewalking generated code.
    pub operand_types: Vec<Type>,
    /// The finalized operand type captured from the linked pre-CPS typed
    /// projection. A bare `return` has no operand; every other operation must
    /// carry a concrete type before executable frame/lane formation begins.
    pub operand_type: Option<Type>,
    pub continuation: Option<FinalCallableIndex>,
    /// A tail await completes its execution directly and therefore has no
    /// generated continuation state. Keeping this explicit is required for
    /// canonical terminal edges; it must never be reconstructed from CPS.
    pub terminal: bool,
}

/// Compiler-resolved queue identity and layout for one source endpoint. Queue
/// identity is intentionally supplied separately from source syntax: the
/// producer must never invent a capacity or alias based on a callable name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedAsyncQueueBinding {
    pub endpoint: SourceBindingId,
    pub shape: QueueShape,
}

/// One source control selector retained with its concrete checked type.  It is
/// intentionally separate from executable edges: a later CFG builder chooses
/// states and lane actions after final closure layout, but it need not inspect
/// generated CPS expressions to obtain the branch discriminator.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncSourceControlBinding {
    pub callable: AsyncCallableId,
    pub control: u32,
    pub selector: SourceExpressionId,
    pub selector_type: Option<Type>,
}

/// The complete bridge between source control flow and final typed state
/// layouts. It is not executable and has no legacy scalar transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedAsyncLoweringInput {
    pub source: Vec<SourceAsyncControlFlow>,
    pub states: Vec<TypedAsyncStateInput>,
    pub suspensions: Vec<TypedAsyncSuspensionBinding>,
    pub controls: Vec<TypedAsyncSourceControlBinding>,
    pub operations: Vec<TypedAsyncSourceOperationBinding>,
    pub queue_bindings: Vec<TypedAsyncQueueBinding>,
}

impl TypedAsyncLoweringInput {
    /// Produce one canonical program for all source callables that are within
    /// the conservative linear boundary. Shape IDs are assigned in the same
    /// lexical callable order retained by `from_typed`; each callable's final
    /// closure fingerprint must be supplied explicitly.
    pub fn into_linear_program(
        &self,
        closure_layout_fingerprints: &BTreeMap<AsyncCallableId, [u8; 32]>,
    ) -> Result<AsyncProgram, String> {
        let mut executions = Vec::with_capacity(self.source.len());
        for (index, source) in self.source.iter().enumerate() {
            let fingerprint = closure_layout_fingerprints.get(&source.callable).ok_or_else(|| {
                format!("RFC-0161 linear program has no closure fingerprint for {:?}", source.callable)
            })?;
            let input = Self {
                source: vec![source.clone()],
                states: self.states.iter().filter(|state| state.callable == source.callable).cloned().collect(),
                suspensions: self.suspensions.iter().filter(|binding| binding.callable == source.callable).cloned().collect(),
                controls: self.controls.iter().filter(|control| control.callable == source.callable).cloned().collect(),
                operations: self.operations.iter().filter(|operation| operation.callable == source.callable).cloned().collect(),
                queue_bindings: self.queue_bindings.clone(),
            };
            let shape = ExecutionShapeId(u32::try_from(index).map_err(|_| "RFC-0161 linear program shape count exceeds u32")?);
            executions.push(input.into_linear_execution(shape, *fingerprint)?);
        }
        AsyncProgram::from_final_typed_executions(executions)
    }

    /// Lower the fully checked source inventory when it is a single linear
    /// callable whose operations have no value-producing or queue binding
    /// requirements.  This is intentionally conservative: operations whose
    /// queue/host/operand semantics are not represented by this producer are
    /// rejected by name instead of being silently mapped to a different ABI.
    /// The general producer can extend this boundary once those facts are
    /// available from typed lowering.
    pub fn into_linear_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
    ) -> Result<FinalTypedAsyncExecution, String> {
        self.into_linear_execution_with_queues(shape, closure_layout_fingerprint, &self.queue_bindings)
    }

    /// Lower a terminal receive when the compiler has supplied the resolved
    /// queue shape. This is the first queue-aware source path; all other queue
    /// operations remain rejected until their result/pending-move lanes are
    /// resolved by the same explicit metadata contract.
    pub fn into_linear_execution_with_queues(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if self.source.len() != 1 {
            return Err("RFC-0161 linear producer requires exactly one async callable".into());
        }
        let source = &self.source[0];
        if !source.controls.is_empty() {
            if source.controls.len() == 1 && self.operations.iter().all(|operation| {
                operation.callable == source.callable
                    && operation.runtime_kind == TypedAsyncOperationKind::Return
                    && operation.terminal
                    && operation.operand_bindings.is_empty()
                    && operation.operand_type.is_none()
            }) {
                return self.into_boolean_control_execution(shape, closure_layout_fingerprint);
            }
            return Err("RFC-0161 linear producer rejects control-flow selectors until typed edges are bound".into());
        }
        let source_operations = self.operations.iter().filter(|operation| {
            operation.callable == source.callable && !is_nested_terminal_operand(source, operation)
        }).collect::<Vec<_>>();
        let queue_operations = source_operations.iter().copied().filter(|operation| {
            !(operation.runtime_kind == TypedAsyncOperationKind::Return
                && operation.operand_bindings.is_empty()
                && operation.operand_type.is_none())
        }).collect::<Vec<_>>();
        if let Some(receive) = queue_operations.iter().copied().find(|operation| {
            operation.callable == source.callable
                && operation.runtime_kind == TypedAsyncOperationKind::ChannelReceive
        }) {
            if queue_operations.len() == 1 {
                if receive.terminal {
                    return self.into_terminal_receive_execution(shape, closure_layout_fingerprint, receive, queue_bindings);
                }
                if receive.continuation.is_some() {
                    return self.into_continuation_receive_execution(shape, closure_layout_fingerprint, receive, queue_bindings);
                }
            }
        }
        if let Some(send) = queue_operations.iter().copied().find(|operation| {
            operation.callable == source.callable
                && operation.runtime_kind == TypedAsyncOperationKind::ChannelSend
        }) {
            if queue_operations.len() == 1 {
                if send.terminal {
                    return self.into_terminal_send_execution(shape, closure_layout_fingerprint, send, queue_bindings);
                }
                if send.continuation.is_some() {
                    return self.into_continuation_send_execution(shape, closure_layout_fingerprint, send, queue_bindings);
                }
            }
        }
        if let Some(select) = queue_operations.iter().copied().find(|operation| {
            operation.callable == source.callable
                && operation.runtime_kind == TypedAsyncOperationKind::ChannelSelect
                && operation.terminal
        }) {
            if queue_operations.len() == 1 {
                return self.into_terminal_select_execution(shape, closure_layout_fingerprint, select, queue_bindings);
            }
        }
        let operations = self.operations.iter().filter(|operation| {
            operation.callable == source.callable
                && !is_nested_terminal_operand(source, operation)
        }).collect::<Vec<_>>();
        let nested_count = self.operations.iter().filter(|operation| {
            operation.callable == source.callable && is_nested_terminal_operand(source, operation)
        }).count();
        if operations.len() + nested_count != source.operations.len() {
            return Err("RFC-0161 linear producer found operations for an unknown callable".into());
        }
        let mut terminal_operation = None;
        for operation in &operations {
            if let Some(previous) = terminal_operation {
                return Err(format!(
                    "RFC-0161 linear producer found source operation {} after terminal operation {}",
                    operation.source_operation,
                    previous,
                ));
            }
            match operation.runtime_kind {
                TypedAsyncOperationKind::Yield | TypedAsyncOperationKind::Return => {}
                kind => return Err(format!("RFC-0161 linear producer does not support typed operation {kind:?}")),
            }
            let unit_terminal = matches!(operation.runtime_kind, TypedAsyncOperationKind::Yield | TypedAsyncOperationKind::Return)
                && operation.operand_bindings.is_empty()
                && operation.operand_type == Some(Type::Named("Nil".into(), Vec::new()));
            if operation.runtime_kind == TypedAsyncOperationKind::Return
                && (!operation.operand_bindings.is_empty() || operation.operand_type.is_some())
                && !unit_terminal
            {
                return Err(format!(
                    "RFC-0161 linear producer does not support value-bearing return at source operation {}",
                    operation.source_operation,
                ));
            }
            if (!operation.operand_bindings.is_empty() || operation.operand_type.is_some()) && !unit_terminal {
                return Err(format!("RFC-0161 linear producer does not support operands for source operation {}", operation.source_operation));
            }
            if operation.terminal {
                terminal_operation = Some(operation.source_operation);
            }
        }

        let state_count = operations.len().checked_add(1).ok_or("RFC-0161 linear producer state count overflow")?;
        let mut states = Vec::with_capacity(state_count);
        let mut sites = Vec::new();
        for (index, operation) in operations.iter().enumerate() {
            let state = ExecutionStateId(index as u32);
            let next = ExecutionStateId(index as u32 + 1);
            let outcome = match operation.runtime_kind {
                TypedAsyncOperationKind::Yield => {
                    let site_id = OperationSiteId(sites.len() as u32);
                    sites.push(OperationSite {
                        id: site_id,
                        kind: ExecutionOperation::Yield,
                        input_lanes: Vec::new(),
                        result_lanes: Vec::new(),
                        pending_move_lanes: Vec::new(),
                        fallback_state: None,
                        queue: None,
                        arms: Vec::new(),
                    });
                    ExecutionOutcome::Ready(next)
                }
                TypedAsyncOperationKind::Return => ExecutionOutcome::Complete,
                _ => unreachable!(),
            };
            states.push(ExecutionState {
                id: state,
                transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome,
                }],
            });
        }
        if operations.is_empty() {
            states.push(ExecutionState {
                id: ExecutionStateId(0),
                transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome: ExecutionOutcome::Complete,
                }],
            });
        } else if !matches!(operations.last().map(|operation| operation.runtime_kind), Some(TypedAsyncOperationKind::Return)) {
            states.push(ExecutionState {
                id: ExecutionStateId(operations.len() as u32),
                transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome: ExecutionOutcome::Complete,
                }],
            });
        }
        let entry = self.states.iter().find(|state| {
            state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry
        });
        let frame: Vec<FrameLane> = entry.map(|state| state.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter),
            ty: input.ty.clone(),
            ownership: match input.convention {
                Convention::Own | Convention::Var => LaneOwnership::Move,
                Convention::Let | Convention::Borrow => LaneOwnership::Copy,
            },
            initially_initialized: true,
        }).collect()).unwrap_or_default();
        if frame.iter().enumerate().any(|(index, lane)| lane.id.0 != index as u32) {
            return Err("RFC-0161 linear producer requires dense entry parameter lanes".into());
        }
        let capture_to_lane = (0..frame.len()).map(|lane| FrameLaneId(lane as u32)).collect::<Vec<_>>();
        let entry_callable = entry.map(|state| state.final_callable).unwrap_or(FinalCallableIndex(0));
        let entry_lanes = entry.map(|state| {
            state.frame.iter().filter_map(|input| input.source_binding.map(|binding| (binding, FrameLaneId(input.parameter)))).collect::<BTreeMap<_, _>>()
        }).unwrap_or_default();
        let execution = TypedExecutionShape {
            id: shape,
            entry: ExecutionStateId(0),
            frame,
            result_lanes: Vec::new(),
            states,
            operations: sites,
            queues: Vec::new(),
            cancellation: None,
        };
        execution.validate()?;
        let state_bindings = (0..execution.states.len()).map(|state| {
            let target = if state == 0 {
                entry
            } else {
                operations.get(state - 1)
                    .and_then(|operation| operation.continuation)
                    .and_then(|continuation| self.states.iter().find(|candidate| candidate.final_callable == continuation))
            };
            let callable = if state == 0 {
                entry_callable
            } else {
                operations.get(state - 1)
                    .and_then(|operation| operation.continuation)
                    .and_then(|continuation| self.states.iter().find(|candidate| candidate.final_callable == continuation))
                    .map(|candidate| candidate.final_callable)
                    .unwrap_or(entry_callable)
            };
            let capture_to_lane = target.map(|target| {
                target.frame.iter().filter_map(|input| input.source_binding.and_then(|binding| entry_lanes.get(&binding).copied())).collect()
            }).unwrap_or_else(|| capture_to_lane.clone());
            FinalTypedAsyncStateBinding {
                state: ExecutionStateId(state as u32),
                callable,
                capture_to_lane,
            }
        }).collect();
        Ok(FinalTypedAsyncExecution { execution, closure_layout_fingerprint, state_bindings, dynamic_boundaries: Vec::new() })
    }

    fn into_terminal_receive_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        operation: &TypedAsyncSourceOperationBinding,
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if operation.operand_bindings.len() != 1 || operation.operand_types.len() != 1 {
            return Err("RFC-0161 terminal receive requires one typed endpoint operand".into());
        }
        let queue = queue_bindings.iter().find(|binding| binding.endpoint == operation.operand_bindings[0])
            .ok_or_else(|| "RFC-0161 terminal receive has no compiler-resolved queue binding".to_string())?;
        if queue.shape.id.0 != 0 {
            return Err("RFC-0161 terminal receive requires a dense queue shape ID".into());
        }
        let source = &self.source[0];
        let entry = self.states.iter().find(|state| {
            state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry
        }).ok_or_else(|| "RFC-0161 terminal receive has no final entry state".to_string())?;
        let frame = entry.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter),
            ty: input.ty.clone(),
            ownership: match input.convention { Convention::Own | Convention::Var => LaneOwnership::Move, Convention::Let | Convention::Borrow => LaneOwnership::Copy },
            initially_initialized: true,
        }).collect::<Vec<_>>();
        let endpoint = entry.frame.iter().find_map(|input| {
            (input.source_binding == Some(operation.operand_bindings[0])).then_some(FrameLaneId(input.parameter))
        }).ok_or_else(|| "RFC-0161 terminal receive endpoint is absent from the entry frame".to_string())?;
        let result = FrameLaneId(u32::try_from(frame.len()).map_err(|_| "RFC-0161 terminal receive result lane exceeds u32")?);
        let mut frame = frame;
        frame.push(FrameLane {
            id: result,
            ty: operation.operand_type.clone().ok_or_else(|| "RFC-0161 terminal receive has no result type".to_string())?,
            ownership: LaneOwnership::Move,
            initially_initialized: false,
        });
        let execution = TypedExecutionShape {
            id: shape,
            entry: ExecutionStateId(0),
            frame,
            result_lanes: vec![result],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) },
                }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome: ExecutionOutcome::Complete,
                }] },
                ExecutionState { id: ExecutionStateId(2), transitions: vec![ExecutionTransition {
                    condition: ExecutionEdgeCondition::Always,
                    actions: Vec::new(),
                    outcome: ExecutionOutcome::Complete,
                }] },
            ],
            operations: vec![OperationSite {
                id: OperationSiteId(0),
                kind: ExecutionOperation::ChannelReceive,
                input_lanes: vec![endpoint],
                result_lanes: vec![result],
                pending_move_lanes: Vec::new(),
                fallback_state: Some(ExecutionStateId(0)),
                queue: Some(queue.shape.id),
                arms: Vec::new(),
            }, OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: Vec::new(), result_lanes: Vec::new(), pending_move_lanes: Vec::new(), fallback_state: None, queue: None, arms: Vec::new() }],
            queues: vec![queue.shape.clone()],
            cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(2) }),
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution {
            execution,
            closure_layout_fingerprint,
            state_bindings: vec![FinalTypedAsyncStateBinding {
                state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: (0..entry.frame.len()).map(|index| FrameLaneId(index as u32)).collect(),
            }, FinalTypedAsyncStateBinding {
                state: ExecutionStateId(1), callable: entry.final_callable, capture_to_lane: vec![result],
            }, FinalTypedAsyncStateBinding {
                state: ExecutionStateId(2), callable: entry.final_callable, capture_to_lane: vec![result],
            }],
            dynamic_boundaries: Vec::new(),
        })
    }

    fn into_continuation_receive_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        operation: &TypedAsyncSourceOperationBinding,
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if operation.operand_bindings.len() != 1 || operation.operand_types.len() != 1 || operation.continuation.is_none() {
            return Err("RFC-0161 continuation receive requires one typed endpoint and a continuation".into());
        }
        let queue = queue_bindings.iter().find(|binding| binding.endpoint == operation.operand_bindings[0])
            .ok_or_else(|| "RFC-0161 continuation receive has no compiler-resolved queue binding".to_string())?;
        if queue.shape.id.0 != 0 { return Err("RFC-0161 continuation receive requires a dense queue shape ID".into()); }
        let source = &self.source[0];
        let entry = self.states.iter().find(|state| state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry)
            .ok_or_else(|| "RFC-0161 continuation receive has no final entry state".to_string())?;
        let continuation = self.states.iter().find(|state| state.final_callable == operation.continuation.unwrap() && state.kind == TypedAsyncStateKind::Segment)
            .ok_or_else(|| "RFC-0161 continuation receive has no bound continuation state".to_string())?;
        if continuation.frame.len() != 1 { return Err("RFC-0161 continuation receive requires one result capture".into()); }
        let mut frame = entry.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter), ty: input.ty.clone(),
            ownership: match input.convention { Convention::Own | Convention::Var => LaneOwnership::Move, Convention::Let | Convention::Borrow => LaneOwnership::Copy }, initially_initialized: true,
        }).collect::<Vec<_>>();
        let endpoint = entry.frame.iter().find_map(|input| (input.source_binding == Some(operation.operand_bindings[0])).then_some(FrameLaneId(input.parameter)))
            .ok_or_else(|| "RFC-0161 continuation receive endpoint is absent from the entry frame".to_string())?;
        let result = FrameLaneId(u32::try_from(frame.len()).map_err(|_| "RFC-0161 continuation receive result lane exceeds u32")?);
        frame.push(FrameLane { id: result, ty: continuation.frame[0].ty.clone(), ownership: LaneOwnership::Move, initially_initialized: false });
        let execution = TypedExecutionShape {
            id: shape, entry: ExecutionStateId(0), frame, result_lanes: vec![result],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Complete }] },
                ExecutionState { id: ExecutionStateId(2), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::ChannelReceive, input_lanes: vec![endpoint], result_lanes: vec![result], pending_move_lanes: Vec::new(), fallback_state: Some(ExecutionStateId(0)), queue: Some(queue.shape.id), arms: Vec::new() },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: Vec::new(), result_lanes: Vec::new(), pending_move_lanes: Vec::new(), fallback_state: None, queue: None, arms: Vec::new() },
            ],
            queues: vec![queue.shape.clone()], cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(2) }),
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution { execution, closure_layout_fingerprint, state_bindings: vec![
            FinalTypedAsyncStateBinding { state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: (0..entry.frame.len()).map(|index| FrameLaneId(index as u32)).collect() },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(1), callable: continuation.final_callable, capture_to_lane: vec![result] },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(2), callable: continuation.final_callable, capture_to_lane: vec![result] },
        ], dynamic_boundaries: Vec::new() })
    }

    fn into_terminal_send_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        operation: &TypedAsyncSourceOperationBinding,
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if operation.operand_bindings.len() != 2 || operation.operand_types.len() != 2 {
            return Err("RFC-0161 ChannelSend terminal send requires typed endpoint and payload operands".into());
        }
        let queue = queue_bindings.iter().find(|binding| binding.endpoint == operation.operand_bindings[0])
            .ok_or_else(|| "RFC-0161 ChannelSend terminal send has no compiler-resolved queue binding".to_string())?;
        if queue.shape.id.0 != 0 { return Err("RFC-0161 terminal send requires a dense queue shape ID".into()); }
        let source = &self.source[0];
        let entry = self.states.iter().find(|state| state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry)
            .ok_or_else(|| "RFC-0161 terminal send has no final entry state".to_string())?;
        let mut frame = entry.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter), ty: input.ty.clone(),
            ownership: match input.convention { Convention::Own | Convention::Var => LaneOwnership::Move, Convention::Let | Convention::Borrow => LaneOwnership::Copy },
            initially_initialized: true,
        }).collect::<Vec<_>>();
        let endpoint = entry.frame.iter().find_map(|input| (input.source_binding == Some(operation.operand_bindings[0])).then_some(FrameLaneId(input.parameter)))
            .ok_or_else(|| "RFC-0161 terminal send endpoint is absent from the entry frame".to_string())?;
        let payload = entry.frame.iter().find_map(|input| (input.source_binding == Some(operation.operand_bindings[1])).then_some(FrameLaneId(input.parameter)))
            .ok_or_else(|| "RFC-0161 terminal send payload must be an entry-frame binding".to_string())?;
        let execution = TypedExecutionShape {
            id: shape, entry: ExecutionStateId(0), frame: std::mem::take(&mut frame), result_lanes: Vec::new(),
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![crate::typed_execution::LaneAction::Discharge { lane: payload }], outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::ChannelSend, input_lanes: vec![endpoint, payload], result_lanes: Vec::new(), pending_move_lanes: vec![payload], fallback_state: Some(ExecutionStateId(0)), queue: Some(queue.shape.id), arms: Vec::new() }],
            queues: vec![queue.shape.clone()], cancellation: None,
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution { execution, closure_layout_fingerprint, state_bindings: vec![FinalTypedAsyncStateBinding {
            state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: (0..entry.frame.len()).map(|index| FrameLaneId(index as u32)).collect(),
        }, FinalTypedAsyncStateBinding { state: ExecutionStateId(1), callable: entry.final_callable, capture_to_lane: Vec::new() }], dynamic_boundaries: Vec::new() })
    }

    fn into_continuation_send_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        operation: &TypedAsyncSourceOperationBinding,
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if operation.operand_bindings.len() != 2 || operation.operand_types.len() != 2 || operation.continuation.is_none() {
            return Err("RFC-0161 continuation ChannelSend requires typed endpoint, payload, and continuation".into());
        }
        let queue = queue_bindings.iter().find(|binding| binding.endpoint == operation.operand_bindings[0])
            .ok_or_else(|| "RFC-0161 continuation ChannelSend has no compiler-resolved queue binding".to_string())?;
        if queue.shape.id.0 != 0 { return Err("RFC-0161 continuation ChannelSend requires a dense queue shape ID".into()); }
        let source = &self.source[0];
        let entry = self.states.iter().find(|state| state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry)
            .ok_or_else(|| "RFC-0161 continuation ChannelSend has no final entry state".to_string())?;
        let continuation = self.states.iter().find(|state| state.final_callable == operation.continuation.unwrap() && state.kind == TypedAsyncStateKind::Segment)
            .ok_or_else(|| "RFC-0161 continuation ChannelSend has no bound continuation state".to_string())?;
        if continuation.frame.len() > 1 { return Err("RFC-0161 continuation ChannelSend requires at most one completion capture".into()); }
        let mut frame = entry.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter), ty: input.ty.clone(),
            ownership: match input.convention { Convention::Own | Convention::Var => LaneOwnership::Move, Convention::Let | Convention::Borrow => LaneOwnership::Copy }, initially_initialized: true,
        }).collect::<Vec<_>>();
        let endpoint = entry.frame.iter().find_map(|input| (input.source_binding == Some(operation.operand_bindings[0])).then_some(FrameLaneId(input.parameter)))
            .ok_or_else(|| "RFC-0161 continuation ChannelSend endpoint is absent from the entry frame".to_string())?;
        let payload = entry.frame.iter().find_map(|input| (input.source_binding == Some(operation.operand_bindings[1])).then_some(FrameLaneId(input.parameter)))
            .ok_or_else(|| "RFC-0161 continuation ChannelSend payload must be an entry-frame binding".to_string())?;
        let completion = if let Some(capture) = continuation.frame.first() {
            let lane = FrameLaneId(u32::try_from(frame.len()).map_err(|_| "RFC-0161 continuation ChannelSend completion lane exceeds u32")?);
            frame.push(FrameLane { id: lane, ty: capture.ty.clone(), ownership: LaneOwnership::Move, initially_initialized: false });
            Some(lane)
        } else { None };
        let execution = TypedExecutionShape {
            id: shape, entry: ExecutionStateId(0), frame, result_lanes: Vec::new(),
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: vec![crate::typed_execution::LaneAction::Discharge { lane: payload }], outcome: ExecutionOutcome::Complete }] },
                ExecutionState { id: ExecutionStateId(2), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::ChannelSend, input_lanes: vec![endpoint, payload], result_lanes: Vec::new(), pending_move_lanes: vec![payload], fallback_state: Some(ExecutionStateId(0)), queue: Some(queue.shape.id), arms: Vec::new() },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: Vec::new(), result_lanes: Vec::new(), pending_move_lanes: Vec::new(), fallback_state: None, queue: None, arms: Vec::new() },
            ],
            queues: vec![queue.shape.clone()], cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(2) }),
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution { execution, closure_layout_fingerprint, state_bindings: vec![
            FinalTypedAsyncStateBinding { state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: (0..entry.frame.len()).map(|index| FrameLaneId(index as u32)).collect() },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(1), callable: continuation.final_callable, capture_to_lane: completion.map(|lane| vec![lane]).unwrap_or_default() },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(2), callable: continuation.final_callable, capture_to_lane: completion.map(|lane| vec![lane]).unwrap_or_default() },
        ], dynamic_boundaries: Vec::new() })
    }

    fn into_terminal_select_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
        operation: &TypedAsyncSourceOperationBinding,
        queue_bindings: &[TypedAsyncQueueBinding],
    ) -> Result<FinalTypedAsyncExecution, String> {
        if operation.operand_bindings.len() < 2 || operation.operand_types.len() != operation.operand_bindings.len() {
            return Err("RFC-0161 ChannelSelect requires at least two typed endpoint operands".into());
        }
        let source = &self.source[0];
        let entry = self.states.iter().find(|state| state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry)
            .ok_or_else(|| "RFC-0161 ChannelSelect has no final entry state".to_string())?;
        let mut frame = entry.frame.iter().map(|input| FrameLane {
            id: FrameLaneId(input.parameter), ty: input.ty.clone(),
            ownership: match input.convention { Convention::Own | Convention::Var => LaneOwnership::Move, Convention::Let | Convention::Borrow => LaneOwnership::Copy }, initially_initialized: true,
        }).collect::<Vec<_>>();
        let result = FrameLaneId(u32::try_from(frame.len()).map_err(|_| "RFC-0161 ChannelSelect result lane exceeds u32")?);
        frame.push(FrameLane { id: result, ty: operation.operand_type.clone().ok_or_else(|| "RFC-0161 ChannelSelect has no result type".to_string())?, ownership: LaneOwnership::Move, initially_initialized: false });
        let mut arms = Vec::with_capacity(operation.operand_bindings.len());
        for (ordinal, endpoint_binding) in operation.operand_bindings.iter().enumerate() {
            let endpoint_lane = entry.frame.iter().find_map(|input| (input.source_binding == Some(*endpoint_binding)).then_some(FrameLaneId(input.parameter)))
                .ok_or_else(|| format!("RFC-0161 ChannelSelect endpoint {:?} is absent from the entry frame", endpoint_binding))?;
            let queue = queue_bindings.iter().find(|binding| binding.endpoint == *endpoint_binding)
                .ok_or_else(|| format!("RFC-0161 ChannelSelect arm {ordinal} has no compiler-resolved queue binding"))?;
            if queue.shape.id.0 as usize != ordinal { return Err("RFC-0161 ChannelSelect requires queue shape IDs in arm order".into()); }
            arms.push(OperationArm { ordinal: u32::try_from(ordinal).map_err(|_| "RFC-0161 ChannelSelect arm count exceeds u32")?, operation: ExecutionOperation::ChannelReceive, endpoint_lane, input_lanes: Vec::new(), result_lanes: vec![result], queue: Some(queue.shape.id), ownership: queue.shape.ownership });
        }
        let transitions = arms.iter().map(|arm| ExecutionTransition { condition: ExecutionEdgeCondition::SelectArm { operation: OperationSiteId(0), arm: arm.ordinal }, actions: Vec::new(), outcome: ExecutionOutcome::Complete }).collect();
        // Preserve source arm order even when the resolver supplies queue
        // bindings in a different order.  Arm identity is endpoint-based;
        // positional collection would silently pair the wrong queue shape.
        let queues = operation.operand_bindings.iter().map(|endpoint| {
            queue_bindings.iter().find(|binding| binding.endpoint == *endpoint)
                .map(|binding| binding.shape.clone())
                .ok_or_else(|| "RFC-0161 ChannelSelect queue binding disappeared while forming shape".to_string())
        }).collect::<Result<Vec<_>, _>>()?;
        let execution = TypedExecutionShape {
            id: shape, entry: ExecutionStateId(0), frame, result_lanes: vec![result],
            states: vec![
                ExecutionState { id: ExecutionStateId(0), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Blocked { operation: OperationSiteId(0), retry: ExecutionStateId(1) } }] },
                ExecutionState { id: ExecutionStateId(1), transitions },
                ExecutionState { id: ExecutionStateId(2), transitions: vec![ExecutionTransition { condition: ExecutionEdgeCondition::Always, actions: Vec::new(), outcome: ExecutionOutcome::Complete }] },
            ],
            operations: vec![
                OperationSite { id: OperationSiteId(0), kind: ExecutionOperation::Select, input_lanes: Vec::new(), result_lanes: Vec::new(), pending_move_lanes: Vec::new(), fallback_state: Some(ExecutionStateId(0)), queue: None, arms },
                OperationSite { id: OperationSiteId(1), kind: ExecutionOperation::Cancellation, input_lanes: Vec::new(), result_lanes: Vec::new(), pending_move_lanes: Vec::new(), fallback_state: None, queue: None, arms: Vec::new() },
            ],
            queues, cancellation: Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(2) }),
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution { execution, closure_layout_fingerprint, state_bindings: vec![
            FinalTypedAsyncStateBinding { state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: (0..entry.frame.len()).map(|index| FrameLaneId(index as u32)).collect() },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(1), callable: entry.final_callable, capture_to_lane: vec![result] },
            FinalTypedAsyncStateBinding { state: ExecutionStateId(2), callable: entry.final_callable, capture_to_lane: vec![result] },
        ], dynamic_boundaries: Vec::new() })
    }

    fn into_boolean_control_execution(
        &self,
        shape: ExecutionShapeId,
        closure_layout_fingerprint: [u8; 32],
    ) -> Result<FinalTypedAsyncExecution, String> {
        let source = &self.source[0];
        let control = &self.controls[0];
        if control.selector_type != Some(Type::Named("Bool".into(), Vec::new())) {
            return Err("RFC-0161 control producer requires a concrete Bool selector".into());
        }
        let entry = self.states.iter().find(|state| {
            state.callable == source.callable && state.kind == TypedAsyncStateKind::Entry
        }).ok_or_else(|| "RFC-0161 control producer has no final entry state".to_string())?;
        if entry.frame.len() != 1 || entry.frame[0].ty != Type::Named("Bool".into(), Vec::new()) {
            return Err("RFC-0161 control producer requires one Bool entry parameter".into());
        }
        let branch_returns = self.operations.iter().filter(|operation| operation.callable == source.callable).collect::<Vec<_>>();
        if !branch_returns.is_empty() {
            let then_count = branch_returns.iter().filter(|operation| operation.path == vec![ControlPathStep::IfThen(crate::async_control_flow::SourceControlId(control.control))]).count();
            let else_count = branch_returns.iter().filter(|operation| operation.path == vec![ControlPathStep::IfElse(crate::async_control_flow::SourceControlId(control.control))]).count();
            if then_count != 1 || else_count != 1 || branch_returns.len() != 2 {
                return Err("RFC-0161 control producer requires one bare return on each Bool branch".into());
            }
        }
        let execution = TypedExecutionShape {
            id: shape,
            entry: ExecutionStateId(0),
            frame: vec![FrameLane { id: crate::typed_execution::FrameLaneId(0), ty: entry.frame[0].ty.clone(), ownership: LaneOwnership::Copy, initially_initialized: true }],
            result_lanes: Vec::new(),
            states: vec![ExecutionState {
                id: ExecutionStateId(0),
                transitions: vec![
                    ExecutionTransition { condition: ExecutionEdgeCondition::Boolean { lane: crate::typed_execution::FrameLaneId(0), when: true }, actions: Vec::new(), outcome: ExecutionOutcome::Complete },
                    ExecutionTransition { condition: ExecutionEdgeCondition::Boolean { lane: crate::typed_execution::FrameLaneId(0), when: false }, actions: Vec::new(), outcome: ExecutionOutcome::Complete },
                ],
            }],
            operations: Vec::new(),
            queues: Vec::new(),
            cancellation: None,
        };
        execution.validate()?;
        Ok(FinalTypedAsyncExecution {
            execution,
            closure_layout_fingerprint,
            state_bindings: vec![FinalTypedAsyncStateBinding {
                state: ExecutionStateId(0), callable: entry.final_callable, capture_to_lane: vec![crate::typed_execution::FrameLaneId(0)],
            }],
            dynamic_boundaries: Vec::new(),
        })
    }

    /// Re-run the ordinary checked lowering projection, then bind each final
    /// state to the lossless source identity retained by [`CheckedModule`].
    pub fn from_checked(checked: &CheckedModule) -> Result<Self, String> {
        let typed = typeck::annotate_checked_source(checked.module().clone())
            .map_err(|error| error.message)?;
        let mut input = Self::from_typed(&typed, checked.source_async_control_flow())?;
        for operation in &mut input.operations {
            let Some(_) = operation.expression else { continue };
            let (expression, ty) = Self::typed_source_operation(checked, operation)?;
            operation.operand_type = Some(typeck::ty_to_ast(ty).ok_or_else(|| format!(
                "source async {:?} operation {} has a non-concrete operand type `{ty}`",
                operation.callable,
                operation.source_operation,
            ))?);
            operation.operand_types = Self::typed_operation_operand_types(checked, operation)?;
            operation.runtime_kind = classify_typed_operation(operation.kind, expression);
        }
        for control in &mut input.controls {
            let (_, ty) = Self::typed_source_expression(checked, &control.callable, control.selector)?;
            control.selector_type = Some(typeck::ty_to_ast(ty).ok_or_else(|| format!(
                "source async {:?} control {} has a non-concrete selector type `{ty}`",
                control.callable,
                control.control,
            ))?);
        }
        Ok(input)
    }

    pub fn from_typed(
        typed: &TypedModule,
        source_inventory: SourceAsyncControlFlowInventory,
    ) -> Result<Self, String> {
        let mut source = source_inventory.callables;
        source.sort_by(|left, right| left.callable.cmp(&right.callable));
        let source_ids = source.iter().map(|flow| flow.callable.clone()).collect::<std::collections::BTreeSet<_>>();
        let source_suspension_counts = source.iter().map(|flow| {
            let count = flow.operations.iter().filter(|operation| {
                operation.kind == SourceAsyncOperationKind::Await
            }).count();
            Ok((
                flow.callable.clone(),
                u32::try_from(count).map_err(|_| "source suspension count exceeds u32")?,
            ))
        }).collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
        let mut source_suspension_bindings = std::collections::BTreeSet::new();
        let mut states = Vec::new();

        for (function_index, item) in typed.module().items.iter().enumerate() {
            let Item::Function(function) = item else { continue };
            let Some(local_state) = frame_state(function) else { continue };
            let kind = if function.attributes.iter().any(|attribute| attribute == FRAME_ENTRY_ATTRIBUTE) {
                TypedAsyncStateKind::Entry
            } else if function.attributes.iter().any(|attribute| attribute == FRAME_FUNCTION_ATTRIBUTE) {
                TypedAsyncStateKind::Segment
            } else {
                return Err(format!("async state at final callable index {function_index} has no state-kind marker"));
            };
            let Some((item, method)) = async_surface(function) else {
                return Err(format!("async state at final callable index {function_index} has no source identity"));
            };
            let Some(module) = async_surface_module(function) else {
                return Err(format!("async state at final callable index {function_index} has no source module identity"));
            };
            let callable = AsyncCallableId {
                module: module.to_string(),
                surface: AsyncSurfaceId {
                    item: u32::try_from(item).map_err(|_| "async source item exceeds u32")?,
                    method: method.map(|method| u32::try_from(method).map_err(|_| "async source method exceeds u32")).transpose()?,
                },
            };
            if !source_ids.contains(&callable) {
                return Err(format!("final async state at callable index {function_index} has no retained source control-flow record"));
            }
            let source_suspension = source_suspension(function);
            if let Some(source_suspension) = source_suspension {
                if kind != TypedAsyncStateKind::Segment {
                    return Err(format!("entry async state at callable index {function_index} has a source suspension identity"));
                }
                let source_suspension_count = source_suspension_counts[&callable];
                if source_suspension >= source_suspension_count {
                    return Err(format!("async state at callable index {function_index} refers to source suspension {source_suspension}, but `{}` has only {source_suspension_count}", function.name));
                }
                if !source_suspension_bindings.insert((callable.clone(), source_suspension)) {
                    return Err(format!("duplicate final async state binding for source suspension {source_suspension}"));
                }
            }
            // Segment captures are synthesized after source annotations and may
            // intentionally omit an AST annotation. Recover their exact type
            // from the checked declaration signature; the callable *identity*
            // above remains the final table index, not this lookup spelling.
            let inferred = match typed.table().function_type(&function.name) {
                Some(Type::Fn(parameters, _, _, _)) => parameters,
                Some(_) => return Err(format!("async state at final callable index {function_index} has a non-callable checked signature")),
                None => return Err(format!("async state at final callable index {function_index} has no checked signature")),
            };
            let mut declared_bindings = std::collections::BTreeMap::new();
            for (parameter, binding) in frame_bindings(function)? {
                if parameter >= function.params.len() {
                    return Err(format!(
                        "async state at final callable index {function_index} maps absent parameter {parameter} to source binding {binding}"
                    ));
                }
                if declared_bindings.insert(parameter, SourceBindingId(binding)).is_some() {
                    return Err(format!(
                        "async state at final callable index {function_index} maps parameter {parameter} more than once"
                    ));
                }
            }
            let frame = function.params.iter().enumerate().map(|(parameter, parameter_info)| {
                Ok(TypedAsyncFrameInput {
                    parameter: u32::try_from(parameter).map_err(|_| "async parameter count exceeds u32")?,
                    name: parameter_info.name.clone(),
                    source_binding: declared_bindings.get(&parameter).copied(),
                    ty: inferred.get(parameter).cloned().or_else(|| parameter_info.ty.clone()).ok_or_else(|| format!("async state at final callable index {function_index} has untyped parameter `{}`", parameter_info.name))?,
                    convention: parameter_info.convention,
                })
            }).collect::<Result<Vec<_>, String>>()?;
            states.push(TypedAsyncStateInput {
                callable,
                local_state: u32::try_from(local_state).map_err(|_| "async local state exceeds u32")?,
                kind,
                source_suspension,
                final_callable: FinalCallableIndex(u32::try_from(function_index).map_err(|_| "final callable count exceeds u32")?),
                frame,
            });
        }
        states.sort_by(|left, right| {
            (&left.callable, left.local_state, left.final_callable)
                .cmp(&(&right.callable, right.local_state, right.final_callable))
        });
        for pair in states.windows(2) {
            if pair[0].callable == pair[1].callable && pair[0].local_state == pair[1].local_state {
                return Err(format!("duplicate final async state binding for {:?} state {}", pair[0].callable, pair[0].local_state));
            }
        }
        for state in &mut states {
            if state.kind != TypedAsyncStateKind::Entry {
                continue;
            }
            let flow = source.iter().find(|flow| flow.callable == state.callable)
                .expect("source callable existence was checked while collecting states");
            let parameters = flow.bindings.iter().filter(|binding| {
                binding.kind == SourceBindingKind::Parameter
            }).collect::<Vec<_>>();
            if parameters.len() != state.frame.len() {
                return Err(format!(
                    "async entry {:?} has {} final parameters but {} source parameters",
                    state.callable,
                    state.frame.len(),
                    parameters.len(),
                ));
            }
            for (frame, source_parameter) in state.frame.iter_mut().zip(parameters) {
                match frame.source_binding {
                    Some(binding) if binding != source_parameter.id => return Err(format!(
                        "async entry {:?} parameter {} maps source binding {:?}, expected {:?}",
                        state.callable, frame.parameter, binding, source_parameter.id,
                    )),
                    Some(_) => {}
                    // Backwards-compatible fallback for manually formed old
                    // compiler fixtures. Production async lowering stamps the
                    // mapping, and continuation frames never take this path.
                    None => frame.source_binding = Some(source_parameter.id),
                }
            }
        }
        for state in &states {
            let flow = source.iter().find(|flow| flow.callable == state.callable)
                .expect("source callable existence was checked while collecting states");
            for frame in &state.frame {
                if let Some(binding) = frame.source_binding
                    && !flow.bindings.iter().any(|candidate| candidate.id == binding)
                {
                    return Err(format!(
                        "async state {:?} parameter {} maps unknown source binding {:?}",
                        state.final_callable, frame.parameter, binding,
                    ));
                }
            }
        }
        // Every source async callable starts exactly one execution. Continuation
        // states are ordinary entries in its resume table; accepting none or
        // more than one entry would force a backend to guess a start state.
        for source_callable in &source_ids {
            let entries = states.iter().filter(|state| {
                &state.callable == source_callable && state.kind == TypedAsyncStateKind::Entry
            }).count();
            if entries != 1 {
                return Err(format!(
                    "async callable {:?} must have exactly one final entry state, found {entries}",
                    source_callable,
                ));
            }
        }
        let mut suspensions = Vec::new();
        for flow in &source {
            let source_awaits = flow.operations.iter().filter(|operation| {
                operation.kind == SourceAsyncOperationKind::Await
            }).collect::<Vec<_>>();
            for state in states.iter().filter(|state| state.callable == flow.callable) {
                let Some(source_suspension) = state.source_suspension else { continue };
                let source_operation = source_awaits
                    .get(usize::try_from(source_suspension).map_err(|_| "source suspension exceeds usize")?)
                    .ok_or_else(|| format!("async state {:?} refers to missing source suspension {source_suspension}", state.final_callable))?;
                suspensions.push(TypedAsyncSuspensionBinding {
                    callable: flow.callable.clone(),
                    source_operation: source_operation.ordinal,
                    final_state: state.final_callable,
                });
            }
        }
        suspensions.sort_by(|left, right| {
            (&left.callable, left.source_operation, left.final_state)
                .cmp(&(&right.callable, right.source_operation, right.final_state))
        });
        for pair in suspensions.windows(2) {
            if pair[0].callable == pair[1].callable
                && pair[0].source_operation == pair[1].source_operation
            {
                return Err(format!(
                    "source async operation {} has multiple continuation-state bindings",
                    pair[0].source_operation,
                ));
            }
        }
        // Every await either enters its compiler-formed continuation state or
        // is a terminal edge. The latter must be represented explicitly below;
        // a later program producer must not rediscover it from CPS shape.
        let mut operations = Vec::new();
        for flow in &source {
            for operation in &flow.operations {
                let continuation = suspensions.iter().find(|binding| {
                    binding.callable == flow.callable
                        && binding.source_operation == operation.ordinal
                }).map(|binding| binding.final_state);
                if operation.kind != SourceAsyncOperationKind::Await && continuation.is_some() {
                    return Err(format!(
                        "source async non-await operation {} has a continuation-state binding",
                        operation.ordinal,
                    ));
                }
                let operand_bindings = operation.expression
                    .map(|expression| flow.bindings_read_by(expression))
                    .transpose()?
                    .unwrap_or_default();
                operations.push(TypedAsyncSourceOperationBinding {
                    callable: flow.callable.clone(),
                    source_operation: operation.ordinal,
                    kind: operation.kind,
                    runtime_kind: source_operation_kind(operation.kind),
                    path: operation.path.clone(),
                    expression: operation.expression,
                    operand_bindings,
                    operand_type: None,
                    operand_types: Vec::new(),
                    continuation,
                    terminal: operation.kind == SourceAsyncOperationKind::Await && continuation.is_none()
                        || operation.kind == SourceAsyncOperationKind::Return,
                });
            }
        }
        operations.sort_by(|left, right| {
            (&left.callable, left.source_operation)
                .cmp(&(&right.callable, right.source_operation))
        });
        for pair in operations.windows(2) {
            if pair[0].callable == pair[1].callable
                && pair[0].source_operation == pair[1].source_operation
            {
                return Err(format!(
                    "source async operation {} has multiple typed operation bindings",
                    pair[0].source_operation,
                ));
            }
        }
        let mut controls = source.iter().flat_map(|flow| flow.controls.iter().map(|control| {
            TypedAsyncSourceControlBinding {
                callable: flow.callable.clone(),
                control: control.id.0,
                selector: control.selector,
                selector_type: None,
            }
        })).collect::<Vec<_>>();
        controls.sort_by(|left, right| {
            (&left.callable, left.control).cmp(&(&right.callable, right.control))
        });
        for pair in controls.windows(2) {
            if pair[0].callable == pair[1].callable && pair[0].control == pair[1].control {
                return Err(format!("source async control {} has multiple typed bindings", pair[0].control));
            }
        }
        Ok(Self { source, states, suspensions, controls, operations, queue_bindings: Vec::new() })
    }

    /// Resolve one typed source-operation binding back to the exact retained
    /// pre-CPS expression which supplies its operands. This is deliberately a
    /// source/typed-lowering bridge only; callers still need to form typed
    /// lanes, ownership actions, and executable state edges from checked data.
    pub fn source_operation_expression<'a>(
        checked: &'a CheckedModule,
        operation: &TypedAsyncSourceOperationBinding,
    ) -> Result<&'a Expr, String> {
        let expression = operation.expression.ok_or_else(|| format!(
            "source async {:?} operation {} has no expression operand",
            operation.callable,
            operation.source_operation,
        ))?;
        let function = checked
            .async_surface()
            .function(&operation.callable.module, &operation.callable.surface)
            .ok_or_else(|| format!(
                "source async operation {} has no retained callable {:?}",
                operation.source_operation,
                operation.callable,
            ))?;
        source_expression(function, expression).ok_or_else(|| format!(
            "source async {:?} operation {} refers to absent source expression {:?}",
            operation.callable,
            operation.source_operation,
            expression,
        ))
    }

    /// Resolve an operation through the fully linked typed-source projection.
    /// The returned expression and inferred type are guaranteed to share the
    /// same allocation, unlike the lossless syntax sidecar used for diagnostics.
    pub fn typed_source_operation<'a>(
        checked: &'a CheckedModule,
        operation: &TypedAsyncSourceOperationBinding,
    ) -> Result<(&'a Expr, &'a typeck::Ty), String> {
        let expression = operation.expression.ok_or_else(|| format!(
            "source async {:?} operation {} has no expression operand",
            operation.callable,
            operation.source_operation,
        ))?;
        Self::typed_source_expression(checked, &operation.callable, expression)
    }

    fn typed_operation_operand_types(
        checked: &CheckedModule,
        operation: &TypedAsyncSourceOperationBinding,
    ) -> Result<Vec<Type>, String> {
        let (expression, _) = Self::typed_source_operation(checked, operation)?;
        let call = match expression {
            Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } => expr.as_ref(),
            other => other,
        };
        let typed = checked.typed_async_source().ok_or_else(|| format!(
            "source async {:?} has no typed pre-CPS projection", operation.callable,
        ))?;
        let argument_types = |arguments: &[&Expr]| arguments.iter().map(|argument| {
            let ty = typed.table().type_of(argument).ok_or_else(|| format!(
                "source async {:?} operation {} argument has no inferred type",
                operation.callable, operation.source_operation,
            ))?;
            typeck::ty_to_ast(ty).ok_or_else(|| format!(
                "source async {:?} operation {} has a non-concrete argument type `{ty}`",
                operation.callable, operation.source_operation,
            ))
        }).collect::<Result<Vec<_>, String>>();
        match call {
            Expr::Call { args, .. } => argument_types(&args.iter().collect::<Vec<_>>()),
            Expr::LabeledCall { args, .. } => argument_types(&args.iter().map(|(_, arg)| arg).collect::<Vec<_>>()),
            _ => Ok(Vec::new()),
        }
    }

    fn typed_source_expression<'a>(
        checked: &'a CheckedModule,
        callable: &AsyncCallableId,
        expression: SourceExpressionId,
    ) -> Result<(&'a Expr, &'a typeck::Ty), String> {
        let typed = checked.typed_async_source().ok_or_else(|| format!(
            "source async {:?} has no typed pre-CPS projection", callable,
        ))?;
        let function = typed_source_function(typed, callable).ok_or_else(|| format!(
            "source async has no typed callable {:?}", callable,
        ))?;
        let expression = source_expression(function, expression).ok_or_else(|| format!(
            "source async {:?} refers to absent typed expression {:?}", callable, expression,
        ))?;
        let ty = typed.table().type_of(expression).ok_or_else(|| format!(
            "source async {:?} expression {:?} has no inferred type", callable, expression,
        ))?;
        Ok((expression, ty))
    }
}

fn is_nested_terminal_operand(source: &SourceAsyncControlFlow, operation: &TypedAsyncSourceOperationBinding) -> bool {
    if operation.kind == SourceAsyncOperationKind::Await {
        return false;
    }
    let Some(expression) = operation.expression else { return false; };
    source.operations.iter()
        .filter(|candidate| matches!(candidate.kind, SourceAsyncOperationKind::Await | SourceAsyncOperationKind::Return))
        .filter_map(|candidate| candidate.expression)
        .any(|await_expression| source.expression_spans.iter()
            .find(|span| span.root == await_expression)
            .is_some_and(|span| expression.0 > span.root.0 && expression.0 < span.end.0))
}

fn typed_source_function<'a>(
    typed: &'a TypedModule,
    callable: &AsyncCallableId,
) -> Option<&'a Function> {
    let matches_callable = |function: &Function| {
        async_surface(function)
            .is_some_and(|(item, method)| {
                u32::try_from(item).ok() == Some(callable.surface.item)
                    && method.and_then(|method| u32::try_from(method).ok()) == callable.surface.method
            })
            && async_surface_module(function) == Some(callable.module.as_str())
    };
    typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if matches_callable(function) => Some(function),
        Item::Impl(definition) => definition.methods.iter().find(|method| matches_callable(method)),
        Item::Function(_) | Item::Type(_) | Item::Trait(_) | Item::Const { .. }
        | Item::TypeAlias { .. } | Item::Comptime(_) => None,
    })
}

fn source_operation_kind(kind: SourceAsyncOperationKind) -> TypedAsyncOperationKind {
    match kind {
        SourceAsyncOperationKind::Await => TypedAsyncOperationKind::StaticAsyncCall,
        SourceAsyncOperationKind::StaticCall => TypedAsyncOperationKind::CallOperand,
        SourceAsyncOperationKind::DynamicCall => TypedAsyncOperationKind::DynamicCallOperand,
        SourceAsyncOperationKind::Return => TypedAsyncOperationKind::Return,
    }
}

fn classify_typed_operation(
    source_kind: SourceAsyncOperationKind,
    expression: &Expr,
) -> TypedAsyncOperationKind {
    if source_kind != SourceAsyncOperationKind::Await {
        return source_operation_kind(source_kind);
    }
    let Expr::Unary { op: witchy_syntax::ast::UnOp::Await, expr } = expression else {
        return TypedAsyncOperationKind::StaticAsyncCall;
    };
    match expr.as_ref() {
        Expr::Call { name, .. } | Expr::LabeledCall { name, .. } => match name.as_str() {
            "chan.channel" | "chan.unbounded" => TypedAsyncOperationKind::ChannelCreate,
            "chan.send" => TypedAsyncOperationKind::ChannelSend,
            "chan.recv" => TypedAsyncOperationKind::ChannelReceive,
            "chan.select" => TypedAsyncOperationKind::ChannelSelect,
            "chan.spawn" | "task.spawn" => TypedAsyncOperationKind::Spawn,
            "chan.join" | "task.join" => TypedAsyncOperationKind::Join,
            "task.yield_now" | "chan.yield_now" => TypedAsyncOperationKind::Yield,
            _ => TypedAsyncOperationKind::StaticAsyncCall,
        },
        Expr::Apply { .. } | Expr::ExistentialCall { .. } => TypedAsyncOperationKind::DynamicAsyncCall,
        _ => TypedAsyncOperationKind::StaticAsyncCall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::link_checked;

    #[test]
    fn binds_inherent_async_states_to_final_indices_without_generated_names() {
        let module = witchy_syntax::parser::parse_module(
            "type Counter:\n    value: Int\n\nimpl Counter:\n    async fn next(self) -> Int:\n        let value = task.done(1).await\n        self.value + value\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        assert_eq!(input.source.len(), 1);
        assert_eq!(input.states.len(), 2);
        assert!(input.states.iter().all(|state| state.callable.module == "main"));
        assert!(input.states.iter().all(|state| state.callable.surface.method == Some(0)));
        assert!(input.states.iter().all(|state| !state.frame.is_empty()));
        assert!(input.states.iter().all(|state| state.frame.iter().all(|lane| !lane.name.is_empty())));
        assert!(input.states.iter().any(|state| {
            state.frame.iter().any(|lane| lane.name == "self")
        }));
        assert!(input.states.iter().filter(|state| state.kind == TypedAsyncStateKind::Entry).all(|state| {
            state.frame.iter().all(|lane| lane.source_binding.is_some())
        }));
        // `self` is carried over the await into a lifted continuation. Its
        // physical parameter must retain the source declaration coordinate,
        // rather than being rediscovered from the generated segment spelling.
        assert!(input.states.iter().any(|state| {
            state.kind == TypedAsyncStateKind::Segment
                && state.frame.iter().any(|lane| {
                    lane.name == "self" && lane.source_binding == Some(SourceBindingId(0))
                })
        }));
        assert_eq!(input.suspensions.len(), 1);
        assert_eq!(input.operations.len(), input.source[0].operations.len());
        assert_eq!(
            input.operations.iter().filter(|operation| operation.kind == SourceAsyncOperationKind::Await).count(),
            1,
        );
        assert!(input.operations.iter().all(|operation| {
            operation.kind != SourceAsyncOperationKind::Await
                || operation.continuation.is_some() != operation.terminal
        }));
        let await_operation = input.operations.iter().find(|operation| {
            operation.kind == SourceAsyncOperationKind::Await
        }).expect("typed await operation");
        assert_eq!(
            await_operation.operand_type,
            Some(witchy_syntax::ast::Type::Named("Int".into(), Vec::new())),
        );
        assert!(matches!(
            TypedAsyncLoweringInput::source_operation_expression(&checked, await_operation),
            Ok(witchy_syntax::ast::Expr::Unary {
                op: witchy_syntax::ast::UnOp::Await,
                ..
            })
        ));
        assert!(matches!(
            TypedAsyncLoweringInput::typed_source_operation(&checked, await_operation),
            Ok((
                witchy_syntax::ast::Expr::Unary { op: witchy_syntax::ast::UnOp::Await, .. },
                crate::typeck::Ty::Int,
            ))
        ));
        assert_eq!(
            input.states.iter().filter_map(|state| state.source_suspension).collect::<Vec<_>>(),
            vec![0],
        );
    }

    #[test]
    fn continuation_source_ids_stay_in_lexical_order_when_segments_emit_in_reverse() {
        let module = witchy_syntax::parser::parse_module(
            "async fn pipeline() -> Int:\n    let first = task.done(1).await\n    let second = task.done(first + 1).await\n    second + 1\n\nfn main(console: Console):\n    console.print(\"ready\")\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        let source_suspensions = input.states.iter()
            .filter_map(|state| state.source_suspension)
            .collect::<Vec<_>>();
        // States are sorted by physical state ID; the CPS transform forms the
        // second continuation first. The retained IDs nevertheless identify
        // their lexical source order without consulting generated names.
        assert_eq!(source_suspensions, vec![1, 0]);
        assert_eq!(input.suspensions.len(), 2);
        assert!(input.suspensions[0].source_operation < input.suspensions[1].source_operation);
        assert!(input.suspensions[0].final_state > input.suspensions[1].final_state);
        let source_awaits = input.source[0].operations.iter()
            .filter(|operation| operation.kind == SourceAsyncOperationKind::Await)
            .map(|operation| operation.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            input.suspensions.iter().map(|binding| binding.source_operation).collect::<Vec<_>>(),
            source_awaits,
        );
        let source_operations = input.source[0].operations.iter()
            .map(|operation| operation.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(
            input.operations.iter().map(|operation| operation.source_operation).collect::<Vec<_>>(),
            source_operations,
        );
        assert_eq!(
            input.operations.iter().map(|operation| operation.expression).collect::<Vec<_>>(),
            input.source[0].operations.iter().map(|operation| operation.expression).collect::<Vec<_>>(),
        );
        assert!(input.operations.iter().all(|operation| {
            operation.kind != SourceAsyncOperationKind::Await
                || operation.continuation.is_some() != operation.terminal
        }));
    }

    #[test]
    fn classifies_every_channel_send_await_as_one_language_operation() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send_one(tx: Sender(Int)):\n    chan.send(tx, 7).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        let send = input.operations.iter().find(|operation| {
            operation.kind == SourceAsyncOperationKind::Await
                && operation.runtime_kind == TypedAsyncOperationKind::ChannelSend
        }).expect("channel send await");
        assert!(send.terminal);
        assert_eq!(send.operand_bindings, vec![SourceBindingId(0)]);
        assert_eq!(send.operand_types.len(), 2);
        assert!(matches!(send.operand_types[1], Type::Named(ref name, _) if name == "Int"));
        assert!(input.operations.iter().any(|operation| {
            operation.kind == SourceAsyncOperationKind::StaticCall
                && operation.runtime_kind == TypedAsyncOperationKind::CallOperand
        }));
    }

    #[test]
    fn classifies_select_await_as_a_distinct_typed_operation() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Receiver, Selected\n\nasync fn choose(first: Receiver(Int), second: Receiver(Int)) -> Selected(Int):\n    chan.select(first, second).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(vec![("main".to_string(), module)], "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        assert!(input.operations.iter().any(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelSelect && operation.operand_bindings.len() == 2));
    }

    #[test]
    fn queue_aware_producer_lowers_terminal_select_arms_in_source_order() {
        let module = witchy_syntax::parser::parse_module("from chan import Receiver, Selected\n\nasync fn choose(first: Receiver(Int), second: Receiver(Int)) -> Selected(Int):\n    chan.select(first, second).await\n\nfn main():\n    ()\n").expect("parse source");
        let checked = link_checked(vec![("main".to_string(), module)], "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("checked source");
        let mut input = checked.typed_async_lowering_input().expect("typed lowering input");
        let select = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelSelect).expect("select operation");
        // Resolver output is not required to be positional; the producer
        // must reconstruct queues by endpoint identity.
        for (id, endpoint) in select.operand_bindings.iter().enumerate().rev() {
            input.queue_bindings.push(TypedAsyncQueueBinding { endpoint: *endpoint, shape: QueueShape { id: crate::typed_execution::QueueShapeId(id as u32), capacity: crate::typed_execution::QueueCapacity::Rendezvous, payload_layout_fingerprint: [16 + id as u8; 32], ownership: crate::typed_execution::PayloadOwnership::Inline } });
        }
        let execution = input.into_linear_execution(ExecutionShapeId(0), [18; 32]).expect("terminal select");
        assert_eq!(execution.execution.operations[0].kind, ExecutionOperation::Select);
        assert_eq!(execution.execution.operations[0].arms.iter().map(|arm| arm.ordinal).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(execution.execution.states[1].transitions.len(), 2);
    }

    #[test]
    fn retains_concrete_control_selector_types_before_cps_lowering() {
        let module = witchy_syntax::parser::parse_module(
            "async fn choose(flag: Bool) -> Int:\n    if flag:\n        task.done(1).await\n    else:\n        task.done(2).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        assert_eq!(input.controls.len(), 1);
        assert_eq!(
            input.controls[0].selector_type,
            Some(witchy_syntax::ast::Type::Named("Bool".into(), Vec::new())),
        );
    }

    #[test]
    fn operation_operands_keep_the_inner_shadowed_binding() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send(tx: Sender(Int)):\n    let value = 1\n    if true:\n        let value = 2\n        chan.send(tx, value).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = TypedAsyncLoweringInput::from_checked(&checked).expect("typed lowering input");
        let send = input.operations.iter().find(|operation| {
            operation.runtime_kind == TypedAsyncOperationKind::ChannelSend
        }).expect("channel send operation");
        assert_eq!(send.operand_bindings, vec![SourceBindingId(0), SourceBindingId(2)]);
    }

    #[test]
    fn linear_producer_emits_executable_shape_for_empty_async_return() {
        let module = witchy_syntax::parser::parse_module(
            "async fn noop():\n    ()\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let execution = input.into_linear_execution(ExecutionShapeId(0), [7; 32])
            .expect("linear async return is executable");
        assert_eq!(execution.execution.states.len(), 1);
        assert_eq!(execution.execution.states[0].transitions[0].outcome, ExecutionOutcome::Complete);
        assert!(execution.execution.operations.is_empty());
    }

    #[test]
    fn linear_producer_rejects_unbound_call_and_channel_semantics_by_operation_kind() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send_one(tx: Sender(Int)):\n    chan.send(tx, 7).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let error = input.into_linear_execution(ExecutionShapeId(0), [0; 32])
            .expect_err("channel operation needs the queue-aware producer");
        assert!(error.contains("ChannelSend"), "unexpected rejection: {error}");
    }

    #[test]
    fn queue_aware_producer_lowers_terminal_receive_with_explicit_shape() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Receiver\n\nasync fn receive_one(rx: Receiver(Int)) -> Option(Int):\n    chan.recv(rx).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let mut input = checked.typed_async_lowering_input().expect("typed lowering input");
        let receive = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelReceive).expect("receive operation");
        input.queue_bindings.push(TypedAsyncQueueBinding {
            endpoint: receive.operand_bindings[0],
            shape: QueueShape { id: crate::typed_execution::QueueShapeId(0), capacity: crate::typed_execution::QueueCapacity::Rendezvous, payload_layout_fingerprint: [3; 32], ownership: crate::typed_execution::PayloadOwnership::Inline },
        });
        let execution = input.into_linear_execution(ExecutionShapeId(0), [4; 32]).expect("queue-aware receive");
        assert_eq!(execution.execution.operations[0].kind, ExecutionOperation::ChannelReceive);
        assert_eq!(execution.execution.operations[0].input_lanes, vec![FrameLaneId(0)]);
        assert_eq!(execution.execution.operations[0].result_lanes, vec![FrameLaneId(1)]);
        assert_eq!(execution.execution.queues.len(), 1);
        assert_eq!(execution.execution.cancellation, Some(CancellationTransition { operation: OperationSiteId(1), state: ExecutionStateId(2) }));
    }

    #[test]
    fn queue_aware_producer_lowers_receive_into_continuation_capture() {
        let module = witchy_syntax::parser::parse_module("from chan import Receiver\n\nasync fn receive_then(rx: Receiver(Int)) -> Option(Int):\n    let value = chan.recv(rx).await\n    value\n\nfn main():\n    ()\n").expect("parse source");
        let checked = link_checked(vec![("main".to_string(), module)], "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("checked source");
        let mut input = checked.typed_async_lowering_input().expect("typed lowering input");
        let receive = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelReceive).expect("receive operation");
        input.queue_bindings.push(TypedAsyncQueueBinding { endpoint: receive.operand_bindings[0], shape: QueueShape { id: crate::typed_execution::QueueShapeId(0), capacity: crate::typed_execution::QueueCapacity::Rendezvous, payload_layout_fingerprint: [12; 32], ownership: crate::typed_execution::PayloadOwnership::Inline } });
        let execution = input.into_linear_execution(ExecutionShapeId(0), [13; 32]).expect("continuation receive");
        assert_eq!(execution.execution.operations[0].result_lanes, vec![FrameLaneId(1)]);
        assert_eq!(execution.execution.states.len(), 3);
        assert_eq!(execution.state_bindings[1].callable, receive.continuation.expect("continuation callable"));
    }

    #[test]
    fn queue_aware_producer_lowers_terminal_send_and_declares_pending_move() {
        let module = witchy_syntax::parser::parse_module(
            "from chan import Sender\n\nasync fn send_one(tx: Sender(Int), value: Int):\n    chan.send(tx, value).await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(vec![("main".to_string(), module)], "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("checked source");
        let mut input = checked.typed_async_lowering_input().expect("typed lowering input");
        let send = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelSend).expect("send operation");
        input.queue_bindings.push(TypedAsyncQueueBinding { endpoint: send.operand_bindings[0], shape: QueueShape { id: crate::typed_execution::QueueShapeId(0), capacity: crate::typed_execution::QueueCapacity::Rendezvous, payload_layout_fingerprint: [5; 32], ownership: crate::typed_execution::PayloadOwnership::Inline } });
        let execution = input.into_linear_execution(ExecutionShapeId(0), [6; 32]).expect("queue-aware send");
        assert_eq!(execution.execution.operations[0].kind, ExecutionOperation::ChannelSend);
        assert_eq!(execution.execution.operations[0].input_lanes, vec![FrameLaneId(0), FrameLaneId(1)]);
        assert_eq!(execution.execution.operations[0].pending_move_lanes, vec![FrameLaneId(1)]);
        assert!(matches!(execution.execution.states[1].transitions[0].actions.as_slice(), [crate::typed_execution::LaneAction::Discharge { lane: FrameLaneId(1) }]));
    }

    #[test]
    fn queue_aware_producer_lowers_send_into_empty_continuation() {
        let module = witchy_syntax::parser::parse_module("from chan import Sender\n\nasync fn send_then(tx: Sender(Int), value: Int):\n    chan.send(tx, value).await\n    return\n\nfn main():\n    ()\n").expect("parse source");
        let checked = link_checked(vec![("main".to_string(), module)], "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("checked source");
        let mut input = checked.typed_async_lowering_input().expect("typed lowering input");
        let send = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::ChannelSend).expect("send operation");
        input.queue_bindings.push(TypedAsyncQueueBinding { endpoint: send.operand_bindings[0], shape: QueueShape { id: crate::typed_execution::QueueShapeId(0), capacity: crate::typed_execution::QueueCapacity::Rendezvous, payload_layout_fingerprint: [14; 32], ownership: crate::typed_execution::PayloadOwnership::Inline } });
        let execution = input.into_linear_execution(ExecutionShapeId(0), [15; 32]).expect("continuation send");
        assert_eq!(execution.execution.operations[0].pending_move_lanes, vec![FrameLaneId(1)]);
        assert_eq!(execution.execution.states.len(), 3);
        assert_eq!(execution.state_bindings[1].callable, send.continuation.expect("continuation callable"));
    }

    #[test]
    fn linear_producer_rejects_value_bearing_return_explicitly() {
        let module = witchy_syntax::parser::parse_module(
            "async fn answer() -> Int:\n    return 7\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let error = input.into_linear_execution(ExecutionShapeId(0), [0; 32])
            .expect_err("value return needs a result-lane producer");
        assert!(error.contains("value-bearing return"), "unexpected rejection: {error}");
    }

    #[test]
    fn linear_producer_accepts_explicit_unit_return() {
        let module = witchy_syntax::parser::parse_module(
            "async fn done():\n    return\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let execution = input.into_linear_execution(ExecutionShapeId(0), [8; 32]).expect("unit return");
        assert_eq!(execution.execution.states[0].transitions[0].outcome, ExecutionOutcome::Complete);
    }

    #[test]
    fn linear_producer_lowers_unit_yield_without_erasing_its_type() {
        let module = witchy_syntax::parser::parse_module(
            "async fn tick():\n    chan.yield_now().await\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let yield_operation = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::Yield).expect("yield operation");
        assert_eq!(yield_operation.operand_type, Some(Type::Named("Nil".into(), Vec::new())));
        let execution = input.into_linear_execution(ExecutionShapeId(0), [5; 32]).expect("unit yield is executable");
        assert_eq!(execution.execution.operations.len(), 1);
        assert_eq!(execution.execution.operations[0].kind, ExecutionOperation::Yield);
    }

    #[test]
    fn linear_producer_binds_continuation_state_to_its_final_callable() {
        let module = witchy_syntax::parser::parse_module(
            "async fn tick(value: Int) -> Int:\n    chan.yield_now().await\n    chan.yield_now().await\n    value\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let yield_operation = input.operations.iter().find(|operation| operation.runtime_kind == TypedAsyncOperationKind::Yield).expect("yield operation");
        let continuation = yield_operation.continuation.expect("yield continuation");
        let execution = input.into_linear_execution(ExecutionShapeId(0), [6; 32]).expect("yield execution");
        assert_eq!(execution.state_bindings[1].callable, continuation);
        assert_eq!(execution.state_bindings[1].capture_to_lane, vec![FrameLaneId(0)]);
    }

    #[test]
    fn boolean_control_producer_emits_typed_terminal_edges() {
        let module = witchy_syntax::parser::parse_module(
            "async fn choose(flag: Bool):\n    if flag:\n        ()\n    else:\n        ()\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let execution = input.into_linear_execution(ExecutionShapeId(0), [5; 32]).expect("boolean control execution");
        assert_eq!(execution.execution.frame.len(), 1);
        assert_eq!(execution.execution.states[0].transitions.len(), 2);
        assert!(execution.execution.states[0].transitions.iter().all(|transition| transition.outcome == ExecutionOutcome::Complete));
    }

    #[test]
    fn boolean_control_producer_accepts_explicit_branch_returns() {
        let module = witchy_syntax::parser::parse_module(
            "async fn choose(flag: Bool):\n    if flag:\n        return\n    else:\n        return\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        assert_eq!(input.operations.iter().filter(|operation| operation.runtime_kind == TypedAsyncOperationKind::Return).count(), 2);
        let execution = input.into_linear_execution(ExecutionShapeId(0), [9; 32]).expect("branch returns");
        assert_eq!(execution.execution.states[0].transitions.len(), 2);
    }

    #[test]
    fn linear_program_producer_emits_all_independent_async_callables() {
        let module = witchy_syntax::parser::parse_module(
            "async fn first():\n    ()\n\nasync fn second():\n    ()\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let fingerprints = input.source.iter().map(|flow| (flow.callable.clone(), [flow.callable.surface.item as u8; 32])).collect();
        let program = input.into_linear_program(&fingerprints).expect("linear async program");
        assert_eq!(program.execution_shapes.len(), 2);
        assert_eq!(program.execution_shapes.iter().map(|shape| shape.id.0).collect::<Vec<_>>(), vec![0, 1]);
        program.validate().expect("canonical fingerprints");
    }

    #[test]
    fn linear_producer_preserves_typed_entry_parameter_lanes() {
        let module = witchy_syntax::parser::parse_module(
            "async fn keep(own value: Int):\n    ()\n\nfn main():\n    ()\n",
        ).expect("parse source");
        let checked = link_checked(
            vec![("main".to_string(), module)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let input = checked.typed_async_lowering_input().expect("typed lowering input");
        let execution = input.into_linear_execution(ExecutionShapeId(0), [4; 32]).expect("typed frame");
        assert_eq!(execution.execution.frame.len(), 1);
        assert_eq!(execution.execution.frame[0].ty, Type::Named("Int".into(), Vec::new()));
        assert_eq!(execution.execution.frame[0].ownership, LaneOwnership::Move);
        assert_eq!(execution.state_bindings[0].capture_to_lane, vec![FrameLaneId(0)]);
    }
}
