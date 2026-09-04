use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use super::{
    BasicBlock, BlockId, CallArgument, CanonicalModule, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, InitialValid, LoweredCandidate, ModuleStage, Operation, OptimizedValid,
    ParameterOwnership, ProperTailSite, TailSiteId, Terminator, TransformedCandidate, ValueId,
    ValueType,
};

mod memory;
use memory::MemoryChecker;
mod contracts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialCheckError {
    InvalidLayoutBundle(String),
    UnknownLayout(crate::layout::LayoutId),
    InvalidExtent { function: FunctionId, object: ValueId },
    InvalidProjection { function: FunctionId, object: ValueId },
    MissingBoundsGuard {
        function: FunctionId,
        index: ValueId,
        count: ValueId,
    },
    MissingCapacityGuard {
        function: FunctionId,
        count: ValueId,
        capacity: ValueId,
    },
    MissingAllocationSizeGuard {
        function: FunctionId,
        capacity: ValueId,
        maximum: u64,
    },
    InvalidOwnership { function: FunctionId, object: ValueId },
    InvalidAllocationClass { function: FunctionId, object: ValueId },
    UseAfterConsume { function: FunctionId, object: ValueId },
    UnsupportedMemoryControlFlow { function: FunctionId },
    DuplicateSignature(FunctionSignatureId),
    DuplicateFunction(FunctionId),
    DuplicateBlock { function: FunctionId, block: BlockId },
    DuplicateValue { function: FunctionId, value: ValueId },
    MissingSignature(FunctionSignatureId),
    MissingFunction(FunctionId),
    MissingBlock { function: FunctionId, block: BlockId },
    MissingValueType { function: FunctionId, value: ValueId },
    UndeclaredValue { function: FunctionId, value: ValueId },
    ValueDoesNotDominateUse {
        function: FunctionId,
        value: ValueId,
        block: BlockId,
    },
    TypeMismatch {
        function: FunctionId,
        value: ValueId,
        expected: ValueType,
        actual: ValueType,
    },
    ArityMismatch { context: &'static str, expected: usize, actual: usize },
    DuplicateTailObligation(TailSiteId),
    MissingTailObligation(TailSiteId),
    UnexpectedTailSite(TailSiteId),
    TailSiteFunctionMismatch {
        site: TailSiteId,
        expected: FunctionId,
        actual: FunctionId,
    },
    UnsupportedMemoryOperation { function: FunctionId, operation: &'static str },
    UnsupportedHostCall { function: FunctionId },
    UnsupportedBorrowArgument { function: FunctionId },
    UnsupportedIndirectCall { function: FunctionId },
    UnsupportedMemorySignature { signature: FunctionSignatureId },
    CallableContractMismatch { signature: FunctionSignatureId },
    UnknownCallableContract { function: FunctionId },
}

impl fmt::Display for InitialCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR initial validation failed: {self:?}")
    }
}

impl Error for InitialCheckError {}

/// Validate the currently implemented canonical v1 subset.
///
/// This first checker slice admits only single-block scalar functions. It is
/// intentionally useful for bringing up lowering and tail-site completeness,
/// while rejecting every memory-bearing operation until the corresponding
/// RFC-0165 judgment is implemented. Only success can mint `InitialValid`.
pub fn check_initial(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    check_candidate(&candidate)?;
    Ok(candidate.into_initial_valid())
}

pub fn check_transform(
    candidate: CanonicalModule<TransformedCandidate>,
) -> Result<CanonicalModule<OptimizedValid>, InitialCheckError> {
    check_candidate(&candidate)?;
    Ok(candidate.into_optimized_valid())
}

pub(crate) fn recheck_final(
    candidate: &CanonicalModule<OptimizedValid>,
) -> Result<(), InitialCheckError> {
    check_candidate(candidate)
}

fn check_candidate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
) -> Result<(), InitialCheckError> {
    let (_, layouts) =
        crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
            .map_err(|error| InitialCheckError::InvalidLayoutBundle(error.to_string()))?;
    let signatures = index_signatures(candidate.signatures())?;
    let functions = index_functions(candidate.functions())?;
    contracts::check_all(&functions, &signatures)?;
    let tail_obligations = index_tail_obligations(candidate.proper_tail_sites())?;
    let mut observed_tail_sites = BTreeSet::new();

    for function in candidate.functions() {
        check_function(
            function,
            &signatures,
            &functions,
            &tail_obligations,
            &mut observed_tail_sites,
            &layouts,
        )?;
    }

    for site in tail_obligations.keys() {
        if !observed_tail_sites.contains(site) {
            return Err(InitialCheckError::MissingTailObligation(*site));
        }
    }

    Ok(())
}

fn index_signatures(
    signatures: &[FunctionSignature],
) -> Result<BTreeMap<FunctionSignatureId, &FunctionSignature>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for signature in signatures {
        if result.insert(signature.id, signature).is_some() {
            return Err(InitialCheckError::DuplicateSignature(signature.id));
        }
    }
    Ok(result)
}

fn index_functions(
    functions: &[Function],
) -> Result<BTreeMap<FunctionId, &Function>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for function in functions {
        if result.insert(function.id, function).is_some() {
            return Err(InitialCheckError::DuplicateFunction(function.id));
        }
    }
    Ok(result)
}

fn index_tail_obligations(
    obligations: &[ProperTailSite],
) -> Result<BTreeMap<TailSiteId, FunctionId>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for obligation in obligations {
        if result.insert(obligation.id, obligation.function).is_some() {
            return Err(InitialCheckError::DuplicateTailObligation(obligation.id));
        }
    }
    Ok(result)
}

fn check_function(
    function: &Function,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
    tail_obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
    layouts: &crate::layout::LayoutInterner,
) -> Result<(), InitialCheckError> {
    let signature = signatures
        .get(&function.signature)
        .copied()
        .ok_or(InitialCheckError::MissingSignature(function.signature))?;

    let blocks = index_blocks(function)?;
    let entry = blocks
        .get(&function.entry)
        .copied()
        .ok_or(InitialCheckError::MissingBlock {
            function: function.id,
            block: function.entry,
        })?;
    check_entry_parameters(function, entry, signature)?;
    check_value_definitions(function)?;

    let predecessors = predecessor_map(function, &blocks)?;
    let dominators = dominators(function, &blocks, &predecessors)?;
    let definition_sites = definition_sites(function);
    let mut memory = MemoryChecker::new(function, layouts)?;

    for block in &function.blocks {
        for (operation_index, operation) in block.operations.iter().enumerate() {
            check_uses_dominate(
                function,
                block.id,
                operation_index,
                operation_uses(operation),
                &definition_sites,
                &dominators,
            )?;
            check_operation(function, operation, signatures, functions, &mut memory)?;
        }
        check_uses_dominate(
            function,
            block.id,
            block.operations.len(),
            terminator_uses(&block.terminator),
            &definition_sites,
            &dominators,
        )?;
        check_terminator(
            function,
            &block.terminator,
            signature,
            signatures,
            functions,
            &blocks,
            tail_obligations,
            observed_tail_sites,
        )?;
    }
    Ok(())
}

fn index_blocks(function: &Function) -> Result<BTreeMap<BlockId, &BasicBlock>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for block in &function.blocks {
        if result.insert(block.id, block).is_some() {
            return Err(InitialCheckError::DuplicateBlock {
                function: function.id,
                block: block.id,
            });
        }
    }
    Ok(result)
}

fn check_entry_parameters(
    function: &Function,
    block: &BasicBlock,
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    require_arity("entry parameters", signature.parameters.len(), block.parameters.len())?;
    for (parameter_index, (parameter, expected)) in
        block.parameters.iter().zip(&signature.parameters).enumerate()
    {
        require_type(function, parameter.value, *expected)?;
        if parameter.value_type != *expected {
            return Err(InitialCheckError::TypeMismatch {
                function: function.id,
                value: parameter.value,
                expected: *expected,
                actual: parameter.value_type,
            });
        }
        let contract = &signature.contract.parameters[parameter_index];
        match contract.ownership {
            ParameterOwnership::Value => {
                if parameter.ownership.is_some() {
                    return Err(InitialCheckError::UnsupportedMemorySignature {
                        signature: signature.id,
                    });
                }
            }
            ParameterOwnership::Unique => {
                if parameter.ownership != Some(super::Ownership::Unique) {
                    return Err(InitialCheckError::CallableContractMismatch {
                        signature: signature.id,
                    });
                }
            }
            ParameterOwnership::Shared => {
                if parameter.ownership != Some(super::Ownership::Shared) {
                    return Err(InitialCheckError::CallableContractMismatch {
                        signature: signature.id,
                    });
                }
            }
            ParameterOwnership::CallBorrow { .. } => {
                if parameter.ownership.is_some() {
                    return Err(InitialCheckError::CallableContractMismatch {
                        signature: signature.id,
                    });
                }
            }
        }
    }
    Ok(())
}

fn check_value_definitions(function: &Function) -> Result<(), InitialCheckError> {
    let mut definitions = BTreeSet::new();
    for block in &function.blocks {
        for parameter in &block.parameters {
            insert_definition(function.id, &mut definitions, parameter.value)?;
        }
        for operation in &block.operations {
            for destination in operation_destinations(operation) {
                insert_definition(function.id, &mut definitions, destination)?;
            }
        }
    }
    for value in &definitions {
        if !function.value_types.contains_key(value) {
            return Err(InitialCheckError::MissingValueType {
                function: function.id,
                value: *value,
            });
        }
    }
    for value in function.value_types.keys() {
        if !definitions.contains(value) {
            return Err(InitialCheckError::UndeclaredValue {
                function: function.id,
                value: *value,
            });
        }
    }
    Ok(())
}

fn insert_definition(
    function: FunctionId,
    definitions: &mut BTreeSet<ValueId>,
    value: ValueId,
) -> Result<(), InitialCheckError> {
    if !definitions.insert(value) {
        return Err(InitialCheckError::DuplicateValue { function, value });
    }
    Ok(())
}

fn operation_destinations(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ConstInt { destination, .. }
        | Operation::ConstFloat { destination, .. }
        | Operation::ConstBool { destination, .. }
        | Operation::ConstSize { destination, .. }
        | Operation::Arithmetic { destination, .. }
        | Operation::Compare { destination, .. }
        | Operation::NarrowSize { destination, .. }
        | Operation::Alloc { destination, .. }
        | Operation::StaticData { destination, .. }
        | Operation::Load { destination, .. }
        | Operation::MoveValue { destination, .. }
        | Operation::Share { destination, .. } => vec![*destination],
        Operation::Call { destinations, .. }
        | Operation::VarCall { destinations, .. }
        | Operation::HostCall { destinations, .. } => destinations.clone(),
        Operation::Guard { .. }
        | Operation::Store { .. }
        | Operation::CopyBytes { .. }
        | Operation::MoveRange { .. }
        | Operation::CloneRange { .. }
        | Operation::UpdateCount { .. }
        | Operation::Retain { .. }
        | Operation::Release { .. }
        | Operation::Reclaim { .. } => Vec::new(),
    }
}

fn operation_uses(operation: &Operation) -> Vec<ValueId> {
    match operation {
        Operation::ConstInt { .. }
        | Operation::ConstFloat { .. }
        | Operation::ConstBool { .. }
        | Operation::ConstSize { .. }
        | Operation::Alloc { .. }
        | Operation::StaticData { .. } => Vec::new(),
        Operation::Arithmetic { left, right, .. }
        | Operation::Compare { left, right, .. } => vec![*left, *right],
        Operation::NarrowSize { value, .. }
        | Operation::Guard { condition: value, .. }
        | Operation::MoveValue { source: value, .. }
        | Operation::Share { source: value, .. }
        | Operation::Retain { object: value }
        | Operation::Release { object: value }
        | Operation::Reclaim { object: value } => vec![*value],
        Operation::Load { place, .. } => place_uses(place),
        Operation::Store { place, value, .. } => {
            let mut result = place_uses(place);
            result.push(*value);
            result
        }
        Operation::CopyBytes { destination, source, length }
        | Operation::MoveRange { destination, source, count: length }
        | Operation::CloneRange { destination, source, count: length } => {
            let mut result = place_uses(destination);
            result.extend(place_uses(source));
            result.push(*length);
            result
        }
        Operation::UpdateCount { object, count } => vec![*object, *count],
        Operation::Call { arguments, .. } | Operation::HostCall { arguments, .. } => {
            argument_uses(arguments)
        }
        Operation::VarCall { callee, arguments, .. } => {
            let mut result = vec![*callee];
            result.extend(argument_uses(arguments));
            result
        }
    }
}

fn place_uses(place: &super::Place) -> Vec<ValueId> {
    let mut result = vec![place.object()];
    match place.projection() {
        super::Projection::Header(_) | super::Projection::Field { .. } => {}
        super::Projection::Element { index } => result.push(*index),
        super::Projection::Slice { start, length } => {
            result.push(*start);
            result.push(*length);
        }
    }
    result
}

fn argument_uses(arguments: &[CallArgument]) -> Vec<ValueId> {
    arguments
        .iter()
        .map(|argument| match argument {
            CallArgument::Value(value) | CallArgument::Move(value) | CallArgument::Share(value) => {
                *value
            }
            CallArgument::Borrow(borrow) => borrow.source(),
        })
        .collect()
}

fn terminator_uses(terminator: &Terminator) -> Vec<ValueId> {
    match terminator {
        Terminator::Branch { arguments, .. } | Terminator::Return { values: arguments } => {
            arguments.clone()
        }
        Terminator::BranchIf {
            condition,
            then_arguments,
            else_arguments,
            ..
        } => {
            let mut result = vec![*condition];
            result.extend(then_arguments.iter().copied());
            result.extend(else_arguments.iter().copied());
            result
        }
        Terminator::TailCall { arguments, .. } => argument_uses(arguments),
        Terminator::VarTailCall { callee, arguments, .. } => {
            let mut result = vec![*callee];
            result.extend(argument_uses(arguments));
            result
        }
        Terminator::Trap | Terminator::Unreachable => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
struct DefinitionSite {
    block: BlockId,
    operation_index: Option<usize>,
}

fn definition_sites(function: &Function) -> BTreeMap<ValueId, DefinitionSite> {
    let mut result = BTreeMap::new();
    for block in &function.blocks {
        for parameter in &block.parameters {
            result.insert(
                parameter.value,
                DefinitionSite {
                    block: block.id,
                    operation_index: None,
                },
            );
        }
        for (operation_index, operation) in block.operations.iter().enumerate() {
            for destination in operation_destinations(operation) {
                result.insert(
                    destination,
                    DefinitionSite {
                        block: block.id,
                        operation_index: Some(operation_index),
                    },
                );
            }
        }
    }
    result
}

fn check_uses_dominate(
    function: &Function,
    use_block: BlockId,
    use_operation_index: usize,
    uses: Vec<ValueId>,
    definitions: &BTreeMap<ValueId, DefinitionSite>,
    dominators: &BTreeMap<BlockId, BTreeSet<BlockId>>,
) -> Result<(), InitialCheckError> {
    for value in uses {
        let definition = definitions
            .get(&value)
            .copied()
            .ok_or(InitialCheckError::UndeclaredValue {
                function: function.id,
                value,
            })?;
        let dominates = if definition.block == use_block {
            definition
                .operation_index
                .is_none_or(|definition_index| definition_index < use_operation_index)
        } else {
            dominators
                .get(&use_block)
                .is_some_and(|set| set.contains(&definition.block))
        };
        if !dominates {
            return Err(InitialCheckError::ValueDoesNotDominateUse {
                function: function.id,
                value,
                block: use_block,
            });
        }
    }
    Ok(())
}

fn predecessor_map(
    function: &Function,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
) -> Result<BTreeMap<BlockId, BTreeSet<BlockId>>, InitialCheckError> {
    let mut predecessors: BTreeMap<_, BTreeSet<_>> =
        blocks.keys().copied().map(|block| (block, BTreeSet::new())).collect();
    for block in &function.blocks {
        for successor in successors(&block.terminator) {
            let incoming = predecessors
                .get_mut(&successor)
                .ok_or(InitialCheckError::MissingBlock {
                    function: function.id,
                    block: successor,
                })?;
            incoming.insert(block.id);
        }
    }
    Ok(predecessors)
}

fn successors(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Branch { target, .. } => vec![*target],
        Terminator::BranchIf { then_target, else_target, .. } => {
            vec![*then_target, *else_target]
        }
        Terminator::Return { .. }
        | Terminator::TailCall { .. }
        | Terminator::VarTailCall { .. }
        | Terminator::Trap
        | Terminator::Unreachable => Vec::new(),
    }
}

fn dominators(
    function: &Function,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
) -> Result<BTreeMap<BlockId, BTreeSet<BlockId>>, InitialCheckError> {
    let all: BTreeSet<_> = blocks.keys().copied().collect();
    let mut reachable = BTreeSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if reachable.insert(block) {
            let body = blocks.get(&block).copied().ok_or(InitialCheckError::MissingBlock {
                function: function.id,
                block,
            })?;
            pending.extend(successors(&body.terminator));
        }
    }
    let mut result = BTreeMap::new();
    for block in blocks.keys().copied() {
        result.insert(
            block,
            if block == function.entry {
                [block].into_iter().collect()
            } else {
                all.clone()
            },
        );
    }
    loop {
        let mut changed = false;
        for block in blocks.keys().copied().filter(|block| *block != function.entry) {
            let incoming = predecessors.get(&block).expect("all blocks have predecessor sets");
            let mut updated = incoming
                .iter()
                .map(|predecessor| result[predecessor].clone())
                .reduce(|left, right| left.intersection(&right).copied().collect())
                .unwrap_or_default();
            updated.insert(block);
            if result[&block] != updated {
                result.insert(block, updated);
                changed = true;
            }
        }
        if !changed {
            return Ok(result);
        }
    }
}

fn check_operation(
    function: &Function,
    operation: &Operation,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
    memory: &mut MemoryChecker<'_>,
) -> Result<(), InitialCheckError> {
    if memory.check(operation)? {
        return Ok(());
    }
    match operation {
        Operation::ConstInt { destination, .. } => {
            require_type(function, *destination, ValueType::Int)
        }
        Operation::ConstFloat { destination, .. } => {
            require_type(function, *destination, ValueType::Float)
        }
        Operation::ConstBool { destination, .. } => {
            require_type(function, *destination, ValueType::Bool)
        }
        Operation::ConstSize { destination, .. } => {
            require_type(function, *destination, ValueType::Size)
        }
        Operation::Arithmetic { destination, left, right, .. } => {
            let result = value_type(function, *destination)?;
            if !matches!(result, ValueType::Int | ValueType::Size) {
                return Err(type_mismatch(function, *destination, ValueType::Int, result));
            }
            require_type(function, *left, result)?;
            require_type(function, *right, result)
        }
        Operation::Compare { destination, left, right, .. } => {
            require_type(function, *destination, ValueType::Bool)?;
            let left_type = value_type(function, *left)?;
            require_type(function, *right, left_type)
        }
        Operation::NarrowSize { destination, value } => {
            require_type(function, *destination, ValueType::Size)?;
            require_type(function, *value, ValueType::Int)
        }
        Operation::Guard { condition, .. } => require_type(function, *condition, ValueType::Bool),
        Operation::MoveValue { destination, source } => {
            let source_type = value_type(function, *source)?;
            if source_type.is_memory_bearing() {
                return unsupported_memory(function, "MoveValue");
            }
            require_type(function, *destination, source_type)
        }
        Operation::Call { destinations, callee, arguments } => {
            let callee = functions
                .get(callee)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            let signature = signatures
                .get(&callee.signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(callee.signature))?;
            check_call(function, destinations, arguments, signature)
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_call(function, destinations, arguments, signature)
        }
        Operation::HostCall { .. } => Err(InitialCheckError::UnsupportedHostCall {
            function: function.id,
        }),
        Operation::Alloc { .. } => unsupported_memory(function, "Alloc"),
        Operation::StaticData { .. } => unsupported_memory(function, "StaticData"),
        Operation::Load { .. } => unsupported_memory(function, "Load"),
        Operation::Store { .. } => unsupported_memory(function, "Store"),
        Operation::CopyBytes { .. } => unsupported_memory(function, "CopyBytes"),
        Operation::MoveRange { .. } => unsupported_memory(function, "MoveRange"),
        Operation::CloneRange { .. } => unsupported_memory(function, "CloneRange"),
        Operation::UpdateCount { .. } => unsupported_memory(function, "UpdateCount"),
        Operation::Share { .. } => unsupported_memory(function, "Share"),
        Operation::Retain { .. } => unsupported_memory(function, "Retain"),
        Operation::Release { .. } => unsupported_memory(function, "Release"),
        Operation::Reclaim { .. } => unsupported_memory(function, "Reclaim"),
    }
}

fn check_call(
    function: &Function,
    destinations: &[ValueId],
    arguments: &[CallArgument],
    signature: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    require_arity("call arguments", signature.parameters.len(), arguments.len())?;
    require_arity("call results", signature.results.len(), destinations.len())?;
    for (argument, expected) in arguments.iter().zip(&signature.contract.parameters) {
        let (value, ownership) = match argument {
            CallArgument::Value(value) => (*value, ParameterOwnership::Value),
            CallArgument::Move(value) => (*value, ParameterOwnership::Unique),
            CallArgument::Share(value) => (*value, ParameterOwnership::Shared),
            CallArgument::Borrow(borrow) => (
                borrow.source(),
                ParameterOwnership::CallBorrow { mode: borrow.mode() },
            ),
        };
        require_type(function, value, expected.value_type)?;
        let accepted = match (expected.ownership, ownership) {
            (ParameterOwnership::Value, ParameterOwnership::Value)
            | (ParameterOwnership::Unique, ParameterOwnership::Unique)
            | (ParameterOwnership::Shared, ParameterOwnership::Shared) => true,
            (
                ParameterOwnership::CallBorrow { mode: expected_mode },
                ParameterOwnership::CallBorrow { mode: actual_mode },
            ) => {
                expected_mode == actual_mode && expected.non_escaping
            }
            _ => false,
        };
        if !accepted {
            return Err(InitialCheckError::UnsupportedBorrowArgument {
                function: function.id,
            });
        }
    }
    for (destination, (expected, contract)) in destinations
        .iter()
        .zip(signature.results.iter().zip(&signature.contract.results))
    {
        require_type(function, *destination, *expected)?;
        let contract_type = match contract {
            super::ResultContract::Value(value_type) => *value_type,
            super::ResultContract::Fresh { layout }
            | super::ResultContract::Shared { layout }
            | super::ResultContract::StaticData { layout }
            | super::ResultContract::AliasesParameter { layout, .. }
            | super::ResultContract::ReallocatesParameter { layout, .. } => {
                ValueType::Ref(*layout)
            }
        };
        if contract_type != *expected {
            return Err(InitialCheckError::CallableContractMismatch {
                signature: signature.id,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_terminator(
    function: &Function,
    terminator: &Terminator,
    current_signature: &FunctionSignature,
    signatures: &BTreeMap<FunctionSignatureId, &FunctionSignature>,
    functions: &BTreeMap<FunctionId, &Function>,
    blocks: &BTreeMap<BlockId, &BasicBlock>,
    tail_obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed_tail_sites: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    match terminator {
        Terminator::Return { values } => {
            require_arity("return values", current_signature.results.len(), values.len())?;
            for (value, expected) in values.iter().zip(&current_signature.results) {
                require_type(function, *value, *expected)?;
            }
            Ok(())
        }
        Terminator::TailCall { site, callee, arguments } => {
            observe_tail_site(function, *site, tail_obligations, observed_tail_sites)?;
            let callee = functions
                .get(callee)
                .copied()
                .ok_or(InitialCheckError::MissingFunction(*callee))?;
            let signature = signatures
                .get(&callee.signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(callee.signature))?;
            check_tail_call(function, arguments, signature, current_signature)
        }
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            observe_tail_site(function, *site, tail_obligations, observed_tail_sites)?;
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            check_tail_call(function, arguments, signature, current_signature)
        }
        Terminator::Branch { target, arguments } => {
            check_branch(function, *target, arguments, blocks)
        }
        Terminator::BranchIf {
            condition,
            then_target,
            then_arguments,
            else_target,
            else_arguments,
        } => {
            require_type(function, *condition, ValueType::Bool)?;
            check_branch(function, *then_target, then_arguments, blocks)?;
            check_branch(function, *else_target, else_arguments, blocks)
        }
        Terminator::Trap | Terminator::Unreachable => Ok(()),
    }
}

fn check_branch(
    function: &Function,
    target: BlockId,
    arguments: &[ValueId],
    blocks: &BTreeMap<BlockId, &BasicBlock>,
) -> Result<(), InitialCheckError> {
    let target_block = blocks
        .get(&target)
        .copied()
        .ok_or(InitialCheckError::MissingBlock {
            function: function.id,
            block: target,
        })?;
    require_arity("branch arguments", target_block.parameters.len(), arguments.len())?;
    for (argument, parameter) in arguments.iter().zip(&target_block.parameters) {
        require_type(function, *argument, parameter.value_type)?;
    }
    Ok(())
}

fn check_tail_call(
    function: &Function,
    arguments: &[CallArgument],
    callee: &FunctionSignature,
    caller: &FunctionSignature,
) -> Result<(), InitialCheckError> {
    if callee.results != caller.results {
        return Err(InitialCheckError::ArityMismatch {
            context: "tail-call result signature",
            expected: caller.results.len(),
            actual: callee.results.len(),
        });
    }
    check_call(function, &[], arguments, &FunctionSignature {
        id: callee.id,
        parameters: callee.parameters.clone(),
        results: Vec::new(),
        contract: callee.contract.clone(),
    })
}

fn observe_tail_site(
    function: &Function,
    site: TailSiteId,
    obligations: &BTreeMap<TailSiteId, FunctionId>,
    observed: &mut BTreeSet<TailSiteId>,
) -> Result<(), InitialCheckError> {
    let expected = obligations
        .get(&site)
        .copied()
        .ok_or(InitialCheckError::UnexpectedTailSite(site))?;
    if expected != function.id {
        return Err(InitialCheckError::TailSiteFunctionMismatch {
            site,
            expected,
            actual: function.id,
        });
    }
    if !observed.insert(site) {
        return Err(InitialCheckError::DuplicateTailObligation(site));
    }
    Ok(())
}

fn value_type(function: &Function, value: ValueId) -> Result<ValueType, InitialCheckError> {
    function
        .value_types
        .get(&value)
        .copied()
        .ok_or(InitialCheckError::MissingValueType {
            function: function.id,
            value,
        })
}

fn require_type(
    function: &Function,
    value: ValueId,
    expected: ValueType,
) -> Result<(), InitialCheckError> {
    let actual = value_type(function, value)?;
    if actual != expected {
        return Err(type_mismatch(function, value, expected, actual));
    }
    Ok(())
}

fn type_mismatch(
    function: &Function,
    value: ValueId,
    expected: ValueType,
    actual: ValueType,
) -> InitialCheckError {
    InitialCheckError::TypeMismatch {
        function: function.id,
        value,
        expected,
        actual,
    }
}

fn require_arity(
    context: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), InitialCheckError> {
    if expected != actual {
        return Err(InitialCheckError::ArityMismatch {
            context,
            expected,
            actual,
        });
    }
    Ok(())
}

fn unsupported_memory<T>(
    function: &Function,
    operation: &'static str,
) -> Result<T, InitialCheckError> {
    Err(InitialCheckError::UnsupportedMemoryOperation {
        function: function.id,
        operation,
    })
}

impl ValueType {
    const fn is_memory_bearing(&self) -> bool {
        matches!(
            self,
            Self::Ref(_) | Self::Capability(_) | Self::Gc(_) | Self::Callable(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::{
        AccessMode, AllocationClass, BasicBlock, BlockId, CallableContract, CanonicalModule,
        CheckedSourceIdentity, ComparisonOperator, Extent, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, GuardKind, LoadTransfer, LoweredModuleParts, Operation, Place,
        Projection, ProperTailSite, ResultContract, Revision, TailSiteId, Terminator, ValueId,
        ValueType,
    };
    use super::{InitialCheckError, check_initial};
    use crate::layout::{LayoutBundle, LayoutId, LayoutInterner, ScalarKind};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const ENTRY: BlockId = BlockId::new(0);
    const VALUE: ValueId = ValueId::new(0);
    const TAIL_SITE: TailSiteId = TailSiteId::from_digest([11; 32]);

    fn contract(results: Vec<ResultContract>) -> CallableContract {
        CallableContract {
            parameters: Vec::new(),
            results,
            parameter_effects: BTreeMap::new(),
        }
    }

    fn empty_layouts() -> LayoutBundle {
        LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("an empty canonical layout bundle is valid")
    }

    fn dynamic_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout is valid");
        let list = layouts
            .intern_runtime_buffer(element)
            .expect("u32 runtime buffer layout is valid");
        let bundle = LayoutBundle::from_interner(&layouts, [list])
            .expect("runtime buffer layout bundle is valid");
        (bundle, list)
    }

    fn lowered(
        value_types: BTreeMap<ValueId, ValueType>,
        operations: Vec<Operation>,
        terminator: Terminator,
        result_types: Vec<ValueType>,
        tail_sites: Vec<ProperTailSite>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let result_contracts = result_types
            .iter()
            .copied()
            .map(ResultContract::Value)
            .collect();
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: result_types,
                contract: contract(result_contracts),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator,
                }],
            }],
            proper_tail_sites: tail_sites,
        })
    }

    #[test]
    fn scalar_module_mints_initial_valid() {
        let module = lowered(
            [(VALUE, ValueType::Int)].into_iter().collect(),
            vec![Operation::ConstInt {
                destination: VALUE,
                value: 42,
            }],
            Terminator::Return {
                values: vec![VALUE],
            },
            vec![ValueType::Int],
            Vec::new(),
        );

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn proper_tail_site_is_matched_exactly_once() {
        let module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: FUNCTION,
                arguments: Vec::new(),
            },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
            }],
        );

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn ordinary_return_cannot_discharge_a_tail_obligation() {
        let module = lowered(
            BTreeMap::new(),
            Vec::new(),
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
            }],
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::MissingTailObligation(TAIL_SITE)
        );
    }

    #[test]
    fn reclaim_without_unique_object_authority_fails_closed() {
        let module = lowered(
            [(VALUE, ValueType::Int)].into_iter().collect(),
            vec![
                Operation::ConstInt {
                    destination: VALUE,
                    value: 0,
                },
                Operation::Reclaim { object: VALUE },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidOwnership {
                function: FUNCTION,
                object: VALUE,
            }
        );
    }

    #[test]
    fn block_arguments_are_typed_ssa_definitions() {
        let next = BlockId::new(1);
        let parameter = ValueId::new(1);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Int)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int), (parameter, ValueType::Int)]
                    .into_iter()
                    .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: VALUE,
                            value: 42,
                        }],
                        terminator: Terminator::Branch {
                            target: next,
                            arguments: vec![VALUE],
                        },
                    },
                    BasicBlock {
                        id: next,
                        parameters: vec![super::super::BlockParameter {
                            value: parameter,
                            value_type: ValueType::Int,
                            ownership: None,
                        }],
                        operations: Vec::new(),
                        terminator: Terminator::Return {
                            values: vec![parameter],
                        },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert!(check_initial(module).is_ok());
    }

    #[test]
    fn definition_on_one_branch_does_not_dominate_join() {
        let condition = ValueId::new(1);
        let then_block = BlockId::new(1);
        let else_block = BlockId::new(2);
        let join_block = BlockId::new(3);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Int)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int), (condition, ValueType::Bool)]
                    .into_iter()
                    .collect(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstBool {
                            destination: condition,
                            value: true,
                        }],
                        terminator: Terminator::BranchIf {
                            condition,
                            then_target: then_block,
                            then_arguments: Vec::new(),
                            else_target: else_block,
                            else_arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: then_block,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstInt {
                            destination: VALUE,
                            value: 1,
                        }],
                        terminator: Terminator::Branch {
                            target: join_block,
                            arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: else_block,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Branch {
                            target: join_block,
                            arguments: Vec::new(),
                        },
                    },
                    BasicBlock {
                        id: join_block,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return {
                            values: vec![VALUE],
                        },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::ValueDoesNotDominateUse {
                function: FUNCTION,
                value: VALUE,
                block: join_block,
            }
        );
    }

    fn dynamic_load_module(
        with_guard: bool,
        capacity_value: u64,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let index = ValueId::new(2);
        let condition = ValueId::new(3);
        let object = ValueId::new(4);
        let loaded = ValueId::new(5);
        let (layouts, layout) = dynamic_u32_layout();
        let mut operations = vec![
            Operation::ConstSize {
                destination: count,
                value: 1,
            },
            Operation::ConstSize {
                destination: capacity,
                value: capacity_value,
            },
            Operation::ConstSize {
                destination: index,
                value: 0,
            },
            Operation::Compare {
                destination: condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: count,
            },
        ];
        if with_guard {
            operations.push(Operation::Guard {
                condition,
                kind: GuardKind::Bounds,
            });
        }
        operations.extend([
            Operation::Alloc {
                destination: object,
                layout,
                extent: Extent::Counted {
                    header_bytes: 4,
                    count,
                    capacity,
                    stride_bytes: 4,
                },
                class: AllocationClass::ReferenceCounted,
            },
            Operation::Load {
                destination: loaded,
                place: Place::projected(
                    object,
                    Projection::Element { index },
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            },
            Operation::Reclaim { object },
        ]);

        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [
                    (count, ValueType::Size),
                    (capacity, ValueType::Size),
                    (index, ValueType::Size),
                    (condition, ValueType::Bool),
                    (object, ValueType::Ref(layout)),
                    (loaded, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations,
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn guarded_dynamic_element_load_is_admitted() {
        assert!(check_initial(dynamic_load_module(true, 1)).is_ok());
    }

    #[test]
    fn dynamic_element_load_without_guard_is_refused() {
        assert_eq!(
            check_initial(dynamic_load_module(false, 1)).unwrap_err(),
            InitialCheckError::MissingBoundsGuard {
                function: FUNCTION,
                index: ValueId::new(2),
                count: ValueId::new(0),
            }
        );
    }

    #[test]
    fn dynamic_allocation_extent_must_fit_wasm_address_space() {
        assert_eq!(
            check_initial(dynamic_load_module(true, u64::MAX)).unwrap_err(),
            InitialCheckError::MissingAllocationSizeGuard {
                function: FUNCTION,
                capacity: ValueId::new(1),
                maximum: (u64::from(u32::MAX) - 4) / 4,
            }
        );
    }

    #[test]
    fn declared_callable_result_must_match_checked_signature() {
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: contract(vec![ResultContract::Value(ValueType::Float)]),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, ValueType::Int)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt {
                        destination: VALUE,
                        value: 42,
                    }],
                    terminator: Terminator::Return {
                        values: vec![VALUE],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::CallableContractMismatch {
                signature: SIGNATURE,
            }
        );
    }

    #[test]
    fn valid_unreachable_block_is_available_to_pruning() {
        let dead = BlockId::new(1);
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: empty_layouts(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: contract(Vec::new()),
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: BTreeMap::new(),
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                    BasicBlock {
                        id: dead,
                        parameters: Vec::new(),
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });

        assert!(check_initial(module).is_ok());
    }
}
