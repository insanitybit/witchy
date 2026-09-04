mod constant_fold;

use super::assurance::{self, AssuranceError};
use super::history::{HistoryError, OptimizationHistoryWitness};
use super::refinement::{self, RefinementError};
use super::{
    CanonicalModule, DischargedObligation, InitialCheckError, InitialValid,
    OptimizedValid,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationError {
    RevisionOverflow,
    Refinement(RefinementError),
    Safety(InitialCheckError),
    Assurance(AssuranceError),
    History(HistoryError),
}

fn map_assurance_error(error: AssuranceError) -> OptimizationError {
    match error {
        AssuranceError::Safety(error) => OptimizationError::Safety(error),
        error => OptimizationError::Assurance(error),
    }
}

pub struct OptimizationOutcome {
    optimized: CanonicalModule<OptimizedValid>,
    history: OptimizationHistoryWitness,
}

impl OptimizationOutcome {
    pub fn optimized(&self) -> &CanonicalModule<OptimizedValid> {
        &self.optimized
    }

    pub fn into_optimized(self) -> CanonicalModule<OptimizedValid> {
        self.optimized
    }

    pub fn history(&self) -> &OptimizationHistoryWitness {
        &self.history
    }

    pub(crate) fn into_parts(
        self,
    ) -> (CanonicalModule<OptimizedValid>, OptimizationHistoryWitness) {
        (self.optimized, self.history)
    }
}

pub fn optimize_unreachable_blocks(
    initial: CanonicalModule<InitialValid>,
) -> Result<OptimizationOutcome, OptimizationError> {
    assurance::registered(
        crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
    )
    .map_err(map_assurance_error)?;
    let before = initial.clone();
    let (functions, changed) = refinement::prune_functions(&before);
    let revision = if changed {
        super::Revision::new(
            before
                .revision()
                .get()
                .checked_add(1)
                .ok_or(OptimizationError::RevisionOverflow)?,
        )
    } else {
        before.revision()
    };
    let candidate = initial.into_transformed_candidate(revision, functions);
    let receipt = refinement::prove_unreachable_pruning(&before, &candidate)
        .map_err(OptimizationError::Refinement)?;
    let (optimized, history) = match receipt {
        Some(receipt) => {
            let (optimized, receipt) = assurance::accept_transform(
                crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                &before,
                candidate,
                receipt,
            )
            .map_err(map_assurance_error)?;
            let history = OptimizationHistoryWitness::from_receipt(&before, &optimized, receipt)
                .map_err(OptimizationError::History)?;
            (optimized, history)
        }
        None => {
            let optimized = assurance::recheck(
                crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                candidate,
            )
            .map_err(map_assurance_error)?;
            let history = OptimizationHistoryWitness::zero_pass(&before, &optimized)
                .map_err(OptimizationError::History)?;
            (optimized, history)
        }
    };
    history
        .validate(&before, &optimized)
        .map_err(OptimizationError::History)?;
    Ok(OptimizationOutcome { optimized, history })
}

/// Fold independently executable scalar operations one at a time.
///
/// Each replacement is checked as its own transformed candidate, and each
/// accepted replacement extends the ordered refinement chain. The reference
/// evaluator deliberately knows nothing about analyses or metadata: those
/// inputs can guide a future candidate search, but cannot authorize a fold.
pub fn optimize_constant_folding(
    initial: CanonicalModule<InitialValid>,
) -> Result<OptimizationOutcome, OptimizationError> {
    assurance::registered(crate::canonical_safewir::OptimizationPassId::ConstantFolding)
        .map_err(map_assurance_error)?;
    let before = initial.clone();
    let mut functions = initial.functions().to_vec();
    let mut revision = initial.revision();
    let mut previous = initial
        .clone()
        .into_transformed_candidate(revision, functions.clone());
    let mut receipts = Vec::new();
    let mut optimized = None;

    while let Some((next_functions, folded)) = constant_fold::first_fold(&functions) {
        let next_revision = revision
            .get()
            .checked_add(1)
            .map(super::Revision::new)
            .ok_or(OptimizationError::RevisionOverflow)?;
        let candidate = initial
            .clone()
            .into_transformed_candidate(next_revision, next_functions.clone());
        let receipt = OptimizationHistoryWitness::constant_folding_receipt(
            &previous,
            &candidate,
            DischargedObligation::FoldedScalarOperation {
                function: folded.function,
                block: folded.block,
                operation_index: folded.operation_index,
            },
        );
        let candidate_for_next = candidate.clone();
        let (checked, receipt) = match assurance::accept_transform(
            crate::canonical_safewir::OptimizationPassId::ConstantFolding,
            &previous,
            candidate,
            receipt,
        ) {
            Ok(result) => result,
            Err(AssuranceError::Safety(_)) => break,
            Err(error) => return Err(map_assurance_error(error)),
        };
        receipts.push(receipt);
        optimized = Some(checked);
        previous = candidate_for_next;
        functions = next_functions;
        revision = next_revision;
    }

    let optimized = match optimized {
        Some(optimized) => optimized,
        None => {
            let candidate = initial.into_transformed_candidate(revision, functions);
            assurance::recheck(
                crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                candidate,
            )
            .map_err(map_assurance_error)?
        }
    };
    let history = OptimizationHistoryWitness::from_receipts(&before, &optimized, receipts)
        .map_err(OptimizationError::History)?;
    history
        .validate(&before, &optimized)
        .map_err(OptimizationError::History)?;
    Ok(OptimizationOutcome { optimized, history })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, CallableContract, CheckedSourceIdentity, Function, FunctionId,
        FunctionSignature, FunctionSignatureId, LoweredModuleParts, Operation, Revision,
        ResultContract, Terminator, ValueId, ValueType,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};

    const FUNCTION: FunctionId = FunctionId::new(0);
    const SIGNATURE: FunctionSignatureId = FunctionSignatureId::new(0);
    const ENTRY: crate::canonical_safewir::BlockId = crate::canonical_safewir::BlockId::new(0);

    fn fixture(with_dead: bool) -> CanonicalModule<InitialValid> {
        let condition = ValueId::new(0);
        let live = crate::canonical_safewir::BlockId::new(1);
        let dead = crate::canonical_safewir::BlockId::new(2);
        let mut blocks = vec![
            BasicBlock {
                id: ENTRY,
                parameters: Vec::new(),
                operations: vec![Operation::ConstBool { destination: condition, value: true }],
                terminator: Terminator::Branch { target: live, arguments: Vec::new() },
            },
            BasicBlock {
                id: live,
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: Terminator::Return { values: Vec::new() },
            },
        ];
        let mut values: BTreeMap<ValueId, ValueType> =
            [(condition, ValueType::Bool)].into_iter().collect();
        if with_dead {
            blocks.push(BasicBlock {
                id: dead,
                parameters: Vec::new(),
                operations: vec![Operation::ConstBool {
                    destination: ValueId::new(9),
                    value: false,
                }],
                terminator: Terminator::Return { values: Vec::new() },
            });
            values.insert(ValueId::new(9), ValueType::Bool);
        }
        let lowered = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(4),
            checked_source: CheckedSourceIdentity::from_digest([3; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: Vec::<ResultContract>::new(),
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types: values,
                blocks,
            }],
            proper_tail_sites: Vec::new(),
        });
        crate::canonical_safewir::check_initial(lowered).unwrap()
    }

    #[test]
    fn zero_pass_has_an_explicit_empty_witness() {
        let outcome = optimize_unreachable_blocks(fixture(false)).unwrap();
        assert!(outcome.history().is_zero_pass());
        assert!(outcome.history().ordered().is_empty());
    }

    #[test]
    fn real_pruning_rechecks_and_records_one_ordered_receipt() {
        let outcome = optimize_unreachable_blocks(fixture(true)).unwrap();
        assert_eq!(outcome.optimized().functions()[0].blocks.len(), 2);
        assert_eq!(outcome.history().ordered().len(), 1);
        assert_eq!(
            outcome.history().ordered()[0].pass(),
            crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning
        );
        assert!(crate::canonical_safewir::production_registry()
            .iter()
            .any(|record| record.pass() == outcome.history().ordered()[0].pass()));
    }

    #[test]
    fn forged_receipt_is_rejected_by_history_validation() {
        let before = fixture(true);
        let outcome = optimize_unreachable_blocks(before.clone()).unwrap();
        let mut forged = outcome.history().clone();
        forged.tamper_first_after_digest();
        assert!(forged.validate(&before, outcome.optimized()).is_err());
    }

    fn scalar_fixture(operations: Vec<Operation>, value_types: BTreeMap<ValueId, ValueType>) -> CanonicalModule<InitialValid> {
        let lowered = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(4),
            checked_source: CheckedSourceIdentity::from_digest([5; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: Vec::<ResultContract>::new(),
                    parameter_effects: BTreeMap::new(),
                },
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
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            }],
            proper_tail_sites: Vec::new(),
        });
        crate::canonical_safewir::check_initial(lowered).unwrap()
    }

    #[test]
    fn constant_folding_uses_reference_semantics_and_orders_receipts_per_instance() {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let sum = ValueId::new(2);
        let comparison = ValueId::new(3);
        let module = scalar_fixture(
            vec![
                Operation::ConstInt { destination: left, value: 40 },
                Operation::ConstInt { destination: right, value: 2 },
                Operation::Arithmetic {
                    destination: sum,
                    operator: super::super::ArithmeticOperator::Add,
                    left,
                    right,
                    overflow: super::super::OverflowBehavior::Wrap,
                },
                Operation::Compare {
                    destination: comparison,
                    operator: super::super::ComparisonOperator::Equal,
                    left: sum,
                    right,
                },
            ],
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (sum, ValueType::Int),
                (comparison, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
        );
        let outcome = optimize_constant_folding(module).unwrap();
        let operations = &outcome.optimized().functions()[0].blocks[0].operations;
        assert!(matches!(operations[2], Operation::ConstInt { value: 42, .. }));
        assert!(matches!(operations[3], Operation::ConstBool { value: false, .. }));
        assert_eq!(outcome.history().ordered().len(), 2);
        assert!(outcome
            .history()
            .ordered()
            .iter()
            .all(|receipt| {
                receipt.pass() == crate::canonical_safewir::OptimizationPassId::ConstantFolding
                    && crate::canonical_safewir::production_registry()
                        .iter()
                        .any(|record| record.pass() == receipt.pass())
            }));
    }

    #[test]
    fn constant_folding_preserves_wrapping_overflow() {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let result = ValueId::new(2);
        let module = scalar_fixture(
            vec![
                Operation::ConstInt { destination: left, value: i64::MAX },
                Operation::ConstInt { destination: right, value: 1 },
                Operation::Arithmetic {
                    destination: result,
                    operator: super::super::ArithmeticOperator::Add,
                    left,
                    right,
                    overflow: super::super::OverflowBehavior::Wrap,
                },
            ],
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (result, ValueType::Int),
            ]
            .into_iter()
            .collect(),
        );
        let outcome = optimize_constant_folding(module).unwrap();
        assert!(matches!(
            outcome.optimized().functions()[0].blocks[0].operations[2],
            Operation::ConstInt { value: i64::MIN, .. }
        ));
    }

    #[test]
    fn constant_folding_uses_u32_size_arithmetic_and_shift_counts() {
        let maximum = ValueId::new(0);
        let one = ValueId::new(1);
        let wrapped = ValueId::new(2);
        let shift_count = ValueId::new(3);
        let shifted = ValueId::new(4);
        let module = scalar_fixture(
            vec![
                Operation::ConstSize { destination: maximum, value: u64::from(u32::MAX) },
                Operation::ConstSize { destination: one, value: 1 },
                Operation::Arithmetic {
                    destination: wrapped,
                    operator: super::super::ArithmeticOperator::Add,
                    left: maximum,
                    right: one,
                    overflow: super::super::OverflowBehavior::Wrap,
                },
                Operation::ConstSize { destination: shift_count, value: 32 },
                Operation::Arithmetic {
                    destination: shifted,
                    operator: super::super::ArithmeticOperator::ShiftLeft,
                    left: one,
                    right: shift_count,
                    overflow: super::super::OverflowBehavior::Wrap,
                },
            ],
            [
                (maximum, ValueType::Size),
                (one, ValueType::Size),
                (wrapped, ValueType::Size),
                (shift_count, ValueType::Size),
                (shifted, ValueType::Size),
            ]
            .into_iter()
            .collect(),
        );
        let outcome = optimize_constant_folding(module).unwrap();
        let operations = &outcome.optimized().functions()[0].blocks[0].operations;
        assert!(matches!(operations[2], Operation::ConstSize { value: 0, .. }));
        assert!(matches!(operations[4], Operation::ConstSize { value: 1, .. }));
        assert_eq!(outcome.history().ordered().len(), 2);
    }

    #[test]
    fn constant_folding_refuses_traps_division_by_zero_and_narrowing() {
        let max = ValueId::new(0);
        let one = ValueId::new(1);
        let zero = ValueId::new(2);
        let trapped = ValueId::new(3);
        let divided = ValueId::new(4);
        let narrowed = ValueId::new(5);
        let module = scalar_fixture(
            vec![
                Operation::ConstInt { destination: max, value: i64::MAX },
                Operation::ConstInt { destination: one, value: 1 },
                Operation::ConstInt { destination: zero, value: 0 },
                Operation::Arithmetic {
                    destination: trapped,
                    operator: super::super::ArithmeticOperator::Add,
                    left: max,
                    right: one,
                    overflow: super::super::OverflowBehavior::Trap,
                },
                Operation::Arithmetic {
                    destination: divided,
                    operator: super::super::ArithmeticOperator::DivideSigned,
                    left: max,
                    right: zero,
                    overflow: super::super::OverflowBehavior::Wrap,
                },
                Operation::NarrowSize { destination: narrowed, value: max },
            ],
            [
                (max, ValueType::Int),
                (one, ValueType::Int),
                (zero, ValueType::Int),
                (trapped, ValueType::Int),
                (divided, ValueType::Int),
                (narrowed, ValueType::Size),
            ]
            .into_iter()
            .collect(),
        );
        let outcome = optimize_constant_folding(module).unwrap();
        let operations = &outcome.optimized().functions()[0].blocks[0].operations;
        assert!(matches!(operations[3], Operation::Arithmetic { .. }));
        assert!(matches!(operations[4], Operation::Arithmetic { .. }));
        assert!(matches!(operations[5], Operation::NarrowSize { .. }));
        assert!(outcome.history().is_zero_pass());
    }
}
