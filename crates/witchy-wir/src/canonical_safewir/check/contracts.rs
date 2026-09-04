use std::collections::{BTreeMap, BTreeSet};

use super::super::{
    AllocationClass, CallArgument, Function, FunctionId, FunctionSignature, FunctionSignatureId, MemoryEffect,
    Extent, LoadTransfer, Operation, ParameterContract, ParameterOwnership, ResultContract,
    ReturnedExtent, Terminator,
    ValueId, ValueType,
};
use super::InitialCheckError;

const MAX_CONTRACT_ITERATIONS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BodySummary {
    needs: Vec<BTreeSet<MemoryEffect>>,
    results: Vec<DerivedResult>,
    effects: BTreeMap<u32, BTreeSet<MemoryEffect>>,
    escaped_parameters: BTreeSet<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DerivedResult {
    Unseen,
    Unknown,
    Value(ValueType),
    Fresh {
        layout: crate::layout::LayoutId,
        class: AllocationClass,
        extent: ReturnedExtent,
    },
    StaticData(crate::layout::LayoutId),
    Shared(crate::layout::LayoutId),
    AliasesParameter { parameter: u32, layout: crate::layout::LayoutId },
    ReallocatesParameter { parameter: u32, layout: crate::layout::LayoutId },
}

impl DerivedResult {
    fn join(&mut self, other: Self) {
        if matches!(self, Self::Unseen) {
            *self = other;
        } else if matches!(other, Self::Unseen) || *self == other {
            return;
        } else {
            *self = Self::Unknown;
        }
    }

    fn checked_contract(&self, function: FunctionId) -> Result<ResultContract, InitialCheckError> {
        match self {
            Self::Value(value_type) => Ok(ResultContract::Value(*value_type)),
            Self::Fresh { layout, class, extent } => Ok(ResultContract::Fresh {
                layout: *layout,
                class: *class,
                extent: extent.clone(),
            }),
            Self::StaticData(layout) => Ok(ResultContract::StaticData { layout: *layout }),
            Self::Shared(layout) => Ok(ResultContract::Shared { layout: *layout }),
            Self::AliasesParameter { parameter, layout } => Ok(ResultContract::AliasesParameter {
                parameter: *parameter,
                layout: *layout,
            }),
            Self::ReallocatesParameter { parameter, layout } => {
                Ok(ResultContract::ReallocatesParameter {
                    parameter: *parameter,
                    layout: *layout,
                })
            }
            Self::Unseen | Self::Unknown => Err(InitialCheckError::UnknownCallableContract { function }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Unseen,
    Unknown,
    Parameter { parameter: u32, layout: Option<crate::layout::LayoutId> },
    Fresh {
        layout: crate::layout::LayoutId,
        class: AllocationClass,
        extent: ReturnedExtent,
    },
    StaticData(crate::layout::LayoutId),
    Shared(crate::layout::LayoutId),
    ReallocatesParameter { parameter: u32, layout: crate::layout::LayoutId },
}

impl Origin {
    fn result(self, value_type: ValueType) -> DerivedResult {
        match (self, value_type) {
            (Origin::Unseen, _) => DerivedResult::Unseen,
            (Origin::Unknown, value_type) if is_scalar(value_type) => DerivedResult::Value(value_type),
            (Origin::Fresh { layout, class, extent }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::Fresh { layout, class, extent }
            }
            (Origin::StaticData(layout), ValueType::Ref(actual)) if layout == actual => DerivedResult::StaticData(layout),
            (Origin::Shared(layout), ValueType::Ref(actual)) if layout == actual => DerivedResult::Shared(layout),
            (Origin::Parameter { parameter, layout: Some(layout) }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::AliasesParameter { parameter, layout }
            }
            (Origin::ReallocatesParameter { parameter, layout }, ValueType::Ref(actual)) if layout == actual => {
                DerivedResult::ReallocatesParameter { parameter, layout }
            }
            _ => DerivedResult::Unknown,
        }
    }
}

pub(super) fn check_all(
    functions: &BTreeMap<FunctionId, &Function>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<(), InitialCheckError> {
    let summaries = derive_all(functions, signatures)?;
    for function in functions.values() {
        let signature = signatures
            .get(&function.signature)
            .copied()
            .ok_or(InitialCheckError::MissingSignature(function.signature))?;
        check_declaration(signature, &summaries[&function.id])?;
    }
    Ok(())
}

pub(super) fn derive_result_contracts(
    functions: &BTreeMap<FunctionId, &Function>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<BTreeMap<FunctionSignatureId, Vec<ResultContract>>, InitialCheckError> {
    let summaries = derive_all(functions, signatures)?;
    let mut contracts = BTreeMap::new();
    for function in functions.values() {
        let results = summaries[&function.id]
            .results
            .iter()
            .map(|result| result.checked_contract(function.id))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(previous) = contracts.insert(function.signature, results.clone()) {
            if previous != results {
                return Err(InitialCheckError::CallableContractMismatch {
                    signature: function.signature,
                });
            }
        }
    }
    Ok(contracts)
}

fn derive_all(
    functions: &BTreeMap<FunctionId, &Function>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<BTreeMap<FunctionId, BodySummary>, InitialCheckError> {
    let mut summaries = functions
        .iter()
        .map(|(id, function)| Ok((*id, initial_summary(function, signatures)?)))
        .collect::<Result<BTreeMap<_, _>, InitialCheckError>>()?;

    let mut converged = false;
    for _ in 0..MAX_CONTRACT_ITERATIONS {
        let next = functions
            .iter()
            .map(|(id, function)| Ok((*id, derive_summary(function, functions, signatures, &summaries)?)))
            .collect::<Result<BTreeMap<_, _>, InitialCheckError>>()?;
        if next == summaries {
            converged = true;
            summaries = next;
            break;
        }
        summaries = next;
    }
    if !converged {
        return Err(InitialCheckError::UnknownCallableContract {
            function: functions.keys().next().copied().unwrap_or(FunctionId::new(0)),
        });
    }

    Ok(summaries)
}

fn initial_summary(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
) -> Result<BodySummary, InitialCheckError> {
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;
    Ok(BodySummary {
        needs: vec![BTreeSet::new(); signature.parameters.len()],
        results: signature
            .results
            .iter()
            .copied()
            .map(|value_type| {
                if is_scalar(value_type) {
                    DerivedResult::Value(value_type)
                } else {
                    DerivedResult::Unseen
                }
            })
            .collect(),
        effects: BTreeMap::new(),
        escaped_parameters: BTreeSet::new(),
    })
}

fn derive_summary(
    function: &Function,
    functions: &BTreeMap<FunctionId, &Function>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
) -> Result<BodySummary, InitialCheckError> {
    let entry = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .ok_or(InitialCheckError::MissingBlock { function: function.id, block: function.entry })?;
    let parameter_values = entry
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| (parameter.value, index as u32))
        .collect::<BTreeMap<_, _>>();
    let entry_origins = parameter_values
        .iter()
        .map(|(value, parameter)| {
            let layout = match function.value_types.get(value).copied() {
                Some(ValueType::Ref(layout)) => Some(layout),
                _ => None,
            };
            (*value, Origin::Parameter { parameter: *parameter, layout })
        })
        .collect::<BTreeMap<_, _>>();
    let mut origins = derive_origins(function, functions, summaries, &entry_origins)?;
    let mut summary = initial_summary(function, signatures)?;

    for block in &function.blocks {
        for operation in &block.operations {
            derive_operation(function, operation, functions, summaries, &mut origins, &mut summary)?;
        }
        match &block.terminator {
            Terminator::Return { values } => {
                for (index, value) in values.iter().enumerate() {
                    let value_type = function.value_types.get(value).copied().unwrap_or(ValueType::Unit);
                    let result = origins.get(value).copied().unwrap_or(Origin::Unknown).result(value_type);
                    if let Some(slot) = summary.results.get_mut(index) {
                        slot.join(result);
                    }
                    mark_escape(&mut summary, origin_of(&origins, *value));
                }
            }
            Terminator::TailCall { callee, arguments, .. } => {
                derive_tail_results(
                    function,
                    *callee,
                    arguments,
                    functions,
                    signatures,
                    summaries,
                    &origins,
                    &mut summary,
                )?;
                derive_call_needs(*callee, arguments, functions, summaries, &origins, &mut summary)?;
            }
            Terminator::VarTailCall {
                signature,
                arguments,
                ..
            } => {
                let target = indirect_target_summary(function.id, *signature, functions, summaries)?;
                let signature = signatures
                    .get(signature)
                    .copied()
                    .ok_or(InitialCheckError::MissingSignature(*signature))?;
                if target.results.len() != summary.results.len()
                    || signature.results.len() != summary.results.len()
                    || target.results.iter().any(|result| !is_indirect_result(result))
                    || signature
                        .parameters
                        .iter()
                        .copied()
                        .any(|value_type| value_type.is_memory_bearing())
                {
                    return Err(InitialCheckError::UnsupportedIndirectCall {
                        function: function.id,
                    });
                }
                for (slot, result) in summary.results.iter_mut().zip(&target.results) {
                    slot.join(result.clone());
                }
                if arguments.len() != signature.parameters.len() {
                    return Err(InitialCheckError::ArityMismatch {
                        context: "derived indirect tail-call arguments",
                        expected: signature.parameters.len(),
                        actual: arguments.len(),
                    });
                }
            }
            Terminator::Branch { .. } | Terminator::BranchIf { .. } | Terminator::Trap | Terminator::Unreachable => {}
        }
    }
    Ok(summary)
}

fn derive_origins(
    function: &Function,
    functions: &BTreeMap<FunctionId, &Function>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
    entry_origins: &BTreeMap<ValueId, Origin>,
) -> Result<BTreeMap<ValueId, Origin>, InitialCheckError> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<BTreeMap<_, _>>();
    let mut origins = function
        .blocks
        .iter()
        .flat_map(|block| block.parameters.iter().map(|parameter| (parameter.value, Origin::Unseen)))
        .collect::<BTreeMap<_, _>>();
    origins.extend(entry_origins.iter().map(|(value, origin)| (*value, *origin)));

    let iteration_limit = function.value_types.len().saturating_add(function.blocks.len()).saturating_add(1);
    for _ in 0..iteration_limit {
        let before = origins.clone();
        for block in &function.blocks {
            for operation in &block.operations {
                propagate_operation_origins(function, operation, functions, summaries, &mut origins)?;
            }
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    propagate_block_arguments(function, &blocks, *target, arguments, &mut origins)?;
                }
                Terminator::BranchIf {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    propagate_block_arguments(function, &blocks, *then_target, then_arguments, &mut origins)?;
                    propagate_block_arguments(function, &blocks, *else_target, else_arguments, &mut origins)?;
                }
                Terminator::Return { .. }
                | Terminator::TailCall { .. }
                | Terminator::VarTailCall { .. }
                | Terminator::Trap
                | Terminator::Unreachable => {}
            }
        }
        if origins == before {
            return Ok(origins);
        }
    }
    Err(InitialCheckError::UnknownCallableContract { function: function.id })
}

fn propagate_operation_origins(
    function: &Function,
    operation: &Operation,
    functions: &BTreeMap<FunctionId, &Function>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
    origins: &mut BTreeMap<ValueId, Origin>,
) -> Result<(), InitialCheckError> {
    match operation {
        Operation::Alloc { destination, layout, class, extent } => {
            join_origin(origins, *destination, Origin::Fresh {
                layout: *layout,
                class: *class,
                extent: returned_extent(extent),
            });
        }
        Operation::StaticData { destination, layout, .. } => {
            join_origin(origins, *destination, Origin::StaticData(*layout));
        }
        Operation::Share { destination, source } => {
            let origin = match origin_of(origins, *source) {
                origin @ (Origin::Parameter { layout: Some(_), .. }
                | Origin::ReallocatesParameter { .. }) => origin,
                Origin::Fresh { layout, .. } | Origin::StaticData(layout) | Origin::Shared(layout) => {
                    Origin::Shared(layout)
                }
                Origin::Unseen => Origin::Unseen,
                _ => Origin::Unknown,
            };
            join_origin(origins, *destination, origin);
        }
        Operation::MoveValue { destination, source } => {
            let origin = origin_of(origins, *source);
            join_origin(origins, *destination, origin);
        }
        Operation::FunctionValue { .. } => {}
        Operation::Load {
            destination,
            place,
            transfer,
        } => join_origin(
            origins,
            *destination,
            load_result_origin(function, *destination, place.value_type(), *transfer),
        ),
        Operation::Call { destinations, callee, arguments } => {
            let summary = summaries.get(callee).ok_or(InitialCheckError::MissingFunction(*callee))?;
            if destinations.len() != summary.results.len() {
                return Err(InitialCheckError::ArityMismatch {
                    context: "derived call results",
                    expected: summary.results.len(),
                    actual: destinations.len(),
                });
            }
            for (destination, result) in destinations.iter().zip(&summary.results) {
                let origin = result_origin(result, arguments, origins, ValueType::Unit);
                join_origin(origins, *destination, origin);
            }
        }
        Operation::VarCall { destinations, signature, arguments, .. } => {
            let target = indirect_target_summary(function.id, *signature, functions, summaries)?;
            if target.results.len() != destinations.len()
                || target.results.iter().any(|result| !is_indirect_result(result))
            {
                return Err(InitialCheckError::UnsupportedIndirectCall { function: function.id });
            }
            for (destination, result) in destinations.iter().zip(&target.results) {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                join_origin(
                    origins,
                    *destination,
                    result_origin(result, arguments, origins, value_type),
                );
            }
        }
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstSize { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. } => join_origin(origins, *destination, Origin::Unknown),
        Operation::Guard { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. }
        | Operation::HostCall { .. } => {}
    }
    Ok(())
}

fn propagate_block_arguments(
    function: &Function,
    blocks: &BTreeMap<super::super::BlockId, &super::super::BasicBlock>,
    target: super::super::BlockId,
    arguments: &[ValueId],
    origins: &mut BTreeMap<ValueId, Origin>,
) -> Result<(), InitialCheckError> {
    let block = blocks
        .get(&target)
        .copied()
        .ok_or(InitialCheckError::MissingBlock { function: function.id, block: target })?;
    if block.parameters.len() != arguments.len() {
        return Err(InitialCheckError::ArityMismatch {
            context: "derived branch arguments",
            expected: block.parameters.len(),
            actual: arguments.len(),
        });
    }
    let incoming = arguments.iter().map(|argument| origin_of(origins, *argument)).collect::<Vec<_>>();
    for (parameter, origin) in block.parameters.iter().zip(incoming) {
        join_origin(origins, parameter.value, origin);
    }
    Ok(())
}

fn join_origin(origins: &mut BTreeMap<ValueId, Origin>, destination: ValueId, incoming: Origin) {
    let current = origins.get(&destination).copied().unwrap_or(Origin::Unseen);
    let joined = match (current, incoming) {
        (Origin::Unseen, value) | (value, Origin::Unseen) => value,
        (left, right) if left == right => left,
        _ => Origin::Unknown,
    };
    origins.insert(destination, joined);
}

#[allow(clippy::too_many_arguments)]
fn derive_operation(
    function: &Function,
    operation: &Operation,
    functions: &BTreeMap<FunctionId, &Function>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
    origins: &mut BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
) -> Result<(), InitialCheckError> {
    match operation {
        Operation::Alloc { destination, layout, class, extent } => {
            origins.insert(*destination, Origin::Fresh {
                layout: *layout,
                class: *class,
                extent: returned_extent(extent),
            });
        }
        Operation::StaticData { destination, layout, .. } => { origins.insert(*destination, Origin::StaticData(*layout)); }
        Operation::Share { destination, source } => {
            let origin = match origin_of(origins, *source) {
                Origin::Parameter { layout: Some(layout), .. }
                | Origin::Fresh { layout, .. }
                | Origin::StaticData(layout)
                | Origin::Shared(layout)
                | Origin::ReallocatesParameter { layout, .. } => Origin::Shared(layout),
                _ => Origin::Unknown,
            };
            origins.insert(*destination, origin);
            add_effect(summary, origin_of(origins, *source), MemoryEffect::Retain);
        }
        Operation::MoveValue { destination, source } => { origins.insert(*destination, origin_of(origins, *source)); }
        Operation::FunctionValue { .. } => {}
        Operation::Load { destination, place, transfer } => {
            add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Read);
            origins.insert(
                *destination,
                load_result_origin(function, *destination, place.value_type(), *transfer),
            );
        }
        Operation::Store { place, value, .. } => {
            add_effect(summary, origin_of(origins, place.object()), MemoryEffect::Write);
            mark_escape(summary, origin_of(origins, *value));
        }
        Operation::UpdateCount { object, .. } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Write); }
        Operation::Reclaim { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Reclaim); }
        Operation::Retain { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Retain); }
        Operation::Release { object } => { add_effect(summary, origin_of(origins, *object), MemoryEffect::Release); }
        Operation::CopyBytes { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Read);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::MoveRange { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Write);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::CloneRange { destination, source, .. } => {
            add_effect(summary, origin_of(origins, source.object()), MemoryEffect::Read);
            add_effect(summary, origin_of(origins, destination.object()), MemoryEffect::Write);
        }
        Operation::Call { destinations, callee, arguments } => {
            let callee_summary = summaries.get(callee).ok_or(InitialCheckError::MissingFunction(*callee))?;
            if destinations.len() != callee_summary.results.len() {
                return Err(InitialCheckError::ArityMismatch { context: "derived call results", expected: callee_summary.results.len(), actual: destinations.len() });
            }
            for (destination, result) in destinations.iter().zip(&callee_summary.results) {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                origins.insert(*destination, result_origin(result, arguments, origins, value_type));
            }
            derive_call_needs(*callee, arguments, functions, summaries, origins, summary)?;
        }
        Operation::VarCall { destinations, signature, arguments, .. } => {
            let target = indirect_target_summary(function.id, *signature, functions, summaries)?;
            if target.results.len() != destinations.len()
                || target.results.iter().any(|result| !is_indirect_result(result))
            {
                return Err(InitialCheckError::UnsupportedIndirectCall { function: function.id });
            }
            for (destination, result) in destinations.iter().zip(&target.results) {
                let value_type = function.value_types.get(destination).copied().unwrap_or(ValueType::Unit);
                origins.insert(
                    *destination,
                    result_origin(result, arguments, origins, value_type),
                );
            }
        }
        Operation::ConstInt { .. } | Operation::ConstFloat { .. } | Operation::ConstBool { .. } | Operation::ConstSize { .. } | Operation::Arithmetic { .. } | Operation::Compare { .. } | Operation::NarrowSize { .. } | Operation::Guard { .. } | Operation::HostCall { .. } => {}
    }
    Ok(())
}

/// Resolve an indirect callable only through the canonical target set for its
/// checked signature. A callable signature with no lowered body is not an
/// authenticated target. Every target must derive the same result contract;
/// unknown, parameter-aliasing, and reallocating results remain fail-closed.
fn indirect_target_summary(
    function: FunctionId,
    signature: FunctionSignatureId,
    functions: &BTreeMap<FunctionId, &Function>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
) -> Result<BodySummary, InitialCheckError> {
    let mut target = None;
    for candidate in functions.values().filter(|candidate| candidate.signature == signature) {
        let summary = summaries
            .get(&candidate.id)
            .ok_or(InitialCheckError::MissingFunction(candidate.id))?;
        if let Some(joined) = &mut target {
            join_body_summary(joined, summary);
        } else {
            target = Some(summary.clone());
        }
    }
    target.ok_or(InitialCheckError::UnsupportedIndirectCall { function })
}

fn join_body_summary(left: &mut BodySummary, right: &BodySummary) {
    for (left, right) in left.results.iter_mut().zip(&right.results) {
        left.join(right.clone());
    }
    for (left, right) in left.needs.iter_mut().zip(&right.needs) {
        left.extend(right);
    }
    for (parameter, effects) in &right.effects {
        left.effects.entry(*parameter).or_default().extend(effects);
    }
    left.escaped_parameters.extend(&right.escaped_parameters);
}

fn derive_tail_results(
    function: &Function,
    callee: FunctionId,
    arguments: &[CallArgument],
    functions: &BTreeMap<FunctionId, &Function>,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
    origins: &BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
) -> Result<(), InitialCheckError> {
    let callee_function = functions.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    let callee_summary = summaries.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    let callee_signature = signatures
        .get(&callee_function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(callee_function.signature))?;
    let expected_results = summary.results.len();
    for (index, slot) in summary.results.iter_mut().enumerate() {
        let result = callee_summary.results.get(index).ok_or(InitialCheckError::ArityMismatch { context: "derived tail-call results", expected: expected_results, actual: callee_summary.results.len() })?;
        let value_type = callee_signature.results.get(index).copied().ok_or(
            InitialCheckError::ArityMismatch {
                context: "tail-call signature results",
                expected: expected_results,
                actual: callee_signature.results.len(),
            },
        )?;
        slot.join(result_origin(result, arguments, origins, value_type).result(value_type));
    }
    let _ = function;
    Ok(())
}

fn derive_call_needs(
    callee: FunctionId,
    arguments: &[CallArgument],
    functions: &BTreeMap<FunctionId, &Function>,
    summaries: &BTreeMap<FunctionId, BodySummary>,
    origins: &BTreeMap<ValueId, Origin>,
    summary: &mut BodySummary,
) -> Result<(), InitialCheckError> {
    if !functions.contains_key(&callee) { return Err(InitialCheckError::MissingFunction(callee)); }
    let callee_summary = summaries.get(&callee).ok_or(InitialCheckError::MissingFunction(callee))?;
    for (index, argument) in arguments.iter().enumerate() {
        let origin = origin_of(origins, argument_value(argument));
        let parameter = match origin {
            Origin::Parameter { parameter, .. } | Origin::ReallocatesParameter { parameter, .. } => parameter,
            _ => continue,
        };
        let need = callee_summary
            .needs
            .get(index)
            .ok_or(InitialCheckError::CallableContractMismatch { signature: functions[&callee].signature })?;
        let slot = summary
            .needs
            .get_mut(parameter as usize)
            .ok_or(InitialCheckError::CallableContractMismatch { signature: functions[&callee].signature })?;
        slot.extend(need.iter().copied());
        if matches!(argument, CallArgument::Move(_)) {
            slot.insert(MemoryEffect::Reclaim);
        }
    }
    Ok(())
}

fn origin_of(origins: &BTreeMap<ValueId, Origin>, value: ValueId) -> Origin {
    origins.get(&value).copied().unwrap_or(Origin::Unknown)
}

fn load_result_origin(
    function: &Function,
    destination: ValueId,
    place_type: ValueType,
    transfer: LoadTransfer,
) -> Origin {
    match (transfer, place_type, function.value_types.get(&destination).copied()) {
        (
            LoadTransfer::CloneShared,
            ValueType::Ref(place_layout),
            Some(ValueType::Ref(destination_layout)),
        ) if place_layout == destination_layout => Origin::Shared(place_layout),
        _ => Origin::Unknown,
    }
}

fn result_origin(result: &DerivedResult, arguments: &[CallArgument], origins: &BTreeMap<ValueId, Origin>, _value_type: ValueType) -> Origin {
    match result {
        DerivedResult::Fresh { layout, class, extent } => Origin::Fresh {
            layout: *layout,
            class: *class,
            extent: extent.clone(),
        },
        DerivedResult::StaticData(layout) => Origin::StaticData(*layout),
        DerivedResult::Shared(layout) => Origin::Shared(*layout),
        DerivedResult::AliasesParameter { parameter, layout } => arguments.get(*parameter as usize).and_then(|argument| origins.get(&argument_value(argument)).copied()).map(|origin| match origin { Origin::Parameter { parameter, .. } => Origin::Parameter { parameter, layout: Some(*layout) }, other => other }).unwrap_or(Origin::Unknown),
        DerivedResult::ReallocatesParameter { parameter, layout } => arguments.get(*parameter as usize).and_then(|argument| origins.get(&argument_value(argument)).copied()).and_then(|origin| match origin { Origin::Parameter { parameter, .. } => Some(Origin::ReallocatesParameter { parameter, layout: *layout }), _ => None }).unwrap_or(Origin::Unknown),
        DerivedResult::Unseen => Origin::Unseen,
        DerivedResult::Unknown | DerivedResult::Value(_) => Origin::Unknown,
    }
}

fn argument_value(argument: &CallArgument) -> ValueId {
    match argument { CallArgument::Value(value) | CallArgument::Move(value) | CallArgument::Share(value) => *value, CallArgument::Borrow(borrow) => borrow.source() }
}

fn add_effect(summary: &mut BodySummary, origin: Origin, effect: MemoryEffect) {
    let (Origin::Parameter { parameter, .. } | Origin::ReallocatesParameter { parameter, .. }) = origin else { return; };
    summary.effects.entry(parameter).or_default().insert(effect);
    if let Some(slot) = summary.needs.get_mut(parameter as usize) {
        slot.insert(effect);
    }
}

fn mark_escape(summary: &mut BodySummary, origin: Origin) {
    if let Origin::Parameter { parameter, .. } | Origin::ReallocatesParameter { parameter, .. } = origin {
        summary.escaped_parameters.insert(parameter);
        if let Some(need) = summary.needs.get_mut(parameter as usize) {
            need.insert(MemoryEffect::Read);
        }
    }
}

fn check_declaration(signature: &FunctionSignature, summary: &BodySummary) -> Result<(), InitialCheckError> {
    if signature.contract.parameters.len() != signature.parameters.len() || signature.contract.results.len() != signature.results.len() { return Err(InitialCheckError::CallableContractMismatch { signature: signature.id }); }
    for (parameter, value_type) in signature.contract.parameters.iter().zip(&signature.parameters) {
        if parameter.value_type != *value_type { return Err(InitialCheckError::CallableContractMismatch { signature: signature.id }); }
    }
    for (index, result) in signature.contract.results.iter().enumerate() {
        if !result_matches(result, &summary.results[index]) { return Err(InitialCheckError::CallableContractMismatch { signature: signature.id }); }
    }
    for (parameter, effects) in &signature.contract.parameter_effects {
        if *parameter as usize >= signature.parameters.len() || effects.iter().copied().collect::<BTreeSet<_>>().len() != effects.len() { return Err(InitialCheckError::CallableContractMismatch { signature: signature.id }); }
    }
    for (index, need) in summary.needs.iter().enumerate() {
        let parameter = &signature.contract.parameters[index];
        let declared = signature.contract.parameter_effects.get(&(index as u32)).cloned().unwrap_or_default().into_iter().collect::<BTreeSet<_>>();
        let derived = summary.effects.get(&(index as u32)).cloned().unwrap_or_default();
        if !derived.is_subset(&declared) || !ownership_supports(parameter, need) || (summary.escaped_parameters.contains(&(index as u32)) && parameter.non_escaping) { return Err(InitialCheckError::CallableContractMismatch { signature: signature.id }); }
    }
    Ok(())
}

fn ownership_supports(parameter: &ParameterContract, needs: &BTreeSet<MemoryEffect>) -> bool {
    needs.iter().all(|need| match (parameter.ownership, need) {
        (ParameterOwnership::Unique, _) => true,
        (ParameterOwnership::Shared, MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release) => true,
        (ParameterOwnership::CallBorrow { mode }, MemoryEffect::Read) => {
            matches!(mode, super::super::BorrowMode::Read | super::super::BorrowMode::Write)
        }
        (ParameterOwnership::CallBorrow { mode: super::super::BorrowMode::Write }, MemoryEffect::Write) => true,
        _ => false,
    })
}

fn result_matches(declared: &ResultContract, derived: &DerivedResult) -> bool {
    match (declared, derived) {
        (ResultContract::Value(expected), DerivedResult::Value(actual)) => expected == actual,
        (
            ResultContract::Fresh { layout: expected_layout, class: expected_class, extent: expected_extent },
            DerivedResult::Fresh { layout: actual_layout, class: actual_class, extent: actual_extent },
        ) => expected_layout == actual_layout && expected_class == actual_class && expected_extent == actual_extent,
        (ResultContract::StaticData { layout: expected }, DerivedResult::StaticData(actual))
        | (ResultContract::Shared { layout: expected }, DerivedResult::Shared(actual)) => expected == actual,
        (ResultContract::AliasesParameter { parameter, layout }, DerivedResult::AliasesParameter { parameter: actual_parameter, layout: actual_layout }) | (ResultContract::ReallocatesParameter { parameter, layout }, DerivedResult::ReallocatesParameter { parameter: actual_parameter, layout: actual_layout }) => parameter == actual_parameter && layout == actual_layout,
        _ => false,
    }
}

fn is_scalar(value_type: ValueType) -> bool { matches!(value_type, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size | ValueType::V128) }

fn is_indirect_result(result: &DerivedResult) -> bool {
    matches!(
        result,
        DerivedResult::Value(_)
            | DerivedResult::Fresh { .. }
            | DerivedResult::StaticData(_)
            | DerivedResult::Shared(_)
            | DerivedResult::Unseen
    )
}

fn returned_extent(extent: &Extent) -> ReturnedExtent {
    match extent {
        Extent::FixedBytes(bytes) => ReturnedExtent::FixedBytes(*bytes),
        Extent::Counted { header_bytes, stride_bytes, .. } => ReturnedExtent::Counted {
            header_bytes: *header_bytes,
            stride_bytes: *stride_bytes,
        },
        Extent::HeaderPrefix { minimum_bytes } => ReturnedExtent::FixedBytes(u64::from(*minimum_bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_safewir::{
        AccessMode, AllocationClass, BasicBlock, BlockId, BlockParameter, Extent, Ownership,
        Projection, StaticDataId,
    };

    const ENTRY: BlockId = BlockId::new(0);
    fn layout() -> crate::layout::LayoutId { crate::layout::LayoutId::from_bytes([1; 32]) }
    fn maps(function: Function, signature: FunctionSignature) -> (BTreeMap<FunctionId, &'static Function>, BTreeMap<FunctionSignatureId, &'static FunctionSignature>) { let function: &'static Function = Box::leak(Box::new(function)); let signature: &'static FunctionSignature = Box::leak(Box::new(signature)); ([(function.id, function)].into_iter().collect(), [(signature.id, signature)].into_iter().collect()) }

    #[test]
    fn scalar_indirect_call_uses_the_body_derived_target_contract() {
        let callable = ValueId::new(0);
        let argument = ValueId::new(1);
        let result = ValueId::new(2);
        let target_argument = ValueId::new(3);
        let target_result = ValueId::new(4);
        let target_signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: vec![ValueType::Int],
            results: vec![ValueType::Int],
            contract: super::super::super::CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Int,
                    ownership: ParameterOwnership::Value,
                    non_escaping: false,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let target: &'static Function = Box::leak(Box::new(Function {
            id: FunctionId::new(1),
            signature: target_signature.id,
            entry: ENTRY,
            value_types: [(target_argument, ValueType::Int), (target_result, ValueType::Int)]
                .into_iter()
                .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: vec![BlockParameter {
                    value: target_argument,
                    value_type: ValueType::Int,
                    ownership: None,
                }],
                operations: vec![Operation::ConstInt {
                    destination: target_result,
                    value: 7,
                }],
                terminator: Terminator::Return { values: vec![target_result] },
            }],
        }));
        let caller_signature = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Callable(target_signature.id), ValueType::Int],
            results: vec![ValueType::Int],
            contract: super::super::super::CallableContract {
                parameters: vec![
                    ParameterContract {
                        value_type: ValueType::Callable(target_signature.id),
                        ownership: ParameterOwnership::Value,
                        non_escaping: false,
                    },
                    ParameterContract {
                        value_type: ValueType::Int,
                        ownership: ParameterOwnership::Value,
                        non_escaping: false,
                    },
                ],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let caller: &'static Function = Box::leak(Box::new(Function {
            id: FunctionId::new(0),
            signature: caller_signature.id,
            entry: ENTRY,
            value_types: [
                (callable, ValueType::Callable(target_signature.id)),
                (argument, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: vec![
                    BlockParameter {
                        value: callable,
                        value_type: ValueType::Callable(target_signature.id),
                        ownership: None,
                    },
                    BlockParameter {
                        value: argument,
                        value_type: ValueType::Int,
                        ownership: None,
                    },
                ],
                operations: vec![Operation::VarCall {
                    destinations: vec![result],
                    callee: callable,
                    signature: target_signature.id,
                    arguments: vec![CallArgument::Value(argument)],
                }],
                terminator: Terminator::Return { values: vec![result] },
            }],
        }));
        let caller_signature: &'static FunctionSignature = Box::leak(Box::new(caller_signature));
        let target_signature: &'static FunctionSignature = Box::leak(Box::new(target_signature));
        let functions = [(caller.id, caller), (target.id, target)].into_iter().collect();
        let signatures = [(caller_signature.id, caller_signature), (target_signature.id, target_signature)]
            .into_iter()
            .collect();

        assert!(check_all(&functions, &signatures).is_ok());
    }

    #[test]
    fn indirect_call_without_a_lowered_target_fails_closed() {
        let callable = ValueId::new(0);
        let argument = ValueId::new(1);
        let result = ValueId::new(2);
        let target_signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: vec![ValueType::Int],
            results: vec![ValueType::Int],
            contract: super::super::super::CallableContract {
                parameters: vec![ParameterContract {
                    value_type: ValueType::Int,
                    ownership: ParameterOwnership::Value,
                    non_escaping: false,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let caller_signature = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Callable(target_signature.id), ValueType::Int],
            results: vec![ValueType::Int],
            contract: super::super::super::CallableContract {
                parameters: vec![
                    ParameterContract {
                        value_type: ValueType::Callable(target_signature.id),
                        ownership: ParameterOwnership::Value,
                        non_escaping: false,
                    },
                    ParameterContract {
                        value_type: ValueType::Int,
                        ownership: ParameterOwnership::Value,
                        non_escaping: false,
                    },
                ],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let caller: &'static Function = Box::leak(Box::new(Function {
            id: FunctionId::new(0),
            signature: caller_signature.id,
            entry: ENTRY,
            value_types: [
                (callable, ValueType::Callable(target_signature.id)),
                (argument, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: vec![
                    BlockParameter { value: callable, value_type: ValueType::Callable(target_signature.id), ownership: None },
                    BlockParameter { value: argument, value_type: ValueType::Int, ownership: None },
                ],
                operations: vec![Operation::VarCall {
                    destinations: vec![result],
                    callee: callable,
                    signature: target_signature.id,
                    arguments: vec![CallArgument::Value(argument)],
                }],
                terminator: Terminator::Return { values: vec![result] },
            }],
        }));
        let caller_signature: &'static FunctionSignature = Box::leak(Box::new(caller_signature));
        let target_signature: &'static FunctionSignature = Box::leak(Box::new(target_signature));
        let functions = [(caller.id, caller)].into_iter().collect();
        let signatures = [(caller_signature.id, caller_signature), (target_signature.id, target_signature)]
            .into_iter()
            .collect();

        assert!(matches!(
            check_all(&functions, &signatures),
            Err(InitialCheckError::UnsupportedIndirectCall { function }) if function == FunctionId::new(0)
        ));
    }

    #[test]
    fn static_result_cannot_claim_fresh() {
        let object = ValueId::new(0); let layout = layout();
        let contract = super::super::super::CallableContract { parameters: Vec::new(), results: vec![ResultContract::Fresh { layout, class: AllocationClass::Bump, extent: ReturnedExtent::FixedBytes(4) }], parameter_effects: BTreeMap::new() };
        let function = Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: ENTRY, value_types: [(object, ValueType::Ref(layout))].into_iter().collect(), blocks: vec![BasicBlock { id: ENTRY, parameters: Vec::new(), operations: vec![Operation::StaticData { destination: object, layout, extent: Extent::FixedBytes(4), data: StaticDataId::new(0) }], terminator: Terminator::Return { values: vec![object] } }] };
        let signature = FunctionSignature { id: FunctionSignatureId::new(0), parameters: Vec::new(), results: vec![ValueType::Ref(layout)], contract };
        let (functions, signatures) = maps(function, signature);
        assert!(matches!(check_all(&functions, &signatures), Err(InitialCheckError::CallableContractMismatch { .. })));
        assert_eq!(
            derive_result_contracts(&functions, &signatures).unwrap()[&FunctionSignatureId::new(0)],
            vec![ResultContract::StaticData { layout }]
        );
    }

    #[test]
    fn allocation_derives_fresh_result() {
        let object = ValueId::new(0); let layout = layout();
        let contract = super::super::super::CallableContract { parameters: Vec::new(), results: vec![ResultContract::Fresh { layout, class: AllocationClass::Bump, extent: ReturnedExtent::FixedBytes(4) }], parameter_effects: BTreeMap::new() };
        let function = Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: ENTRY, value_types: [(object, ValueType::Ref(layout))].into_iter().collect(), blocks: vec![BasicBlock { id: ENTRY, parameters: Vec::new(), operations: vec![Operation::Alloc { destination: object, layout, extent: Extent::FixedBytes(4), class: AllocationClass::Bump }], terminator: Terminator::Return { values: vec![object] } }] };
        let signature = FunctionSignature { id: FunctionSignatureId::new(0), parameters: Vec::new(), results: vec![ValueType::Ref(layout)], contract };
        let (functions, signatures) = maps(function, signature);
        assert!(check_all(&functions, &signatures).is_ok());
    }

    #[test]
    fn allocation_class_mismatch_cannot_claim_a_fresh_result() {
        let object = ValueId::new(0); let layout = layout();
        let contract = super::super::super::CallableContract { parameters: Vec::new(), results: vec![ResultContract::Fresh { layout, class: AllocationClass::ReferenceCounted, extent: ReturnedExtent::FixedBytes(4) }], parameter_effects: BTreeMap::new() };
        let function = Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: ENTRY, value_types: [(object, ValueType::Ref(layout))].into_iter().collect(), blocks: vec![BasicBlock { id: ENTRY, parameters: Vec::new(), operations: vec![Operation::Alloc { destination: object, layout, extent: Extent::FixedBytes(4), class: AllocationClass::Bump }], terminator: Terminator::Return { values: vec![object] } }] };
        let signature = FunctionSignature { id: FunctionSignatureId::new(0), parameters: Vec::new(), results: vec![ValueType::Ref(layout)], contract };
        let (functions, signatures) = maps(function, signature);
        assert!(matches!(check_all(&functions, &signatures), Err(InitialCheckError::CallableContractMismatch { .. })));
    }

    #[test]
    fn clone_shared_load_derives_shared_reference_result() {
        let container = ValueId::new(0);
        let index = ValueId::new(1);
        let result = ValueId::new(2);
        let container_layout = crate::layout::LayoutId::from_bytes([2; 32]);
        let element_layout = layout();
        let contract = super::super::super::CallableContract {
            parameters: vec![
                ParameterContract {
                    value_type: ValueType::Ref(container_layout),
                    ownership: ParameterOwnership::Shared,
                    non_escaping: false,
                },
                ParameterContract {
                    value_type: ValueType::Size,
                    ownership: ParameterOwnership::Value,
                    non_escaping: false,
                },
            ],
            results: vec![ResultContract::Shared { layout: element_layout }],
            parameter_effects: [(0, vec![MemoryEffect::Read])].into_iter().collect(),
        };
        let function = Function {
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: ENTRY,
            value_types: [
                (container, ValueType::Ref(container_layout)),
                (index, ValueType::Size),
                (result, ValueType::Ref(element_layout)),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: ENTRY,
                parameters: vec![
                    BlockParameter {
                        value: container,
                        value_type: ValueType::Ref(container_layout),
                        ownership: Some(Ownership::Shared),
                    },
                    BlockParameter {
                        value: index,
                        value_type: ValueType::Size,
                        ownership: None,
                    },
                ],
                operations: vec![Operation::Load {
                    destination: result,
                    place: super::super::super::Place::projected(
                        container,
                        Projection::Element { index },
                        ValueType::Ref(element_layout),
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CloneShared,
                }],
                terminator: Terminator::Return { values: vec![result] },
            }],
        };
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Ref(container_layout), ValueType::Size],
            results: vec![ValueType::Ref(element_layout)],
            contract,
        };
        let (functions, signatures) = maps(function, signature);

        assert!(check_all(&functions, &signatures).is_ok());
        assert_eq!(
            derive_result_contracts(&functions, &signatures).unwrap()
                [&FunctionSignatureId::new(0)],
            vec![ResultContract::Shared { layout: element_layout }]
        );
    }

    #[test]
    fn move_unique_load_does_not_derive_fresh_result() {
        let function = Function {
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: ENTRY,
            value_types: [(ValueId::new(0), ValueType::Ref(layout()))]
                .into_iter()
                .collect(),
            blocks: Vec::new(),
        };
        let result = load_result_origin(
            &function,
            ValueId::new(0),
            ValueType::Ref(layout()),
            LoadTransfer::MoveUnique,
        );
        assert_eq!(result, Origin::Unknown);
    }

    #[test]
    fn returning_a_parameter_derives_alias_result() {
        let object = ValueId::new(0); let layout = layout();
        let contract = super::super::super::CallableContract { parameters: vec![ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Shared, non_escaping: false }], results: vec![ResultContract::AliasesParameter { parameter: 0, layout }], parameter_effects: BTreeMap::new() };
        let function = Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: ENTRY, value_types: [(object, ValueType::Ref(layout))].into_iter().collect(), blocks: vec![BasicBlock { id: ENTRY, parameters: vec![BlockParameter { value: object, value_type: ValueType::Ref(layout), ownership: Some(Ownership::Shared) }], operations: Vec::new(), terminator: Terminator::Return { values: vec![object] } }] };
        let signature = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(layout)], results: vec![ValueType::Ref(layout)], contract };
        let (functions, signatures) = maps(function, signature);
        assert!(check_all(&functions, &signatures).is_ok());
    }

    #[test]
    fn returning_a_parameter_through_a_cfg_join_derives_alias_result() {
        let object = ValueId::new(0);
        let joined = ValueId::new(1);
        let join = BlockId::new(1);
        let layout = layout();
        let contract = super::super::super::CallableContract {
            parameters: vec![ParameterContract {
                value_type: ValueType::Ref(layout),
                ownership: ParameterOwnership::Shared,
                non_escaping: false,
            }],
            results: vec![ResultContract::AliasesParameter { parameter: 0, layout }],
            parameter_effects: BTreeMap::new(),
        };
        let function = Function {
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: ENTRY,
            value_types: [
                (object, ValueType::Ref(layout)),
                (joined, ValueType::Ref(layout)),
            ]
            .into_iter()
            .collect(),
            blocks: vec![
                BasicBlock {
                    id: ENTRY,
                    parameters: vec![BlockParameter {
                        value: object,
                        value_type: ValueType::Ref(layout),
                        ownership: Some(Ownership::Shared),
                    }],
                    operations: Vec::new(),
                    terminator: Terminator::Branch {
                        target: join,
                        arguments: vec![object],
                    },
                },
                BasicBlock {
                    id: join,
                    parameters: vec![BlockParameter {
                        value: joined,
                        value_type: ValueType::Ref(layout),
                        ownership: Some(Ownership::Shared),
                    }],
                    operations: Vec::new(),
                    terminator: Terminator::Return { values: vec![joined] },
                },
            ],
        };
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Ref(layout)],
            results: vec![ValueType::Ref(layout)],
            contract,
        };
        let (functions, signatures) = maps(function, signature);
        assert!(check_all(&functions, &signatures).is_ok());
    }

    #[test]
    fn weaker_effect_declaration_is_rejected() {
        let object = ValueId::new(0); let layout = layout();
        let contract = super::super::super::CallableContract { parameters: vec![ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Unique, non_escaping: false }], results: Vec::new(), parameter_effects: BTreeMap::new() };
        let function = Function { id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry: ENTRY, value_types: [(object, ValueType::Ref(layout))].into_iter().collect(), blocks: vec![BasicBlock { id: ENTRY, parameters: vec![BlockParameter { value: object, value_type: ValueType::Ref(layout), ownership: Some(Ownership::Unique) }], operations: vec![Operation::Reclaim { object }], terminator: Terminator::Return { values: Vec::new() } }] };
        let signature = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(layout)], results: Vec::new(), contract };
        let (functions, signatures) = maps(function, signature);
        assert!(matches!(check_all(&functions, &signatures), Err(InitialCheckError::CallableContractMismatch { .. })));
    }

    #[test]
    fn call_borrow_cannot_hide_release_behind_a_read_need() {
        let parameter = ParameterContract {
            value_type: ValueType::Ref(layout()),
            ownership: ParameterOwnership::CallBorrow {
                mode: super::super::super::BorrowMode::Read,
            },
            non_escaping: true,
        };
        let needs = [MemoryEffect::Read, MemoryEffect::Release]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert!(!ownership_supports(&parameter, &needs));
    }
}
