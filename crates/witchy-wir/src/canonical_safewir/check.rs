use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use super::{
    BasicBlock, BlockId, CallArgument, CanonicalAbiCatalog, CanonicalModule, Function, FunctionId,
    FunctionSignature, FunctionSignatureId, GuardKind, HostCallArgument, ImportContract, ImportId, InitialValid,
    LoweredCandidate, ModuleStage, Operation,
    OptimizedValid,
    ParameterOwnership, ProperTailSite, TailSiteId, Terminator, TransformedCandidate, ValueId,
    ValueType,
};

mod memory;
use memory::MemoryChecker;
mod contracts;
mod host;

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
    InvalidGuard {
        function: FunctionId,
        condition: ValueId,
        kind: GuardKind,
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
    DuplicateImport(ImportId),
    MissingImportContract { function: FunctionId, import: ImportId },
    UnknownImportIdentity { import: ImportId },
    HostContractRoleMismatch { import: ImportId },
    HostParameterRoleMismatch { function: FunctionId, import: ImportId, parameter: usize },
    HostResultRoleMismatch {
        function: FunctionId,
        import: ImportId,
        result: usize,
        expected: ValueType,
        actual: ValueType,
    },
    HostBufferExtentMismatch { function: FunctionId, import: ImportId, parameter: usize },
    UnknownHostBufferExtent { import: ImportId },
    UnsupportedHostMutation { import: ImportId },
    UnsupportedHostReentrancy { import: ImportId },
    MissingCanonicalAbiCatalog,
    InvalidCanonicalAbiContract { import: ImportId },
    UnsupportedHostBuffer { function: FunctionId, import: ImportId, parameter: usize },
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
/// The memory slice is deliberately small and fail-closed: generic allocation,
/// counted collections, projected scalar access, explicit runtime guards, and
/// structural ownership are admitted; loans and pointer-bearing copies are not.
pub fn check_initial(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    check_candidate(&candidate, None)?;
    Ok(candidate.into_initial_valid())
}

pub(crate) fn check_initial_with_catalog(
    candidate: CanonicalModule<LoweredCandidate>,
    catalog: &CanonicalAbiCatalog,
) -> Result<CanonicalModule<InitialValid>, InitialCheckError> {
    check_candidate(&candidate, Some(catalog))?;
    Ok(candidate.into_initial_valid())
}

pub fn check_transform(
    candidate: CanonicalModule<TransformedCandidate>,
) -> Result<CanonicalModule<OptimizedValid>, InitialCheckError> {
    check_candidate(&candidate, None)?;
    Ok(candidate.into_optimized_valid())
}

pub(crate) fn recheck_final(
    candidate: &CanonicalModule<OptimizedValid>,
) -> Result<(), InitialCheckError> {
    check_candidate(candidate, None)
}

fn check_candidate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    catalog: Option<&CanonicalAbiCatalog>,
) -> Result<(), InitialCheckError> {
    let (_, layouts) =
        crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
            .map_err(|error| InitialCheckError::InvalidLayoutBundle(error.to_string()))?;
    let signatures = index_signatures(candidate.signatures())?;
    let imports = index_imports(candidate.imports())?;
    let functions = index_functions(candidate.functions())?;
    contracts::check_all(&functions, &signatures)?;
    host::check_imports(&imports, &signatures, catalog)?;
    let tail_obligations = index_tail_obligations(candidate.proper_tail_sites())?;
    let mut observed_tail_sites = BTreeSet::new();

    for function in candidate.functions() {
        check_function(
            function,
            &signatures,
            &imports,
            catalog,
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

fn index_imports(
    imports: &[ImportContract],
) -> Result<BTreeMap<ImportId, &ImportContract>, InitialCheckError> {
    let mut result = BTreeMap::new();
    for import in imports {
        if result.insert(import.id, import).is_some() {
            return Err(InitialCheckError::DuplicateImport(import.id));
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
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
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
    let mut memory = MemoryChecker::new(function, layouts, signature)?;

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
            check_operation(function, operation, signatures, imports, catalog, functions, &mut memory)?;
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
            &mut memory,
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
        Operation::Call { arguments, .. } => argument_uses(arguments),
        Operation::HostCall { arguments, .. } => host_argument_uses(arguments),
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

fn host_argument_uses(arguments: &[HostCallArgument]) -> Vec<ValueId> {
    let mut result = Vec::new();
    for argument in arguments {
        match argument {
            HostCallArgument::Value(value) | HostCallArgument::Capability(value) => result.push(*value),
            HostCallArgument::Buffer { place, .. } => result.extend(place_uses(place)),
        }
    }
    result
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
    imports: &BTreeMap<ImportId, &ImportContract>,
    catalog: Option<&CanonicalAbiCatalog>,
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
            check_call(function, destinations, arguments, signature, Some(memory))
        }
        Operation::VarCall { destinations, callee, signature, arguments } => {
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            reject_indirect_memory_call(function, destinations, arguments)?;
            check_call(function, destinations, arguments, signature, None)
        }
        Operation::HostCall { destinations, import, arguments } => {
            host::check_call(function, *import, destinations, arguments, imports, signatures, catalog)
        }
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
    mut memory: Option<&mut MemoryChecker<'_>>,
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
    if let Some(memory) = memory.as_deref_mut() {
        memory.check_call(
            arguments,
            &signature.contract.parameters,
            destinations,
            &signature.contract.results,
        )?;
    }
    Ok(())
}

fn reject_indirect_memory_call(
    function: &Function,
    destinations: &[ValueId],
    arguments: &[CallArgument],
) -> Result<(), InitialCheckError> {
    if arguments.iter().any(|argument| {
        let value = match argument {
            CallArgument::Value(value)
            | CallArgument::Move(value)
            | CallArgument::Share(value) => *value,
            CallArgument::Borrow(borrow) => borrow.source(),
        };
        value_type(function, value)
            .map(|value_type| value_type.is_memory_bearing())
            .unwrap_or(true)
    }) || destinations.iter().any(|destination| {
        value_type(function, *destination)
            .map(|value_type| value_type.is_memory_bearing())
            .unwrap_or(true)
    }) {
        return Err(InitialCheckError::UnsupportedIndirectCall {
            function: function.id,
        });
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
    memory: &mut MemoryChecker<'_>,
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
            check_tail_call(function, arguments, signature, current_signature, Some(memory))
        }
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            observe_tail_site(function, *site, tail_obligations, observed_tail_sites)?;
            require_type(function, *callee, ValueType::Callable(*signature))?;
            let signature = signatures
                .get(signature)
                .copied()
                .ok_or(InitialCheckError::MissingSignature(*signature))?;
            reject_indirect_memory_call(function, &[], arguments)?;
            check_tail_call(function, arguments, signature, current_signature, None)
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
    memory: Option<&mut MemoryChecker<'_>>,
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
    }, memory)
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
        AccessMode, AllocationClass, BasicBlock, BlockId, BlockParameter, CallableContract,
        CanonicalAbiCatalog, CanonicalModule,
        CheckedSourceIdentity, ComparisonOperator, Extent, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, GuardKind, HeaderField, HostBufferExtent, HostCallArgument,
        HostMutation, HostParameterRole, HostReentrancy, HostResultRole, ImportContract,
        ImportContractIdentity, ImportId, LoadTransfer, LoweredModuleParts, MemoryEffect, Operation, ParameterContract,
        ParameterOwnership, Place, Projection, ProperTailSite, ResultContract, Revision, TailSiteId,
        Terminator, ValueId, ValueType,
    };
    use super::{InitialCheckError, check_initial};
    use crate::layout::{LayoutBundle, LayoutId, LayoutInterner, RcHeader, ScalarKind};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const CALLEE: FunctionId = FunctionId::new(1);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const CALLEE_SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(1);
    const ENTRY: BlockId = BlockId::new(0);
    const VALUE: ValueId = ValueId::new(0);
    const TAIL_SITE: TailSiteId = TailSiteId::from_digest([11; 32]);

    fn call_contract(
        layout: LayoutId,
        ownership: ParameterOwnership,
        results: Vec<ResultContract>,
        parameter_effects: BTreeMap<u32, Vec<MemoryEffect>>,
    ) -> CallableContract {
        CallableContract {
            parameters: vec![ParameterContract {
                value_type: ValueType::Ref(layout),
                ownership,
                non_escaping: matches!(ownership, ParameterOwnership::CallBorrow { .. }),
            }],
            results,
            parameter_effects,
        }
    }

    fn call_module(
        layouts: LayoutBundle,
        caller_values: BTreeMap<ValueId, ValueType>,
        caller_operations: Vec<Operation>,
        caller_terminator: Terminator,
        callee_operations: Vec<Operation>,
        callee_values: BTreeMap<ValueId, ValueType>,
        callee_terminator: Terminator,
        callee_contract: CallableContract,
        callee_results: Vec<ValueType>,
        callee_parameters: Vec<BlockParameter>,
        tail_sites: Vec<ProperTailSite>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![
                FunctionSignature {
                    id: SIGNATURE,
                    parameters: Vec::new(),
                    results: Vec::new(),
                    contract: CallableContract {
                        parameters: Vec::new(),
                        results: Vec::new(),
                        parameter_effects: BTreeMap::new(),
                    },
                },
                FunctionSignature {
                    id: CALLEE_SIGNATURE,
                    parameters: callee_parameters
                        .iter()
                        .map(|parameter| parameter.value_type)
                        .collect(),
                    results: callee_results
                        .iter()
                        .copied()
                        .collect(),
                    contract: callee_contract,
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: FUNCTION,
                    signature: SIGNATURE,
                    entry: ENTRY,
                    value_types: caller_values,
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: caller_operations,
                        terminator: caller_terminator,
                    }],
                },
                Function {
                    id: CALLEE,
                    signature: CALLEE_SIGNATURE,
                    entry: ENTRY,
                    value_types: callee_values,
                    blocks: vec![BasicBlock {
                        id: ENTRY,
                        parameters: callee_parameters,
                        operations: callee_operations,
                        terminator: callee_terminator,
                    }],
                },
            ],
            proper_tail_sites: tail_sites,
        })
    }

    fn allocated_list_with_header(
        layout: LayoutId,
        header_bytes: u32,
    ) -> (BTreeMap<ValueId, ValueType>, Vec<Operation>) {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        (
            [
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (object, ValueType::Ref(layout)),
            ]
            .into_iter()
            .collect(),
            vec![
                Operation::ConstSize { destination: count, value: 0 },
                Operation::ConstSize { destination: capacity, value: 1 },
                Operation::Alloc {
                    destination: object,
                    layout,
                    extent: Extent::Counted {
                        header_bytes,
                        count,
                        capacity,
                        stride_bytes: 4,
                    },
                    class: AllocationClass::ReferenceCounted,
                },
            ],
        )
    }

    fn allocated_list(layout: LayoutId) -> (BTreeMap<ValueId, ValueType>, Vec<Operation>) {
        allocated_list_with_header(layout, 4)
    }

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

    fn packed_u32_layout() -> (LayoutBundle, LayoutId) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("u32 layout is valid");
        let list = layouts
            .intern_packed_list(element, RcHeader::Required)
            .expect("u32 packed list layout is valid");
        let bundle = LayoutBundle::from_interner(&layouts, [list])
            .expect("packed list layout bundle is valid");
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

    fn host_module(
        function_parameter: ValueType,
        import_parameter: ValueType,
        argument: HostCallArgument,
        layouts: LayoutBundle,
        imports: Vec<ImportContract>,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let function_signature = FunctionSignature {
            id: SIGNATURE,
            parameters: vec![function_parameter],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: function_parameter,
                    ownership: ParameterOwnership::Value,
                    non_escaping: false,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        let import_signature = FunctionSignature {
            id: FunctionSignatureId::new(1),
            parameters: vec![import_parameter],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![ParameterContract {
                    value_type: import_parameter,
                    ownership: ParameterOwnership::Value,
                    non_escaping: false,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts,
            signatures: vec![function_signature, import_signature],
            imports,
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: [(VALUE, function_parameter), (ValueId::new(1), ValueType::Int)]
                    .into_iter()
                    .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: vec![BlockParameter {
                        value: VALUE,
                        value_type: function_parameter,
                        ownership: None,
                    }],
                    operations: vec![Operation::HostCall {
                        destinations: vec![ValueId::new(1)],
                        import: ImportId::new(0),
                        arguments: vec![argument],
                    }],
                    terminator: Terminator::Return {
                        values: vec![ValueId::new(1)],
                    },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    fn host_import(
        parameter_role: HostParameterRole,
        result_role: HostResultRole,
        mutation: HostMutation,
        reentrancy: HostReentrancy,
    ) -> ImportContract {
        ImportContract {
            id: ImportId::new(0),
            signature: FunctionSignatureId::new(1),
            abi_identity: ImportContractIdentity::AuthenticatedAbi([9; 32]),
            parameter_roles: vec![parameter_role],
            result_roles: vec![result_role],
            mutation,
            reentrancy,
        }
    }

    #[test]
    fn typed_capability_host_call_is_admitted() {
        let kind = super::super::CapabilityKindId::new(4);
        let import = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Capability(kind),
            ValueType::Capability(kind),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(super::check_initial_with_catalog(module, &catalog).is_ok());
    }

    #[test]
    fn integer_cannot_fill_capability_host_role() {
        let kind = super::super::CapabilityKindId::new(4);
        let import = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Int,
            ValueType::Capability(kind),
            HostCallArgument::Value(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn wrong_capability_kind_is_rejected() {
        let expected = super::super::CapabilityKindId::new(4);
        let actual = super::super::CapabilityKindId::new(5);
        let import = host_import(
            HostParameterRole::Capability(expected),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Capability(actual),
            ValueType::Capability(expected),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn unknown_import_contract_is_rejected() {
        let module = lowered(
            [(ValueId::new(1), ValueType::Int)].into_iter().collect(),
            vec![Operation::HostCall {
                destinations: vec![ValueId::new(1)],
                import: ImportId::new(99),
                arguments: vec![],
            }],
            Terminator::Return {
                values: vec![ValueId::new(1)],
            },
            vec![ValueType::Int],
            Vec::new(),
        );
        let result = check_initial(module);
        assert!(
            matches!(result.as_ref(), Err(InitialCheckError::MissingImportContract { .. })),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn candidate_abi_bytes_do_not_authenticate_an_import() {
        let kind = super::super::CapabilityKindId::new(4);
        let expected = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![expected]);
        let mut forged = host_import(
            HostParameterRole::Capability(kind),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        forged.abi_identity = ImportContractIdentity::AuthenticatedAbi([8; 32]);
        let module = host_module(
            ValueType::Capability(kind),
            ValueType::Capability(kind),
            HostCallArgument::Capability(VALUE),
            empty_layouts(),
            vec![forged],
        );
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::InvalidCanonicalAbiContract { .. })
        ));
    }

    #[test]
    fn host_buffer_extent_must_satisfy_contract() {
        let (layouts, layout) = dynamic_u32_layout();
        let place = Place::projected(
            VALUE,
            Projection::Field { index: 0 },
            ValueType::Ref(layout),
            AccessMode::Read,
        );
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Exact(8),
        };
        let import = host_import(
            role.clone(),
            HostResultRole::Value(ValueType::Int),
            HostMutation::ReadOnly,
            HostReentrancy::Forbidden,
        );
        let module = host_module(
            ValueType::Ref(layout),
            ValueType::Ref(layout),
            HostCallArgument::Buffer {
                place,
                extent: HostBufferExtent::Exact(4),
            },
            layouts,
            vec![import.clone()],
        );
        let catalog = CanonicalAbiCatalog::from_imports(vec![import]);
        assert!(matches!(
            super::check_initial_with_catalog(module, &catalog),
            Err(InitialCheckError::UnsupportedHostBuffer { .. })
        ));
    }

    #[test]
    fn forged_field_and_element_buffer_projections_are_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let role = HostParameterRole::Buffer {
            value_type: ValueType::Ref(layout),
            access: AccessMode::Read,
            extent: HostBufferExtent::Exact(8),
        };
        for projection in [
            Projection::Field { index: 99 },
            Projection::Element { index: VALUE },
        ] {
            let import = host_import(
                role.clone(),
                HostResultRole::Value(ValueType::Int),
                HostMutation::ReadOnly,
                HostReentrancy::Forbidden,
            );
            let catalog = CanonicalAbiCatalog::from_imports(vec![import.clone()]);
            let module = host_module(
                ValueType::Ref(layout),
                ValueType::Ref(layout),
                HostCallArgument::Buffer {
                    place: Place::projected(
                        VALUE,
                        projection,
                        ValueType::Ref(layout),
                        AccessMode::Read,
                    ),
                    extent: HostBufferExtent::Exact(8),
                },
                layouts.clone(),
                vec![import],
            );
            assert!(matches!(
                super::check_initial_with_catalog(module, &catalog),
                Err(InitialCheckError::UnsupportedHostBuffer { .. })
            ));
        }
    }

    #[test]
    fn unsupported_host_mutation_and_reentrancy_fail_closed() {
        let kind = super::super::CapabilityKindId::new(4);
        for (mutation, reentrancy, expected) in [
            (HostMutation::Unknown, HostReentrancy::Forbidden, true),
            (HostMutation::ReadOnly, HostReentrancy::Allowed, true),
        ] {
            let import = host_import(
                HostParameterRole::Capability(kind),
                HostResultRole::Value(ValueType::Int),
                mutation,
                reentrancy,
            );
            let import_for_catalog = import.clone();
            let result = super::check_initial_with_catalog(host_module(
                ValueType::Capability(kind),
                ValueType::Capability(kind),
                HostCallArgument::Capability(VALUE),
                empty_layouts(),
                vec![import],
            ), &CanonicalAbiCatalog::from_imports(vec![import_for_catalog]));
            assert_eq!(result.is_err(), expected);
        }
    }

    #[test]
    fn host_mutation_must_match_explicit_buffer_access() {
        let (layouts, layout) = dynamic_u32_layout();
        for (access, mutation) in [
            (AccessMode::Write, HostMutation::ReadOnly),
            (AccessMode::Read, HostMutation::WriteBuffers),
        ] {
            let role = HostParameterRole::Buffer {
                value_type: ValueType::Ref(layout),
                access,
                extent: HostBufferExtent::Exact(8),
            };
            let import = host_import(
                role,
                HostResultRole::Value(ValueType::Int),
                mutation,
                HostReentrancy::Forbidden,
            );
            let catalog = CanonicalAbiCatalog::from_imports(vec![import.clone()]);
            let result = super::check_initial_with_catalog(
                host_module(
                    ValueType::Ref(layout),
                    ValueType::Ref(layout),
                    HostCallArgument::Buffer {
                        place: Place::projected(
                            VALUE,
                            Projection::Field { index: 0 },
                            ValueType::Ref(layout),
                            access,
                        ),
                        extent: HostBufferExtent::Exact(8),
                    },
                    layouts.clone(),
                    vec![import],
                ),
                &catalog,
            );
            assert!(matches!(
                result,
                Err(InitialCheckError::UnsupportedHostMutation { .. })
            ));
        }
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

    fn counted_access_module(
        count_value: u64,
        capacity_value: u64,
        index_value: u64,
        guard_right: ValueId,
        with_guard: bool,
    ) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let index = ValueId::new(2);
        let condition = ValueId::new(3);
        let object = ValueId::new(4);
        let loaded = ValueId::new(5);
        let (layouts, layout) = dynamic_u32_layout();
        let mut operations = vec![
            Operation::ConstSize { destination: count, value: count_value },
            Operation::ConstSize { destination: capacity, value: capacity_value },
            Operation::ConstSize { destination: index, value: index_value },
            Operation::Compare {
                destination: condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: guard_right,
            },
        ];
        if with_guard {
            operations.push(Operation::Guard { condition, kind: GuardKind::Bounds });
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
    fn logical_length_guard_is_not_satisfied_by_capacity_guard() {
        let capacity = ValueId::new(1);
        assert_eq!(
            check_initial(counted_access_module(1, 2, 1, capacity, true)).unwrap_err(),
            InitialCheckError::MissingBoundsGuard {
                function: FUNCTION,
                index: ValueId::new(2),
                count: ValueId::new(0),
            }
        );
    }

    #[test]
    fn update_count_grows_logical_length_only_after_capacity_guard() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let new_count = ValueId::new(2);
        let index = ValueId::new(3);
        let capacity_condition = ValueId::new(4);
        let index_condition = ValueId::new(5);
        let object = ValueId::new(6);
        let loaded = ValueId::new(7);
        let (layouts, layout) = dynamic_u32_layout();
        let operations = vec![
            Operation::ConstSize { destination: count, value: 1 },
            Operation::ConstSize { destination: capacity, value: 2 },
            Operation::ConstSize { destination: new_count, value: 2 },
            Operation::ConstSize { destination: index, value: 1 },
            Operation::Compare {
                destination: capacity_condition,
                operator: ComparisonOperator::LessEqualUnsigned,
                left: new_count,
                right: capacity,
            },
            Operation::Guard { condition: capacity_condition, kind: GuardKind::Bounds },
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
            Operation::UpdateCount { object, count: new_count },
            Operation::Compare {
                destination: index_condition,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: new_count,
            },
            Operation::Guard { condition: index_condition, kind: GuardKind::Bounds },
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
        ];
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
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
                    (new_count, ValueType::Size),
                    (index, ValueType::Size),
                    (capacity_condition, ValueType::Bool),
                    (index_condition, ValueType::Bool),
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
        });
        assert!(check_initial(module).is_ok());
    }

    fn header_load_module(length_type: ValueType) -> CanonicalModule<super::super::LoweredCandidate> {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let loaded_length = ValueId::new(3);
        let loaded_capacity = ValueId::new(4);
        let (layouts, layout) = packed_u32_layout();
        let header = |field, destination, value_type| Operation::Load {
            destination,
            place: Place::projected(
                object,
                Projection::Header(field),
                value_type,
                AccessMode::Read,
            ),
            transfer: LoadTransfer::CopyScalar,
        };
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
                    (object, ValueType::Ref(layout)),
                    (loaded_length, length_type),
                    (loaded_capacity, ValueType::Size),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: capacity, value: 2 },
                        Operation::Alloc {
                            destination: object,
                            layout,
                            extent: Extent::Counted {
                                header_bytes: 8,
                                count,
                                capacity,
                                stride_bytes: 4,
                            },
                            class: AllocationClass::ReferenceCounted,
                        },
                        header(HeaderField::Length, loaded_length, length_type),
                        header(HeaderField::Capacity, loaded_capacity, ValueType::Size),
                        Operation::Reclaim { object },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn counted_header_projections_load_typed_scalars() {
        assert!(check_initial(header_load_module(ValueType::Size)).is_ok());
    }

    #[test]
    fn header_projection_cannot_claim_non_scalar_type() {
        assert!(matches!(
            check_initial(header_load_module(ValueType::Bool)),
            Err(InitialCheckError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn bounds_guard_without_a_comparison_is_refused() {
        let condition = ValueId::new(1);
        let module = lowered(
            [(condition, ValueType::Bool)].into_iter().collect(),
            vec![
                Operation::ConstBool { destination: condition, value: true },
                Operation::Guard { condition, kind: GuardKind::Bounds },
            ],
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidGuard {
                function: FUNCTION,
                condition,
                kind: GuardKind::Bounds,
            }
        );
    }

    #[test]
    fn shared_ownership_cannot_be_reclaimed_as_unique() {
        let count = ValueId::new(0);
        let capacity = ValueId::new(1);
        let object = ValueId::new(2);
        let shared = ValueId::new(3);
        let (layouts, layout) = dynamic_u32_layout();
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
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
                    (object, ValueType::Ref(layout)),
                    (shared, ValueType::Ref(layout)),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: ENTRY,
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 0 },
                        Operation::ConstSize { destination: capacity, value: 1 },
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
                        Operation::Share { destination: shared, source: object },
                        Operation::Reclaim { object: shared },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::InvalidOwnership {
                function: FUNCTION,
                object: shared,
            }
        );
    }

    #[test]
    fn call_borrow_after_reclaim_is_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.extend([
            Operation::Reclaim { object },
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Borrow(super::super::CallBorrow::new(
                    object,
                    super::super::BorrowMode::Read,
                ))],
            },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::CallBorrow {
                    mode: super::super::BorrowMode::Read,
                },
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: None,
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object,
            }
        );
    }

    #[test]
    fn call_move_consumes_unique_and_later_use_is_rejected() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.extend([
            Operation::Call {
                destinations: Vec::new(),
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Move(object)],
            },
            Operation::Reclaim { object },
        ]);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Unique,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Unique),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object,
            }
        );
    }

    #[test]
    fn call_share_and_memory_value_are_fail_closed() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.push(Operation::Call {
            destinations: Vec::new(),
            callee: CALLEE,
            arguments: vec![super::super::CallArgument::Share(object)],
        });
        let share_module = call_module(
            layouts.clone(),
            values.clone(),
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(share_module).unwrap_err(),
            InitialCheckError::UnsupportedMemoryOperation {
                function: FUNCTION,
                operation: "Call(Share)",
            }
        );

        let (values, mut operations) = allocated_list(layout);
        operations.push(Operation::Call {
            destinations: Vec::new(),
            callee: CALLEE,
            arguments: vec![super::super::CallArgument::Value(object)],
        });
        let value_module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Value,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: None,
            }],
            Vec::new(),
        );
        assert_eq!(
            check_initial(value_module).unwrap_err(),
            InitialCheckError::UnsupportedMemoryOperation {
                function: FUNCTION,
                operation: "Call(Value memory)",
            }
        );
    }

    #[test]
    fn nonescaping_read_borrow_can_read_a_parameter_header() {
        let (layouts, layout) = packed_u32_layout();
        let object = ValueId::new(2);
        let destination = ValueId::new(12);
        let (mut values, mut operations) = allocated_list_with_header(layout, 8);
        values.insert(destination, ValueType::Size);
        operations.push(Operation::Call {
            destinations: vec![destination],
            callee: CALLEE,
            arguments: vec![super::super::CallArgument::Borrow(super::super::CallBorrow::new(
                object,
                super::super::BorrowMode::Read,
            ))],
        });
        let parameter = ValueId::new(10);
        let length = ValueId::new(11);
        let module = call_module(
            layouts,
            values,
            operations,
            Terminator::Return { values: Vec::new() },
            vec![Operation::Load {
                destination: length,
                place: Place::projected(
                    parameter,
                    Projection::Header(HeaderField::Length),
                    ValueType::Size,
                    AccessMode::Read,
                ),
                transfer: LoadTransfer::CopyScalar,
            }],
            [(parameter, ValueType::Ref(layout)), (length, ValueType::Size)]
                .into_iter()
                .collect(),
            Terminator::Return { values: vec![length] },
            call_contract(
                layout,
                ParameterOwnership::CallBorrow {
                    mode: super::super::BorrowMode::Read,
                },
                vec![ResultContract::Value(ValueType::Size)],
                [(0, vec![MemoryEffect::Read])].into_iter().collect(),
            ),
            vec![ValueType::Size],
            vec![BlockParameter {
                value: parameter,
                value_type: ValueType::Ref(layout),
                ownership: None,
            }],
            Vec::new(),
        );
        let result = check_initial(module);
        assert_eq!(result.as_ref().map(|_| ()), Ok(()), "{result:?}");
    }

    #[test]
    fn tail_call_borrow_and_share_use_memory_judgment() {
        let (layouts, layout) = dynamic_u32_layout();
        let (values, mut operations) = allocated_list(layout);
        let object = ValueId::new(2);
        operations.push(Operation::Reclaim { object });
        let borrow_module = call_module(
            layouts.clone(),
            values.clone(),
            operations,
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Borrow(super::super::CallBorrow::new(
                    object,
                    super::super::BorrowMode::Read,
                ))],
            },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::CallBorrow {
                    mode: super::super::BorrowMode::Read,
                },
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: None,
            }],
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
            }],
        );
        assert_eq!(
            check_initial(borrow_module).unwrap_err(),
            InitialCheckError::UseAfterConsume {
                function: FUNCTION,
                object,
            }
        );

        let (values, operations) = allocated_list(layout);
        let share_module = call_module(
            layouts,
            values,
            operations,
            Terminator::TailCall {
                site: TAIL_SITE,
                callee: CALLEE,
                arguments: vec![super::super::CallArgument::Share(object)],
            },
            Vec::new(),
            [(ValueId::new(10), ValueType::Ref(layout))]
                .into_iter()
                .collect(),
            Terminator::Return { values: Vec::new() },
            call_contract(
                layout,
                ParameterOwnership::Shared,
                Vec::new(),
                BTreeMap::new(),
            ),
            Vec::new(),
            vec![BlockParameter {
                value: ValueId::new(10),
                value_type: ValueType::Ref(layout),
                ownership: Some(super::super::Ownership::Shared),
            }],
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
            }],
        );
        assert_eq!(
            check_initial(share_module).unwrap_err(),
            InitialCheckError::UnsupportedMemoryOperation {
                function: FUNCTION,
                operation: "Call(Share)",
            }
        );
    }
}
