//! Assured canonical SafeWIR transformations.
//!
//! This module is not wired into production. Each entry point consumes a
//! validated graph, constructs an authority-free candidate, checks the narrow
//! before/after relation independently, and runs the full canonical checker.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::{
    CanonicalModule, CheckedInitialSafeWir, CheckedOptimizedSafeWir, ComparisonOperator, Function,
    GuardKind, InitialCheckError, InitialValid, Operation, OptimizationAuthorityError,
    OptimizationCandidate, Revision, ValueId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundsGuardEliminationError {
    RevisionOverflow { revision: Revision },
    Authority(OptimizationAuthorityError),
    RewriteRelation(RewriteRelationError),
    Safety(InitialCheckError),
}

impl fmt::Display for BoundsGuardEliminationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionOverflow { revision } => {
                write!(formatter, "cannot advance SafeWIR revision {revision:?}")
            }
            Self::Authority(error) => error.fmt(formatter),
            Self::RewriteRelation(error) => {
                write!(formatter, "bounds-guard rewrite relation failed: {error}")
            }
            Self::Safety(error) => write!(
                formatter,
                "bounds-guard candidate failed validation: {error}"
            ),
        }
    }
}

impl std::error::Error for BoundsGuardEliminationError {}

/// Apply the pass to checked initial authority and bind the exact revalidated
/// output to a private optimization witness for independent final validation.
pub fn eliminate_same_block_redundant_bounds_guards_checked(
    before: CheckedInitialSafeWir,
) -> Result<CheckedOptimizedSafeWir, BoundsGuardEliminationError> {
    let optimized = eliminate_same_block_redundant_bounds_guards(before.module().clone())?;
    super::authority::bind_checked_same_block_bounds_guard_optimization(before, optimized)
        .map_err(BoundsGuardEliminationError::Authority)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteRelationError {
    RevisionMismatch,
    ModuleMetadataChanged,
    FunctionShapeChanged,
    BlockShapeChanged,
    OperationChanged,
    GuardHasNoLocalComparison,
    GuardHasNoEarlierSurvivingMatch,
}

impl fmt::Display for RewriteRelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ComparisonKey {
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
}

/// Remove only a later bounds guard whose comparison has exactly the same
/// operator and SSA operands as an earlier surviving bounds guard in the same
/// basic block.
///
/// This is deliberately not part of the production pass registry yet.
pub fn eliminate_same_block_redundant_bounds_guards(
    before: CanonicalModule<InitialValid>,
) -> Result<CanonicalModule<InitialValid>, BoundsGuardEliminationError> {
    let mut candidate: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
    if rewrite(&mut candidate) {
        candidate.revision = before.revision.checked_next().ok_or(
            BoundsGuardEliminationError::RevisionOverflow {
                revision: before.revision,
            },
        )?;
    }
    check_rewrite_relation(&before, &candidate)
        .map_err(BoundsGuardEliminationError::RewriteRelation)?;
    super::check::check_optimization_candidate(candidate)
        .map_err(BoundsGuardEliminationError::Safety)
}

fn rewrite(candidate: &mut CanonicalModule<OptimizationCandidate>) -> bool {
    let mut changed = false;
    for function in &mut candidate.functions {
        for block in &mut function.blocks {
            let mut comparisons = BTreeMap::new();
            let mut surviving = BTreeSet::new();
            let before_len = block.operations.len();
            block.operations.retain(|operation| {
                match operation {
                    Operation::Compare {
                        destination,
                        operator,
                        left,
                        right,
                    } => {
                        comparisons.insert(
                            *destination,
                            ComparisonKey {
                                operator: *operator,
                                left: *left,
                                right: *right,
                            },
                        );
                    }
                    Operation::Guard {
                        condition,
                        kind: GuardKind::Bounds,
                    } => {
                        if let Some(key) = comparisons.get(condition).copied() {
                            if !surviving.insert(key) {
                                return false;
                            }
                        }
                    }
                    _ => {}
                }
                true
            });
            changed |= block.operations.len() != before_len;
        }
    }
    changed
}

/// Independently recognize the one allowed rewrite. This intentionally does
/// not call `rewrite` or compare against a regenerated candidate.
pub(super) fn check_rewrite_relation(
    before: &CanonicalModule<InitialValid>,
    after: &CanonicalModule<OptimizationCandidate>,
) -> Result<(), RewriteRelationError> {
    if before.checked_source != after.checked_source
        || before.launch_contract_payload != after.launch_contract_payload
        || before.layouts != after.layouts
        || before.signatures != after.signatures
        || before.imports != after.imports
        || before.proper_tail_sites != after.proper_tail_sites
        || before.static_data_segments != after.static_data_segments
        || before.abi_catalog != after.abi_catalog
        || before.abi_bindings != after.abi_bindings
        || before.closure_bindings != after.closure_bindings
        || before.adapter_bindings != after.adapter_bindings
        || before.closure_layouts != after.closure_layouts
        || before.owner_families != after.owner_families
        || before.vm_services != after.vm_services
    {
        return Err(RewriteRelationError::ModuleMetadataChanged);
    }
    if before.functions.len() != after.functions.len() {
        return Err(RewriteRelationError::FunctionShapeChanged);
    }
    let mut changed = false;
    for (before, after) in before.functions.iter().zip(&after.functions) {
        changed |= check_function_relation(before, after)?;
    }
    let expected_revision = if changed {
        before
            .revision
            .checked_next()
            .ok_or(RewriteRelationError::RevisionMismatch)?
    } else {
        before.revision
    };
    if after.revision != expected_revision {
        return Err(RewriteRelationError::RevisionMismatch);
    }
    Ok(())
}

fn check_function_relation(
    before: &Function,
    after: &Function,
) -> Result<bool, RewriteRelationError> {
    if before.id != after.id
        || before.signature != after.signature
        || before.entry != after.entry
        || before.value_types != after.value_types
        || before.blocks.len() != after.blocks.len()
    {
        return Err(RewriteRelationError::FunctionShapeChanged);
    }
    let mut changed = false;
    for (before, after) in before.blocks.iter().zip(&after.blocks) {
        if before.id != after.id
            || before.parameters != after.parameters
            || before.terminator != after.terminator
        {
            return Err(RewriteRelationError::BlockShapeChanged);
        }

        let mut comparisons = BTreeMap::new();
        let mut surviving = BTreeSet::new();
        let mut after_index = 0;
        for operation in &before.operations {
            if let Operation::Compare {
                destination,
                operator,
                left,
                right,
            } = operation
            {
                comparisons.insert(*destination, (*operator, *left, *right));
            }

            if after.operations.get(after_index) == Some(operation) {
                if let Operation::Guard {
                    condition,
                    kind: GuardKind::Bounds,
                } = operation
                {
                    if let Some(key) = comparisons.get(condition).copied() {
                        surviving.insert(key);
                    }
                }
                after_index += 1;
                continue;
            }

            let Operation::Guard {
                condition,
                kind: GuardKind::Bounds,
            } = operation
            else {
                return Err(RewriteRelationError::OperationChanged);
            };
            let key = comparisons
                .get(condition)
                .copied()
                .ok_or(RewriteRelationError::GuardHasNoLocalComparison)?;
            if !surviving.contains(&key) {
                return Err(RewriteRelationError::GuardHasNoEarlierSurvivingMatch);
            }
        }
        if after_index != after.operations.len() {
            return Err(RewriteRelationError::OperationChanged);
        }
        changed |= before.operations.len() != after.operations.len();
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::layout::{LayoutBundle, LayoutInterner};

    use super::super::{
        BasicBlock, BlockId, CallableContract, CheckedSourceIdentity, FunctionId,
        FunctionSignature, FunctionSignatureId, LoweredCandidate, LoweredModuleParts,
        ResultContract, Revision, Terminator, ValueType,
    };
    use super::*;

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const LEFT: ValueId = ValueId::new(0);
    const RIGHT: ValueId = ValueId::new(1);
    const FIRST_CONDITION: ValueId = ValueId::new(2);
    const SECOND_CONDITION: ValueId = ValueId::new(3);
    const MARKER: ValueId = ValueId::new(4);

    fn block(id: u32, operations: Vec<Operation>, terminator: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId::new(id),
            parameters: Vec::new(),
            operations,
            terminator,
        }
    }

    fn candidate(blocks: Vec<BasicBlock>) -> CanonicalModule<LoweredCandidate> {
        let signature = FunctionSignature {
            id: SIGNATURE,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::<ResultContract>::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let value_types = [
            (LEFT, ValueType::Size),
            (RIGHT, ValueType::Size),
            (FIRST_CONDITION, ValueType::Bool),
            (SECOND_CONDITION, ValueType::Bool),
            (MARKER, ValueType::Int),
        ]
        .into_iter()
        .collect();
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(1),
            checked_source: CheckedSourceIdentity::from_digest([9; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).expect("layouts"),
            signatures: vec![signature],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: BlockId::new(0),
                value_types,
                blocks,
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    fn comparison(
        destination: ValueId,
        operator: ComparisonOperator,
        left: ValueId,
        right: ValueId,
    ) -> Operation {
        Operation::Compare {
            destination,
            operator,
            left,
            right,
        }
    }

    fn bounds(condition: ValueId) -> Operation {
        Operation::Guard {
            condition,
            kind: GuardKind::Bounds,
        }
    }

    fn validated(operations: Vec<Operation>) -> CanonicalModule<InitialValid> {
        super::super::check_initial(candidate(vec![block(
            0,
            operations,
            Terminator::Return { values: Vec::new() },
        )]))
        .expect("initial graph is valid")
    }

    fn duplicate_operations() -> Vec<Operation> {
        vec![
            Operation::ConstSize {
                destination: LEFT,
                value: 1,
            },
            Operation::ConstSize {
                destination: RIGHT,
                value: 2,
            },
            comparison(
                FIRST_CONDITION,
                ComparisonOperator::LessUnsigned,
                LEFT,
                RIGHT,
            ),
            bounds(FIRST_CONDITION),
            Operation::ConstInt {
                destination: MARKER,
                value: 7,
            },
            comparison(
                SECOND_CONDITION,
                ComparisonOperator::LessUnsigned,
                LEFT,
                RIGHT,
            ),
            bounds(SECOND_CONDITION),
        ]
    }

    #[test]
    fn removes_only_the_later_same_block_guard_and_revalidates() {
        let optimized =
            eliminate_same_block_redundant_bounds_guards(validated(duplicate_operations()))
                .expect("rewrite relation and full revalidation");
        assert_eq!(optimized.revision(), Revision::new(2));
        let operations = &optimized.functions()[0].blocks[0].operations;
        assert_eq!(operations.len(), 6);
        assert!(operations.contains(&bounds(FIRST_CONDITION)));
        assert!(!operations.contains(&bounds(SECOND_CONDITION)));
        assert!(operations.contains(&comparison(
            SECOND_CONDITION,
            ComparisonOperator::LessUnsigned,
            LEFT,
            RIGHT,
        )));
    }

    #[test]
    fn no_op_keeps_the_existing_revision() {
        let operations = vec![
            Operation::ConstSize {
                destination: LEFT,
                value: 1,
            },
            Operation::ConstSize {
                destination: RIGHT,
                value: 2,
            },
            comparison(
                FIRST_CONDITION,
                ComparisonOperator::LessUnsigned,
                LEFT,
                RIGHT,
            ),
            bounds(FIRST_CONDITION),
            comparison(
                SECOND_CONDITION,
                ComparisonOperator::LessEqualUnsigned,
                LEFT,
                RIGHT,
            ),
            Operation::ConstInt {
                destination: MARKER,
                value: 0,
            },
        ];
        let optimized = eliminate_same_block_redundant_bounds_guards(validated(operations))
            .expect("unchanged graph is revalidated");
        assert_eq!(optimized.revision(), Revision::new(1));
    }

    #[test]
    fn changed_graph_refuses_revision_overflow() {
        let mut lowered = candidate(vec![block(
            0,
            duplicate_operations(),
            Terminator::Return { values: Vec::new() },
        )]);
        lowered.revision = Revision::new(u64::MAX);
        let initial = super::super::check_initial(lowered).expect("initial graph is valid");
        assert_eq!(
            eliminate_same_block_redundant_bounds_guards(initial).unwrap_err(),
            BoundsGuardEliminationError::RevisionOverflow {
                revision: Revision::new(u64::MAX),
            },
        );
    }

    #[test]
    fn does_not_remove_different_operator_or_operands_or_guard_kind() {
        for later in [
            comparison(
                SECOND_CONDITION,
                ComparisonOperator::LessEqualUnsigned,
                LEFT,
                RIGHT,
            ),
            comparison(
                SECOND_CONDITION,
                ComparisonOperator::LessUnsigned,
                RIGHT,
                LEFT,
            ),
        ] {
            let mut operations = duplicate_operations();
            operations[5] = later;
            let optimized = eliminate_same_block_redundant_bounds_guards(validated(operations))
                .expect("non-matching guard remains valid");
            assert_eq!(optimized.functions()[0].blocks[0].operations.len(), 7);
        }

        let mut operations = duplicate_operations();
        operations[6] = Operation::Guard {
            condition: SECOND_CONDITION,
            kind: GuardKind::Arithmetic,
        };
        let optimized = eliminate_same_block_redundant_bounds_guards(validated(operations))
            .expect("non-bounds guard remains valid");
        assert_eq!(optimized.functions()[0].blocks[0].operations.len(), 7);
    }

    #[test]
    fn does_not_carry_guard_authority_across_blocks() {
        let initial = super::super::check_initial(candidate(vec![
            block(
                0,
                vec![
                    Operation::ConstSize {
                        destination: LEFT,
                        value: 1,
                    },
                    Operation::ConstSize {
                        destination: RIGHT,
                        value: 2,
                    },
                    comparison(
                        FIRST_CONDITION,
                        ComparisonOperator::LessUnsigned,
                        LEFT,
                        RIGHT,
                    ),
                    bounds(FIRST_CONDITION),
                    Operation::ConstInt {
                        destination: MARKER,
                        value: 0,
                    },
                ],
                Terminator::Branch {
                    target: BlockId::new(1),
                    arguments: Vec::new(),
                },
            ),
            block(
                1,
                vec![
                    comparison(
                        SECOND_CONDITION,
                        ComparisonOperator::LessUnsigned,
                        LEFT,
                        RIGHT,
                    ),
                    bounds(SECOND_CONDITION),
                ],
                Terminator::Return { values: Vec::new() },
            ),
        ]))
        .expect("initial graph is valid");
        let optimized = eliminate_same_block_redundant_bounds_guards(initial)
            .expect("cross-block guard remains valid");
        assert_eq!(optimized.functions()[0].blocks[1].operations.len(), 2);
    }

    #[test]
    fn relation_rejects_non_guard_changes_and_removing_the_first_guard() {
        let before = validated(duplicate_operations());

        let mut changed: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
        changed.functions[0].blocks[0].operations[0] = Operation::ConstSize {
            destination: LEFT,
            value: 99,
        };
        assert_eq!(
            check_rewrite_relation(&before, &changed),
            Err(RewriteRelationError::OperationChanged),
        );

        let mut removed_first: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
        removed_first.functions[0].blocks[0].operations.remove(3);
        assert_eq!(
            check_rewrite_relation(&before, &removed_first),
            Err(RewriteRelationError::GuardHasNoEarlierSurvivingMatch),
        );
    }

    #[test]
    fn relation_rejects_cross_block_and_non_identical_guard_deletions() {
        let before = validated(duplicate_operations());
        let mut different: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
        different.functions[0].blocks[0].operations[5] = comparison(
            SECOND_CONDITION,
            ComparisonOperator::LessEqualUnsigned,
            LEFT,
            RIGHT,
        );
        different.functions[0].blocks[0].operations.remove(6);
        assert_eq!(
            check_rewrite_relation(&before, &different),
            Err(RewriteRelationError::OperationChanged),
        );
    }
}
