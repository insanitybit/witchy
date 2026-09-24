//! Assured canonical SafeWIR transformations.
//!
//! Production passes consume validated input, consult disposable analyses,
//! construct an authority-free candidate, and revalidate it. The driver keeps
//! the original checked input if a pass fails; final validation is still required.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::analysis::{AnalysisDb, BoundsGuardRedundancy, ProgramPoint};

use super::{
    CanonicalModule, CheckedInitialSafeWir, CheckedOptimizedSafeWir, CheckedSafeWirRevision,
    ComparisonOperator, Function, GuardKind, InitialCheckError, InitialValid, Operation,
    OptimizationAuthorityError, OptimizationCandidate, Revision, ValueId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundsGuardEliminationError {
    NoChange,
    RevisionOverflow { revision: Revision },
    Authority(OptimizationAuthorityError),
    RewriteRelation(RewriteRelationError),
    Safety(InitialCheckError),
}

impl fmt::Display for BoundsGuardEliminationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoChange => write!(formatter, "bounds-guard pass made no change"),
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
    let before = CheckedSafeWirRevision::from(before);
    attempt_bounds_guard_pass(&before)
}

fn attempt_bounds_guard_pass(
    before: &CheckedSafeWirRevision,
) -> Result<CheckedOptimizedSafeWir, BoundsGuardEliminationError> {
    let optimized = eliminate_same_block_redundant_bounds_guards(before.module().clone())?;
    if optimized.revision() == before.module().revision()
        && super::identity::module_digest(&optimized)
            == super::identity::module_digest(before.module())
    {
        return Err(BoundsGuardEliminationError::NoChange);
    }
    super::authority::bind_checked_same_block_bounds_guard_optimization(before, optimized)
        .map_err(BoundsGuardEliminationError::Authority)
}

/// Enabled transformations have explicit input/output and rejection behavior.
/// Adding a pass requires an exhaustive dispatch arm that checks its candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassId {
    SameBlockBoundsGuards,
}

pub enum CheckedPassOutcome {
    Applied(CheckedOptimizedSafeWir),
    Retained {
        current: CheckedSafeWirRevision,
        reason: CheckedPassError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedPassError {
    SameBlockBoundsGuards(BoundsGuardEliminationError),
}

impl CheckedPassOutcome {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        match self {
            Self::Applied(checked) => checked.module(),
            Self::Retained { current, .. } => current.module(),
        }
    }

    pub fn validate_final(
        self,
        source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    ) -> Result<super::ValidatedSafeWir, super::FinalValidationError> {
        match self {
            Self::Applied(checked) => super::validate_optimized_final(checked, source),
            Self::Retained { current, .. } => current.validate_final(source),
        }
    }
}

trait CheckedPassContract {
    fn apply(
        before: &CheckedSafeWirRevision,
    ) -> Result<CheckedOptimizedSafeWir, CheckedPassError>;
}

struct SameBlockBoundsGuardPass;

impl CheckedPassContract for SameBlockBoundsGuardPass {
    fn apply(
        before: &CheckedSafeWirRevision,
    ) -> Result<CheckedOptimizedSafeWir, CheckedPassError> {
        attempt_bounds_guard_pass(before).map_err(CheckedPassError::SameBlockBoundsGuards)
    }
}

pub fn run_checked_pass(
    before: impl Into<CheckedSafeWirRevision>,
    pass: PassId,
) -> CheckedPassOutcome {
    let before = before.into();
    let result = match pass {
        PassId::SameBlockBoundsGuards => SameBlockBoundsGuardPass::apply(&before),
    };
    match result {
        Ok(after) => CheckedPassOutcome::Applied(after),
        Err(reason) => CheckedPassOutcome::Retained { current: before, reason },
    }
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

/// Remove only a later bounds guard whose comparison has exactly the same
/// operator and SSA operands as an earlier surviving bounds guard in the same
/// basic block.
///
/// Production dispatch uses this same analysis and candidate checker.
pub fn eliminate_same_block_redundant_bounds_guards(
    before: CanonicalModule<InitialValid>,
) -> Result<CanonicalModule<InitialValid>, BoundsGuardEliminationError> {
    let mut candidate: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
    if rewrite(&mut candidate, &mut AnalysisDb::new(&before)) {
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

fn rewrite(
    candidate: &mut CanonicalModule<OptimizationCandidate>,
    analyses: &mut AnalysisDb<'_>,
) -> bool {
    let mut changed = false;
    for function in &mut candidate.functions {
        for block in &mut function.blocks {
            let before_len = block.operations.len();
            let mut operation_index = 0;
            block.operations.retain(|operation| {
                let point = ProgramPoint {
                    revision: analyses.revision(),
                    function: function.id,
                    block: block.id,
                    operation: operation_index,
                };
                operation_index += 1;
                if !matches!(operation, Operation::Guard { kind: GuardKind::Bounds, .. }) {
                    return true;
                }
                analyses.query::<BoundsGuardRedundancy>(point) != Some(&true)
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
        || before.proper_tail_locations != after.proper_tail_locations
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
        ProperTailLocation, ProperTailSite, ResultContract, Revision, TailSiteId, Terminator,
        ValueType,
    };
    use super::*;

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const LEFT: ValueId = ValueId::new(0);
    const RIGHT: ValueId = ValueId::new(1);
    const FIRST_CONDITION: ValueId = ValueId::new(2);
    const SECOND_CONDITION: ValueId = ValueId::new(3);
    const MARKER: ValueId = ValueId::new(4);
    const TAIL_SITE: TailSiteId = TailSiteId::from_digest([7; 32]);

    #[test]
    fn optional_pass_failure_preserves_initial_authority_and_final_validation() {
        let mut lowered = candidate(vec![block(0, duplicate_operations(), Terminator::Return { values: Vec::new() })]);
        lowered.revision = Revision::new(u64::MAX);
        let before = super::super::check_initial_with_test_witness(lowered).unwrap();
        let expected = before.module().clone();
        let outcome = run_checked_pass(before, PassId::SameBlockBoundsGuards);
        let CheckedPassOutcome::Retained { current, reason } = outcome else {
            panic!("an overflowing optional pass must retain its input");
        };
        assert!(matches!(
            reason,
            CheckedPassError::SameBlockBoundsGuards(
                BoundsGuardEliminationError::RevisionOverflow { .. }
            )
        ));
        assert_eq!(current.module().revision(), expected.revision());
        assert_eq!(
            super::super::identity::module_digest(current.module()),
            super::super::identity::module_digest(&expected)
        );
        let CheckedSafeWirRevision::Initial(original) = current else {
            panic!("initial authority changed kind on retention");
        };
        super::super::validate_final_for_test(original).expect("retained authority validates");
    }

    fn block(id: u32, operations: Vec<Operation>, terminator: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId::new(id),
            parameters: Vec::new(),
            operations,
            terminator,
        }
    }

    fn candidate(blocks: Vec<BasicBlock>) -> CanonicalModule<LoweredCandidate> {
        candidate_with_tail_sites(blocks, Vec::new())
    }

    fn candidate_with_tail_sites(
        blocks: Vec<BasicBlock>,
        proper_tail_sites: Vec<ProperTailSite>,
    ) -> CanonicalModule<LoweredCandidate> {
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
            proper_tail_sites,
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

    fn validated_with_tail_location() -> CanonicalModule<InitialValid> {
        let module = candidate_with_tail_sites(
            vec![block(
                0,
                duplicate_operations(),
                Terminator::TailCall {
                    site: TAIL_SITE,
                    callee: FUNCTION,
                    arguments: Vec::new(),
                },
            )],
            vec![ProperTailSite {
                id: TAIL_SITE,
                function: FUNCTION,
                discharged_by: None,
            }],
        )
        .with_proper_tail_locations(vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(0),
        )]);
        super::super::check_initial(module).expect("tail-call graph is valid")
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

    #[test]
    fn relation_rejects_dropped_or_relocated_proper_tail_locations() {
        let before = validated_with_tail_location();

        let mut dropped: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
        dropped.proper_tail_locations.clear();
        assert_eq!(
            check_rewrite_relation(&before, &dropped),
            Err(RewriteRelationError::ModuleMetadataChanged),
        );

        let mut relocated: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
        relocated.proper_tail_locations = vec![ProperTailLocation::new(
            TAIL_SITE,
            FUNCTION,
            BlockId::new(1),
        )];
        assert_eq!(
            check_rewrite_relation(&before, &relocated),
            Err(RewriteRelationError::ModuleMetadataChanged),
        );
    }
}
