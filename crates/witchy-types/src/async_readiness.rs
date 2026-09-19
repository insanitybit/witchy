//! Checked product joining lexical async facts to one canonical execution.
//!
//! This is deliberately not an async planner or an optimization pass.  The
//! source-facing [`TypedAsyncLoweringInput`] retains lexical callable and
//! binding identities while [`AsyncProgram`] owns executable lanes, edges,
//! cancellation, and the canonical semantic fingerprint.  This product makes
//! a later consumer present both exact artifacts from the same checked source;
//! it must not recreate either fact from generated continuation syntax.

use std::fmt;

use witchy_syntax::ast::{Convention, Module, Type};

use crate::async_program::AsyncProgram;
use crate::loans::LoanFactTelemetry;
use crate::pipeline::CheckedModule;
use crate::async_control_flow::AsyncSourceMode;
use crate::typeck::{CheckedViewFactsError, TypedModule};
use crate::typed_async_lowering::{TypedAsyncLoweringInput, TypedAsyncOperationKind};
use crate::typed_execution::LaneOwnership;

/// Why the checked async readiness boundary refused an input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsyncReadinessError {
    MissingCanonicalSource,
    StaleCheckedSource,
    MissingTypedAsyncFacts,
    MissingLoanFacts(String),
    MissingRootedViewFacts(String),
    MissingAccessFacts(String),
    TypedInputMismatch,
    QueueBindingMismatch,
    InvalidAsyncProgram(String),
    StateDestinationMismatch,
    CaptureDestinationMissing,
    CancellationWithoutSuspension,
    ReplayableOnceCapture,
    ProgramIdentityMismatch,
}

impl fmt::Display for AsyncReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCanonicalSource => formatter.write_str(
                "async readiness requires the source-bound canonical checked product",
            ),
            Self::StaleCheckedSource => formatter.write_str(
                "async readiness belongs to a different checked source identity",
            ),
            Self::MissingTypedAsyncFacts => formatter.write_str(
                "async readiness lacks the checked typed async source facts",
            ),
            Self::MissingLoanFacts(error) => write!(formatter, "async readiness lacks checked loan facts: {error}"),
            Self::MissingRootedViewFacts(error) => write!(formatter, "async readiness lacks checked rooted-view facts: {error}"),
            Self::MissingAccessFacts(error) => write!(formatter, "async readiness lacks checked access facts: {error}"),
            Self::TypedInputMismatch => formatter.write_str(
                "async readiness typed lowering input no longer matches checked source",
            ),
            Self::QueueBindingMismatch => formatter.write_str(
                "async readiness queue bindings do not exactly match checked operands and canonical queues",
            ),
            Self::InvalidAsyncProgram(error) => write!(formatter, "async readiness rejects invalid async program: {error}"),
            Self::StateDestinationMismatch => formatter.write_str(
                "async readiness state destination does not match the checked typed frame",
            ),
            Self::CaptureDestinationMissing => formatter.write_str(
                "async readiness suspension capture has no checked destination lane",
            ),
            Self::CancellationWithoutSuspension => formatter.write_str(
                "async readiness cancellation has no checked source suspension",
            ),
            Self::ReplayableOnceCapture => formatter.write_str(
                "async readiness rejects an affine once capture in a replayable task frame",
            ),
            Self::ProgramIdentityMismatch => formatter.write_str(
                "async readiness program bytes no longer match the checked execution identity",
            ),
        }
    }
}

impl std::error::Error for AsyncReadinessError {}

/// One sealed, source-bound async execution product.
///
/// All fields are private: construction is available only through
/// [`CheckedModule::checked_async_readiness`], and every query rechecks the
/// original checked allocation, typed lowering input, and canonical program
/// bytes.  Keeping the bytes in addition to the program fingerprint makes a
/// stale or redirected scheduling edge reject without trusting an optimizer
/// cache or a generated callable name.
#[derive(Debug)]
pub struct CheckedAsyncReadiness {
    checked_module: *const Module,
    canonical_typed_module: *const Module,
    typed_async_module: Option<*const Module>,
    source_bundle_digest: [u8; 32],
    typed_input: TypedAsyncLoweringInput,
    program_bytes: Vec<u8>,
    program_fingerprint: [u8; 32],
    loan_telemetry: LoanFactTelemetry,
    access_contract_count: usize,
    loan_owner_root_count: usize,
    rooted_view_contract: bool,
    rooted_view_fact_count: usize,
    must_capture_lanes: usize,
    cancellation_count: usize,
    cross_call_count: usize,
    normal_callable_count: usize,
    opt_callable_count: usize,
}

/// The checked immutable-value evidence attached to one typed allocation.
///
/// RFC-0167 rooted views supersede lexical loan graphs.  A module carries
/// exactly one of these products: accepting a missing view product as an empty
/// loan graph would let an async consumer recover ownership from generated
/// frames instead of the checked source allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetainedValueContract {
    LegacyLoans {
        telemetry: LoanFactTelemetry,
        owner_root_count: usize,
    },
    RootedViews {
        fact_count: usize,
    },
}

fn retained_value_contract(typed: &TypedModule) -> Result<RetainedValueContract, AsyncReadinessError> {
    match typed.view_facts() {
        Ok(facts) => Ok(RetainedValueContract::RootedViews { fact_count: facts.iter().count() }),
        Err(CheckedViewFactsError::Stale) => Err(AsyncReadinessError::MissingRootedViewFacts(
            CheckedViewFactsError::Stale.to_string(),
        )),
        Err(CheckedViewFactsError::Missing) => {
            let facts = typed
                .loan_facts()
                .map_err(|error| AsyncReadinessError::MissingLoanFacts(error.to_string()))?;
            Ok(RetainedValueContract::LegacyLoans {
                telemetry: facts.telemetry(),
                owner_root_count: facts.owner_roots().len(),
            })
        }
    }
}

impl PartialEq for CheckedAsyncReadiness {
    fn eq(&self, other: &Self) -> bool {
        self.source_bundle_digest == other.source_bundle_digest
            && self.typed_input == other.typed_input
            && self.program_bytes == other.program_bytes
    }
}

impl CheckedAsyncReadiness {
    pub(crate) fn new(
        checked: &CheckedModule,
        input: &TypedAsyncLoweringInput,
        program: &AsyncProgram,
    ) -> Result<Self, AsyncReadinessError> {
        let source = checked.canonical_source().ok_or(AsyncReadinessError::MissingCanonicalSource)?;
        source
            .optimization_readiness()
            .validate_source(source)
            .map_err(|_| AsyncReadinessError::StaleCheckedSource)?;
        let retained_values = retained_value_contract(source.typed_module())?;
        let (loan_telemetry, loan_owner_root_count, rooted_view_contract, rooted_view_fact_count) = match retained_values {
            RetainedValueContract::LegacyLoans { telemetry, owner_root_count } => {
                (telemetry, owner_root_count, false, 0)
            }
            RetainedValueContract::RootedViews { fact_count } => {
                (LoanFactTelemetry::default(), 0, true, fact_count)
            }
        };
        let access_contract_count = source
            .access_facts()
            .map_err(|error| AsyncReadinessError::MissingAccessFacts(error.to_string()))?
            .call_contracts()
            .count();
        let typed_async_module = match checked.typed_async_source() {
            Some(typed) => {
                retained_value_contract(typed)?;
                typed
                    .access_facts()
                    .map_err(|error| AsyncReadinessError::MissingAccessFacts(error.to_string()))?;
                Some(std::ptr::from_ref(typed.module()))
            }
            None if input.source.is_empty() && program.execution_shapes.is_empty() => None,
            None => return Err(AsyncReadinessError::MissingTypedAsyncFacts),
        };
        let expected = TypedAsyncLoweringInput::from_checked(checked)
            .map_err(AsyncReadinessError::InvalidAsyncProgram)?;
        if !checked_input_matches(&expected, input) {
            return Err(AsyncReadinessError::TypedInputMismatch);
        }
        validate_program_binding(input, program)?;
        let program_bytes = program
            .canonical_bytes()
            .map_err(AsyncReadinessError::InvalidAsyncProgram)?;
        Ok(Self {
            checked_module: std::ptr::from_ref(checked.module()),
            canonical_typed_module: std::ptr::from_ref(source.typed_module().module()),
            typed_async_module,
            source_bundle_digest: source.source_witness().source_bundle_digest(),
            typed_input: input.clone(),
            program_fingerprint: program.semantic_fingerprint,
            must_capture_lanes: program.execution_shapes.iter().flat_map(|shape| &shape.frame)
                .filter(|lane| lane.ownership == LaneOwnership::MustConsume).count(),
            cancellation_count: program.execution_shapes.iter().filter(|shape| shape.cancellation.is_some()).count(),
            cross_call_count: input.operations.iter().filter(|operation| matches!(
                operation.runtime_kind,
                TypedAsyncOperationKind::StaticAsyncCall | TypedAsyncOperationKind::DynamicAsyncCall,
            )).count(),
            normal_callable_count: input.source.iter().filter(|source| source.mode == AsyncSourceMode::Normal).count(),
            opt_callable_count: input.source.iter().filter(|source| source.mode == AsyncSourceMode::Opt).count(),
            loan_telemetry,
            access_contract_count,
            loan_owner_root_count,
            rooted_view_contract,
            rooted_view_fact_count,
            program_bytes,
        })
    }

    /// Re-authenticate all three components before a production consumer uses
    /// a scheduling, capture, or cancellation fact.
    pub fn validate(
        &self,
        checked: &CheckedModule,
        input: &TypedAsyncLoweringInput,
        program: &AsyncProgram,
    ) -> Result<(), AsyncReadinessError> {
        let source = checked.canonical_source().ok_or(AsyncReadinessError::MissingCanonicalSource)?;
        if !std::ptr::eq(self.checked_module, checked.module())
            || !std::ptr::eq(self.canonical_typed_module, source.typed_module().module())
            || self.source_bundle_digest != source.source_witness().source_bundle_digest()
            || self.typed_async_module != checked.typed_async_source().map(|typed| std::ptr::from_ref(typed.module()))
        {
            return Err(AsyncReadinessError::StaleCheckedSource);
        }
        retained_value_contract(source.typed_module())?;
        source
            .access_facts()
            .map_err(|error| AsyncReadinessError::MissingAccessFacts(error.to_string()))?;
        if let Some(typed) = checked.typed_async_source() {
            retained_value_contract(typed)?;
            typed.access_facts().map_err(|error| AsyncReadinessError::MissingAccessFacts(error.to_string()))?;
        }
        let expected = TypedAsyncLoweringInput::from_checked(checked)
            .map_err(AsyncReadinessError::InvalidAsyncProgram)?;
        if input != &self.typed_input || !checked_input_matches(&expected, &self.typed_input) {
            return Err(AsyncReadinessError::TypedInputMismatch);
        }
        validate_program_binding(input, program)?;
        let bytes = program.canonical_bytes().map_err(AsyncReadinessError::InvalidAsyncProgram)?;
        if bytes != self.program_bytes || program.semantic_fingerprint != self.program_fingerprint {
            return Err(AsyncReadinessError::ProgramIdentityMismatch);
        }
        Ok(())
    }

    pub fn loan_telemetry(&self) -> LoanFactTelemetry { self.loan_telemetry }
    pub fn access_contract_count(&self) -> usize { self.access_contract_count }
    pub fn loan_owner_root_count(&self) -> usize { self.loan_owner_root_count }
    /// Number of exact rooted-view formations retained by the canonical
    /// immutable-value product. A view-bearing declaration-only module can
    /// legitimately retain zero formations; use `uses_rooted_view_contract`
    /// to distinguish that from the legacy loan product.
    pub fn rooted_view_fact_count(&self) -> usize { self.rooted_view_fact_count }
    pub fn uses_rooted_view_contract(&self) -> bool { self.rooted_view_contract }
    pub fn must_capture_lanes(&self) -> usize { self.must_capture_lanes }
    pub fn cancellation_count(&self) -> usize { self.cancellation_count }
    pub fn cross_call_count(&self) -> usize { self.cross_call_count }
    pub fn normal_callable_count(&self) -> usize { self.normal_callable_count }
    pub fn opt_callable_count(&self) -> usize { self.opt_callable_count }
    pub fn program_fingerprint(&self) -> [u8; 32] { self.program_fingerprint }
}

fn validate_program_binding(
    input: &TypedAsyncLoweringInput,
    program: &AsyncProgram,
) -> Result<(), AsyncReadinessError> {
    program.validate().map_err(AsyncReadinessError::InvalidAsyncProgram)?;
    if input.source.len() != program.execution_shapes.len() {
        return Err(AsyncReadinessError::StateDestinationMismatch);
    }
    for (shape_index, source) in input.source.iter().enumerate() {
        let shape = &program.execution_shapes[shape_index];
        let states = input.states.iter().filter(|state| state.callable == source.callable).collect::<Vec<_>>();
        for state in states {
            // Scheduler states may share one final callable (for example the
            // ready and cancellation paths of a terminal receive).  Those
            // extra executable edges have no fabricated source state.  What
            // matters here is that every typed frame has at least one exact
            // physical binding, not that scheduler-state cardinality mirrors
            // source continuation cardinality.
            let binding = shape.states.iter().find(|binding| {
                binding.callable == state.final_callable
                    && binding.capture_to_lane.len() == state.frame.len()
                    && state.frame.iter().zip(&binding.capture_to_lane).all(|(field, lane)| {
                        shape.frame.get(*lane as usize).is_some_and(|lane| {
                            lane.ty == field.ty && lane.ownership == field_lane_ownership(field.convention)
                        })
                    })
            })
                .ok_or(AsyncReadinessError::StateDestinationMismatch)?;
            // A carried source binding already resident in the entry frame
            // must retain that same physical lane.  This makes a fresh
            // fingerprint insufficient to bless a capture-lane swap when two
            // fields happen to have equal types and ownership.
            let entry = input.states.iter().find(|candidate| {
                candidate.callable == source.callable
                    && candidate.kind == crate::typed_async_lowering::TypedAsyncStateKind::Entry
            }).ok_or(AsyncReadinessError::StateDestinationMismatch)?;
            let entry_binding = shape.states.iter().find(|binding| {
                binding.callable == entry.final_callable
                    && binding.capture_to_lane.len() == entry.frame.len()
                    && entry.frame.iter().zip(&binding.capture_to_lane).all(|(field, lane)| {
                        shape.frame.get(*lane as usize).is_some_and(|lane| {
                            lane.ty == field.ty && lane.ownership == field_lane_ownership(field.convention)
                        })
                    })
            })
                .ok_or(AsyncReadinessError::StateDestinationMismatch)?;
            for (position, field) in state.frame.iter().enumerate() {
                let Some(source_binding) = field.source_binding else { continue };
                let Some(entry_position) = entry.frame.iter().position(|entry_field| {
                    entry_field.source_binding == Some(source_binding)
                }) else { continue };
                if binding.capture_to_lane[position] != entry_binding.capture_to_lane[entry_position] {
                    return Err(AsyncReadinessError::CaptureDestinationMissing);
                }
            }
        }
        for suspension in input.suspensions.iter().filter(|suspension| suspension.callable == source.callable) {
            let operation = input.operations.iter().find(|operation| {
                operation.callable == source.callable && operation.source_operation == suspension.source_operation
            }).ok_or(AsyncReadinessError::StateDestinationMismatch)?;
            let state = input.states.iter().find(|state| state.final_callable == suspension.final_state)
                .ok_or(AsyncReadinessError::StateDestinationMismatch)?;
            if operation.operand_bindings.iter().any(|binding| {
                !state.frame.iter().any(|field| field.source_binding == Some(*binding))
            }) {
                return Err(AsyncReadinessError::CaptureDestinationMissing);
            }
        }
        if shape.cancellation.is_some()
            && !input.operations.iter().any(|operation| {
                operation.callable == source.callable && operation.kind == crate::async_control_flow::SourceAsyncOperationKind::Await
            })
        {
            return Err(AsyncReadinessError::CancellationWithoutSuspension);
        }
    }
    validate_queue_bindings(input, program)?;
    if input.states.iter().flat_map(|state| &state.frame).any(|field| type_carries_once(&field.ty)) {
        return Err(AsyncReadinessError::ReplayableOnceCapture);
    }
    Ok(())
}

fn checked_input_matches(
    expected: &TypedAsyncLoweringInput,
    supplied: &TypedAsyncLoweringInput,
) -> bool {
    expected.source == supplied.source
        && expected.states == supplied.states
        && expected.suspensions == supplied.suspensions
        && expected.controls == supplied.controls
        && expected.operations == supplied.operations
        && queue_bindings_are_structural(supplied).is_ok()
}

fn validate_queue_bindings(
    input: &TypedAsyncLoweringInput,
    program: &AsyncProgram,
) -> Result<(), AsyncReadinessError> {
    queue_bindings_are_structural(input)?;
    let queues = program.execution_shapes.iter().flat_map(|shape| &shape.queues).collect::<Vec<_>>();
    if input.queue_bindings.len() != queues.len()
        || input.queue_bindings.iter().any(|binding| !queues.iter().any(|queue| **queue == binding.shape))
    {
        return Err(AsyncReadinessError::QueueBindingMismatch);
    }
    Ok(())
}

fn queue_bindings_are_structural(
    input: &TypedAsyncLoweringInput,
) -> Result<(), AsyncReadinessError> {
    let mut endpoints = std::collections::BTreeSet::new();
    for binding in &input.queue_bindings {
        let checked_queue_operand = input.operations.iter().any(|operation| {
            matches!(
                operation.runtime_kind,
                TypedAsyncOperationKind::ChannelSend
                    | TypedAsyncOperationKind::ChannelReceive
                    | TypedAsyncOperationKind::ChannelSelect
            ) && operation.operand_bindings.contains(&binding.endpoint)
        });
        if !endpoints.insert(binding.endpoint) || !checked_queue_operand {
            return Err(AsyncReadinessError::QueueBindingMismatch);
        }
    }
    Ok(())
}

fn field_lane_ownership(convention: Convention) -> LaneOwnership {
    match convention {
        Convention::Own | Convention::Var => LaneOwnership::Move,
        Convention::Let | Convention::Borrow => LaneOwnership::Copy,
    }
}

fn type_carries_once(ty: &Type) -> bool {
    match ty {
        Type::Fn(inputs, output, _, qualifiers) => qualifiers.once
            || inputs.iter().any(type_carries_once)
            || type_carries_once(output),
        Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
            arguments.iter().any(type_carries_once)
        }
        Type::Slice(element) | Type::Qualified(_, element) => type_carries_once(element),
        Type::RecordCompose { base, fields } => {
            type_carries_once(base) || fields.iter().any(|(_, field)| type_carries_once(field))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::async_surface::AsyncSurfaceId;
    use witchy_syntax::ast::{CallableQualifiers, Convention};

    use crate::async_control_flow::{
        AsyncCallableId, SourceAsyncBinding, SourceAsyncControlFlow, SourceBindingId,
        SourceBindingKind,
    };
    use crate::async_program::{AsyncExecutionShape, AsyncStateEdge, FinalStateBinding};
    use crate::typed_async_lowering::{
        FinalCallableIndex, TypedAsyncFrameInput, TypedAsyncQueueBinding, TypedAsyncStateInput,
        TypedAsyncStateKind,
    };
    use crate::typed_execution::{
        ExecutionEdgeCondition, ExecutionOutcome, FrameLane, FrameLaneId, PayloadOwnership,
        QueueCapacity, QueueShape, QueueShapeId,
    };

    fn input(frame: Vec<TypedAsyncFrameInput>) -> TypedAsyncLoweringInput {
        let callable = AsyncCallableId {
            module: "main".into(),
            surface: AsyncSurfaceId { item: 0, method: None },
        };
        TypedAsyncLoweringInput {
            source: vec![SourceAsyncControlFlow {
                callable: callable.clone(),
                mode: AsyncSourceMode::Normal,
                controls: vec![],
                bindings: vec![],
                binding_uses: vec![],
                expression_spans: vec![],
                operations: vec![],
            }],
            states: vec![TypedAsyncStateInput {
                callable,
                local_state: 0,
                kind: TypedAsyncStateKind::Entry,
                source_suspension: None,
                final_callable: FinalCallableIndex(0),
                frame,
            }],
            suspensions: vec![],
            controls: vec![],
            operations: vec![],
            queue_bindings: vec![],
        }
    }

    fn program(capture_to_lane: Vec<u32>, frame: Vec<FrameLane>) -> AsyncProgram {
        AsyncProgram::new(vec![AsyncExecutionShape {
            id: crate::typed_execution::ExecutionShapeId(0),
            closure_layout_fingerprint: [0x16; 32],
            frame,
            entry: crate::async_program::AsyncProgramStateId(0),
            result_lanes: vec![],
            states: vec![FinalStateBinding {
                state: crate::async_program::AsyncProgramStateId(0),
                callable: FinalCallableIndex(0),
                capture_to_lane,
                edges: vec![AsyncStateEdge {
                    condition: ExecutionEdgeCondition::Always,
                    actions: vec![],
                    outcome: ExecutionOutcome::Complete,
                }],
            }],
            operations: vec![],
            queues: vec![],
            cancellation: None,
            dynamic_boundaries: vec![],
        }]).expect("well-formed minimal canonical program")
    }

    #[test]
    fn async_readiness_rejects_state_and_program_identity_drift() {
        let input = input(vec![]);
        let program = program(vec![], vec![]);
        validate_program_binding(&input, &program).expect("exact state destination is retained");

        let mut stale = program.clone();
        stale.execution_shapes[0].entry = crate::async_program::AsyncProgramStateId(1);
        assert!(matches!(
            validate_program_binding(&input, &stale),
            Err(AsyncReadinessError::InvalidAsyncProgram(_)),
        ));

        let mut redirected = input.clone();
        redirected.states[0].final_callable = FinalCallableIndex(1);
        assert_eq!(
            validate_program_binding(&redirected, &program).unwrap_err(),
            AsyncReadinessError::StateDestinationMismatch,
        );
    }

    #[test]
    fn async_readiness_rejects_replayable_once_frame_capture() {
        let once = Type::Fn(
            vec![],
            Box::new(Type::Named("Int".into(), vec![])),
            vec![],
            CallableQualifiers { pure: false, once: true },
        );
        let input = input(vec![TypedAsyncFrameInput {
            parameter: 0,
            name: "callback".into(),
            source_binding: None,
            ty: once.clone(),
            convention: Convention::Own,
        }]);
        let program = program(vec![0], vec![FrameLane {
            id: FrameLaneId(0),
            ty: once,
            ownership: LaneOwnership::Move,
            initially_initialized: true,
        }]);
        assert_eq!(
            validate_program_binding(&input, &program).unwrap_err(),
            AsyncReadinessError::ReplayableOnceCapture,
        );
    }

    #[test]
    fn async_readiness_rejects_refingerprinted_capture_lane_swap() {
        let int = Type::Named("Int".into(), vec![]);
        let string = Type::Named("String".into(), vec![]);
        let input = input(vec![
            TypedAsyncFrameInput {
                parameter: 0,
                name: "number".into(),
                source_binding: None,
                ty: int.clone(),
                convention: Convention::Let,
            },
            TypedAsyncFrameInput {
                parameter: 1,
                name: "text".into(),
                source_binding: None,
                ty: string.clone(),
                convention: Convention::Let,
            },
        ]);
        let lanes = vec![
            FrameLane { id: FrameLaneId(0), ty: int, ownership: LaneOwnership::Copy, initially_initialized: true },
            FrameLane { id: FrameLaneId(1), ty: string, ownership: LaneOwnership::Copy, initially_initialized: true },
        ];
        let correct = program(vec![0, 1], lanes.clone());
        validate_program_binding(&input, &correct).expect("typed fields bind their matching lanes");

        // `new` recomputes a valid semantic fingerprint for this program, so
        // the rejection proves field-to-lane type checks rather than merely
        // detecting mutation after fingerprinting.
        let swapped = program(vec![1, 0], lanes);
        assert_ne!(correct.semantic_fingerprint, swapped.semantic_fingerprint);
        assert_eq!(
            validate_program_binding(&input, &swapped).unwrap_err(),
            AsyncReadinessError::StateDestinationMismatch,
        );
    }

    #[test]
    fn async_readiness_rejects_queue_binding_for_nonoperand_lexical_binding() {
        let mut input = input(vec![]);
        let unrelated = SourceBindingId(7);
        input.source[0].bindings.push(SourceAsyncBinding {
            id: unrelated,
            name: "not_an_endpoint".into(),
            kind: SourceBindingKind::Let,
        });
        input.queue_bindings.push(TypedAsyncQueueBinding {
            endpoint: unrelated,
            shape: QueueShape {
                id: QueueShapeId(0),
                capacity: QueueCapacity::Rendezvous,
                payload_layout_fingerprint: [0x67; 32],
                ownership: PayloadOwnership::Inline,
            },
        });
        assert_eq!(
            queue_bindings_are_structural(&input).unwrap_err(),
            AsyncReadinessError::QueueBindingMismatch,
        );
    }
}
