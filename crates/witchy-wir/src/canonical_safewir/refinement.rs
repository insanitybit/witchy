use std::collections::{BTreeMap, BTreeSet};

use super::history::{DischargedObligation, OptimizationHistoryWitness, RefinementReceipt};
use super::{BasicBlock, CanonicalModule, Function, FunctionId, InitialValid, ModuleStage, Operation,
    Terminator, TransformedCandidate, ValueId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefinementError {
    ModuleShapeChanged,
    FunctionShapeChanged(FunctionId),
    MissingBlock { function: FunctionId, block: super::BlockId },
    RetainedBlockChanged { function: FunctionId, block: super::BlockId },
    ValueDefinitionsChanged(FunctionId),
    RevisionChangedWithoutPruning,
    RevisionDidNotAdvance,
    InvalidFoldReceipt,
    FoldLocationMissing { function: FunctionId, block: super::BlockId, operation_index: usize },
    FoldCandidateMismatch,
}

pub(crate) fn prune_functions(before: &CanonicalModule<InitialValid>) -> (Vec<Function>, bool) {
    let mut changed = false;
    let functions = before
        .functions()
        .iter()
        .map(|function| {
            let reachable = reachable_blocks(function).expect("initially checked CFG");
            if function.blocks.iter().all(|block| reachable.contains(&block.id)) {
                return function.clone();
            }
            changed = true;
            let blocks: Vec<_> = function
                .blocks
                .iter()
                .filter(|block| reachable.contains(&block.id))
                .cloned()
                .collect();
            Function {
                id: function.id,
                signature: function.signature,
                entry: function.entry,
                value_types: retained_value_types(function, &blocks),
                blocks,
            }
        })
        .collect();
    (functions, changed)
}

pub(crate) fn prove_unreachable_pruning<S: ModuleStage>(
    before: &CanonicalModule<S>,
    after: &CanonicalModule<TransformedCandidate>,
) -> Result<Option<RefinementReceipt>, RefinementError> {
    if before.checked_source() != after.checked_source()
        || before.layouts().canonical_bytes() != after.layouts().canonical_bytes()
        || before.signatures() != after.signatures()
        || before.imports() != after.imports()
        || before.static_data_segments() != after.static_data_segments()
        || before.proper_tail_sites() != after.proper_tail_sites()
        || before.functions().len() != after.functions().len()
    {
        return Err(RefinementError::ModuleShapeChanged);
    }

    let mut obligations = Vec::new();
    let mut any_pruned = false;
    for (before_function, after_function) in before.functions().iter().zip(after.functions()) {
        if before_function.id != after_function.id
            || before_function.signature != after_function.signature
            || before_function.entry != after_function.entry
        {
            return Err(RefinementError::FunctionShapeChanged(before_function.id));
        }
        let reachable = reachable_blocks(before_function)
            .map_err(|block| RefinementError::MissingBlock { function: before_function.id, block })?;
        let expected: Vec<_> = before_function
            .blocks
            .iter()
            .filter(|block| reachable.contains(&block.id))
            .collect();
        if expected.len() != after_function.blocks.len() {
            return Err(RefinementError::FunctionShapeChanged(before_function.id));
        }
        for (expected, actual) in expected.iter().zip(&after_function.blocks) {
            if *expected != actual {
                return Err(RefinementError::RetainedBlockChanged {
                    function: before_function.id,
                    block: actual.id,
                });
            }
        }
        if retained_value_types(before_function, &after_function.blocks)
            != after_function.value_types
        {
            return Err(RefinementError::ValueDefinitionsChanged(before_function.id));
        }
        let removed: Vec<_> = before_function
            .blocks
            .iter()
            .filter(|block| !reachable.contains(&block.id))
            .map(|block| block.id.get())
            .collect();
        if !removed.is_empty() {
            any_pruned = true;
            obligations.push(DischargedObligation::PrunedBlocks {
                function: before_function.id,
                blocks: removed,
            });
        }
    }

    if !any_pruned && before.revision() != after.revision() {
        return Err(RefinementError::RevisionChangedWithoutPruning);
    }
    if any_pruned
        && after.revision().get() != before.revision().get().saturating_add(1)
    {
        return Err(RefinementError::RevisionDidNotAdvance);
    }
    if !any_pruned {
        return Ok(None);
    }
    Ok(Some(OptimizationHistoryWitness::unreachable_receipt(
        before, after, obligations,
    )))
}

pub(crate) fn prove_constant_folding<S: ModuleStage>(
    before: &CanonicalModule<S>,
    after: &CanonicalModule<TransformedCandidate>,
    receipt: &RefinementReceipt,
) -> Result<(), RefinementError> {
    if before.checked_source() != after.checked_source()
        || before.layouts().canonical_bytes() != after.layouts().canonical_bytes()
        || before.signatures() != after.signatures()
        || before.imports() != after.imports()
        || before.static_data_segments() != after.static_data_segments()
        || before.proper_tail_sites() != after.proper_tail_sites()
        || after.revision().get() != before.revision().get().saturating_add(1)
    {
        return Err(RefinementError::ModuleShapeChanged);
    }
    let [DischargedObligation::FoldedScalarOperation {
        function,
        block,
        operation_index,
    }] = receipt.discharged_obligations()
    else {
        return Err(RefinementError::InvalidFoldReceipt);
    };
    let mut expected = before.functions().to_vec();
    let expected_function = expected
        .iter_mut()
        .find(|candidate| candidate.id == *function)
        .ok_or(RefinementError::FoldLocationMissing {
            function: *function,
            block: *block,
            operation_index: *operation_index,
        })?;
    let replacement = super::optimization::constant_fold::expected_fold(
        expected_function,
        *block,
        *operation_index,
    )
    .ok_or(RefinementError::FoldLocationMissing {
        function: *function,
        block: *block,
        operation_index: *operation_index,
    })?;
    let expected_block = expected_function
        .blocks
        .iter_mut()
        .find(|candidate| candidate.id == *block)
        .ok_or(RefinementError::FoldLocationMissing {
            function: *function,
            block: *block,
            operation_index: *operation_index,
        })?;
    let expected_operation = expected_block
        .operations
        .get_mut(*operation_index)
        .ok_or(RefinementError::FoldLocationMissing {
            function: *function,
            block: *block,
            operation_index: *operation_index,
        })?;
    *expected_operation = replacement;
    if expected != after.functions() {
        return Err(RefinementError::FoldCandidateMismatch);
    }
    Ok(())
}

fn reachable_blocks(function: &Function) -> Result<BTreeSet<super::BlockId>, super::BlockId> {
    let blocks: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, block)).collect();
    let mut reachable = BTreeSet::new();
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        let block = blocks.get(&block_id).ok_or(block_id)?;
        pending.extend(successors(&block.terminator));
    }
    Ok(reachable)
}

fn successors(terminator: &Terminator) -> Vec<super::BlockId> {
    match terminator {
        Terminator::Branch { target, .. } => vec![*target],
        Terminator::BranchIf { then_target, else_target, .. } => vec![*then_target, *else_target],
        Terminator::Return { .. }
        | Terminator::TailCall { .. }
        | Terminator::VarTailCall { .. }
        | Terminator::Trap
        | Terminator::Unreachable => Vec::new(),
    }
}

fn retained_value_types(
    function: &Function,
    blocks: &[BasicBlock],
) -> BTreeMap<ValueId, super::ValueType> {
    let definitions: BTreeSet<_> = blocks
        .iter()
        .flat_map(|block| {
            block
                .parameters
                .iter()
                .map(|parameter| parameter.value)
                .chain(block.operations.iter().flat_map(operation_destinations))
        })
        .collect();
    function
        .value_types
        .iter()
        .filter(|(value, _)| definitions.contains(value))
        .map(|(value, value_type)| (*value, *value_type))
        .collect()
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
        | Operation::Share { destination, .. }
        | Operation::FunctionValue { destination, .. } => vec![*destination],
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
