pub(super) mod constant_fold;

use super::assurance::{self, AssuranceError};
use super::history::{HistoryError, OptimizationHistoryWitness};
use super::refinement::{self, RefinementError};
use super::{
    CanonicalModule, DischargedObligation, Function, InitialCheckError, InitialValid,
    LoweredModuleParts, OptimizedValid,
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
        AssuranceError::Refinement(error) => OptimizationError::Refinement(error),
        error => OptimizationError::Assurance(error),
    }
}

pub struct OptimizationOutcome {
    optimized: CanonicalModule<OptimizedValid>,
    history: OptimizationHistoryWitness,
}

#[derive(Clone)]
enum SessionRevision {
    Initial(CanonicalModule<InitialValid>),
    Optimized(CanonicalModule<OptimizedValid>),
}

impl SessionRevision {
    fn revision(&self) -> super::Revision {
        match self {
            Self::Initial(module) => module.revision(),
            Self::Optimized(module) => module.revision(),
        }
    }

    fn functions(&self) -> &[Function] {
        match self {
            Self::Initial(module) => module.functions(),
            Self::Optimized(module) => module.functions(),
        }
    }
}

/// An ordered, single-use optimization transaction.
///
/// The initial checked module is retained unchanged for the final history
/// witness. `current` is always the latest safety-checked revision. A pass
/// may only publish a revision after the registered refinement acceptance and
/// common SafeWIR recheck both succeed; a refused candidate is discarded.
pub struct OptimizationSession {
    initial: CanonicalModule<InitialValid>,
    current: SessionRevision,
    receipts: Vec<super::history::RefinementReceipt>,
}

impl OptimizationSession {
    pub fn new(initial: CanonicalModule<InitialValid>) -> Self {
        Self {
            current: SessionRevision::Initial(initial.clone()),
            initial,
            receipts: Vec::new(),
        }
    }

    pub fn current_revision(&self) -> super::Revision {
        self.current.revision()
    }

    fn candidate_for(
        &self,
        revision: super::Revision,
        functions: Vec<Function>,
    ) -> CanonicalModule<super::TransformedCandidate> {
        // The canonical passes currently transform function bodies only. The
        // immutable initial module supplies the unchanged module-level truth.
        self.initial
            .clone()
            .into_transformed_candidate(revision, functions)
    }

    fn initial_view(
        module: &CanonicalModule<OptimizedValid>,
    ) -> Result<CanonicalModule<InitialValid>, OptimizationError> {
        let catalog = module.abi_catalog().cloned();
        let candidate = CanonicalModule::from_lowering_with_static_data(LoweredModuleParts {
            revision: module.revision(),
            checked_source: module.checked_source(),
            layouts: module.layouts().clone(),
            signatures: module.signatures().to_vec(),
            imports: module.imports().to_vec(),
            functions: module.functions().to_vec(),
            proper_tail_sites: module.proper_tail_sites().to_vec(),
        }, module.static_data_segments().to_vec())
        .with_source_abi_bindings(module.source_abi_bindings().clone());
        let candidate = catalog.map_or(candidate.clone(), |catalog| candidate.with_abi_catalog(catalog));
        super::check_initial(candidate)
        .map_err(OptimizationError::Safety)
    }

    pub fn apply_unreachable_blocks(&mut self) -> Result<(), OptimizationError> {
        let before = match self.current.clone() {
            SessionRevision::Initial(module) => module,
            SessionRevision::Optimized(module) => Self::initial_view(&module)?,
        };
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
        let candidate = self.candidate_for(revision, functions);
        let receipt = refinement::prove_unreachable_pruning(&before, &candidate)
            .map_err(OptimizationError::Refinement)?;
        match receipt {
            Some(receipt) => {
                let (optimized, receipt) = assurance::accept_transform(
                    crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                    &before,
                    candidate,
                    receipt,
                )
                .map_err(map_assurance_error)?;
                self.current = SessionRevision::Optimized(optimized);
                self.receipts.push(receipt);
            }
            None => {
                let optimized = assurance::recheck(
                    crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                    candidate,
                )
                .map_err(map_assurance_error)?;
                self.current = SessionRevision::Optimized(optimized);
            }
        }
        Ok(())
    }

    /// Apply all independently executable constant folds to the current
    /// checked revision. A rejected fold leaves the last accepted revision in
    /// place and does not append a receipt.
    pub fn apply_constant_folding(&mut self) -> Result<(), OptimizationError> {
        assurance::registered(crate::canonical_safewir::OptimizationPassId::ConstantFolding)
            .map_err(map_assurance_error)?;
        let mut current = self.current.clone();
        let mut functions = current.functions().to_vec();
        let mut revision = current.revision();
        let mut accepted_any = false;

        while let Some((next_functions, folded)) = constant_fold::first_fold(&functions) {
            let next_revision = revision
                .get()
                .checked_add(1)
                .map(super::Revision::new)
                .ok_or(OptimizationError::RevisionOverflow)?;
            let candidate = self.candidate_for(next_revision, next_functions.clone());
            let obligation = DischargedObligation::FoldedScalarOperation {
                function: folded.function,
                block: folded.block,
                operation_index: folded.operation_index,
            };
            let receipt = match &current {
                SessionRevision::Initial(module) => {
                    OptimizationHistoryWitness::constant_folding_receipt(module, &candidate, obligation)
                }
                SessionRevision::Optimized(module) => {
                    OptimizationHistoryWitness::constant_folding_receipt(module, &candidate, obligation)
                }
            };
            let accepted_result = match &current {
                SessionRevision::Initial(module) => assurance::accept_transform(
                    crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                    module,
                    candidate,
                    receipt,
                ),
                SessionRevision::Optimized(module) => assurance::accept_transform(
                    crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                    module,
                    candidate,
                    receipt,
                ),
            };
            let accepted = match accepted_result {
                Ok(accepted) => accepted,
                Err(AssuranceError::Safety(_)) => break,
                Err(error) => return Err(map_assurance_error(error)),
            };
            self.receipts.push(accepted.1);
            current = SessionRevision::Optimized(accepted.0);
            functions = next_functions;
            revision = next_revision;
            accepted_any = true;
        }

        if !accepted_any {
            let candidate = self.candidate_for(revision, functions);
            let optimized = assurance::recheck(
                crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                candidate,
            )
            .map_err(map_assurance_error)?;
            current = SessionRevision::Optimized(optimized);
        }
        self.current = current;
        Ok(())
    }

    pub fn finalize(self) -> Result<OptimizationOutcome, OptimizationError> {
        let Self { initial, current, receipts } = self;
        let optimized = match current {
            SessionRevision::Optimized(module) => module,
            SessionRevision::Initial(_) => {
                let candidate = initial
                    .clone()
                    .into_transformed_candidate(initial.revision(), initial.functions().to_vec());
                assurance::recheck(
                    crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                    candidate,
                )
                .map_err(map_assurance_error)?
            }
        };
        let history = OptimizationHistoryWitness::from_receipts(&initial, &optimized, receipts)
            .map_err(OptimizationError::History)?;
        history
            .validate(&initial, &optimized)
            .map_err(OptimizationError::History)?;
        Ok(OptimizationOutcome { optimized, history })
    }
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
    let mut session = OptimizationSession::new(initial);
    session.apply_unreachable_blocks()?;
    session.finalize()
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
    let mut session = OptimizationSession::new(initial);
    session.apply_constant_folding()?;
    session.finalize()
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

    #[test]
    fn correctly_bound_fold_receipt_cannot_authorize_the_wrong_replacement() {
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let sum = ValueId::new(2);
        let before = scalar_fixture(
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
            ],
            [
                (left, ValueType::Int),
                (right, ValueType::Int),
                (sum, ValueType::Int),
            ]
            .into_iter()
            .collect(),
        );
        let mut functions = before.functions().to_vec();
        functions[0].blocks[0].operations[2] =
            Operation::ConstInt { destination: sum, value: 41 };
        let candidate = before.clone().into_transformed_candidate(
            Revision::new(before.revision().get() + 1),
            functions,
        );
        let receipt = OptimizationHistoryWitness::constant_folding_receipt(
            &before,
            &candidate,
            DischargedObligation::FoldedScalarOperation {
                function: FUNCTION,
                block: ENTRY,
                operation_index: 2,
            },
        );
        assert!(matches!(
            assurance::accept_transform(
                crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                &before,
                candidate,
                receipt,
            ),
            Err(AssuranceError::Refinement(RefinementError::FoldCandidateMismatch))
        ));
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

    fn composition_fixture() -> CanonicalModule<InitialValid> {
        let condition = ValueId::new(0);
        let left = ValueId::new(1);
        let right = ValueId::new(2);
        let sum = ValueId::new(3);
        let comparison = ValueId::new(4);
        let live = crate::canonical_safewir::BlockId::new(1);
        let dead = crate::canonical_safewir::BlockId::new(2);
        let mut value_types: BTreeMap<ValueId, ValueType> = [
            (condition, ValueType::Bool),
            (left, ValueType::Int),
            (right, ValueType::Int),
            (sum, ValueType::Int),
            (comparison, ValueType::Bool),
        ]
        .into_iter()
        .collect();
        value_types.insert(ValueId::new(9), ValueType::Bool);
        let lowered = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(4),
            checked_source: CheckedSourceIdentity::from_digest([6; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![FunctionSignature {
                id: SIGNATURE,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: Vec::new(),
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FUNCTION,
                signature: SIGNATURE,
                entry: ENTRY,
                value_types,
                blocks: vec![
                    BasicBlock {
                        id: ENTRY,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstBool { destination: condition, value: true }],
                        terminator: Terminator::Branch { target: live, arguments: Vec::new() },
                    },
                    BasicBlock {
                        id: live,
                        parameters: Vec::new(),
                        operations: vec![
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
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                    BasicBlock {
                        id: dead,
                        parameters: Vec::new(),
                        operations: vec![Operation::ConstBool {
                            destination: ValueId::new(9),
                            value: false,
                        }],
                        terminator: Terminator::Return { values: Vec::new() },
                    },
                ],
            }],
            proper_tail_sites: Vec::new(),
        });
        crate::canonical_safewir::check_initial(lowered).unwrap()
    }

    #[test]
    fn session_composes_pruning_then_folding_and_binds_the_ordered_chain() {
        let initial = composition_fixture();
        let mut session = OptimizationSession::new(initial.clone());
        session.apply_unreachable_blocks().unwrap();
        assert_eq!(session.current_revision(), Revision::new(5));
        session.apply_constant_folding().unwrap();
        let outcome = session.finalize().unwrap();

        assert_eq!(outcome.optimized().functions()[0].blocks.len(), 2);
        let operations = &outcome.optimized().functions()[0].blocks[1].operations;
        assert!(matches!(operations[2], Operation::ConstInt { value: 42, .. }));
        assert!(matches!(operations[3], Operation::ConstBool { value: false, .. }));
        assert_eq!(
            outcome.history().ordered().iter().map(|receipt| receipt.pass()).collect::<Vec<_>>(),
            vec![
                crate::canonical_safewir::OptimizationPassId::UnreachableBlockPruning,
                crate::canonical_safewir::OptimizationPassId::ConstantFolding,
                crate::canonical_safewir::OptimizationPassId::ConstantFolding,
            ]
        );

        let mut forged = outcome.history().clone();
        forged.tamper_first_after_digest();
        assert!(forged.validate(&initial, outcome.optimized()).is_err());
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
