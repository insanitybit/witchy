use super::history::{HistoryError, OptimizationHistoryWitness};
use super::refinement::{self, RefinementError};
use super::{check_transform, CanonicalModule, InitialCheckError, InitialValid, OptimizedValid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationError {
    RevisionOverflow,
    Refinement(RefinementError),
    Safety(InitialCheckError),
    History(HistoryError),
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
}

pub fn optimize_unreachable_blocks(
    initial: CanonicalModule<InitialValid>,
) -> Result<OptimizationOutcome, OptimizationError> {
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
    let optimized = check_transform(candidate).map_err(OptimizationError::Safety)?;
    let history = match receipt {
        Some(receipt) => OptimizationHistoryWitness::from_receipt(&before, &optimized, receipt)
            .map_err(OptimizationError::History)?,
        None => OptimizationHistoryWitness::zero_pass(&before, &optimized)
            .map_err(OptimizationError::History)?,
    };
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
    }

    #[test]
    fn forged_receipt_is_rejected_by_history_validation() {
        let before = fixture(true);
        let outcome = optimize_unreachable_blocks(before.clone()).unwrap();
        let mut forged = outcome.history().clone();
        forged.tamper_first_after_digest();
        assert!(forged.validate(&before, outcome.optimized()).is_err());
    }
}
