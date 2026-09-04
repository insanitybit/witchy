use std::fmt;

use super::history::{HistoryError, ModuleDigest, module_digest, tail_digest};
use super::{
    CanonicalModule, CheckedSourceIdentity, InitialCheckError, InitialValid, LoweredCandidate,
    OptimizationHistoryWitness, OptimizationOutcome, OptimizedValid, Revision,
};

/// Evidence minted by the initial checker for one exact lowered SafeWIR
/// revision. It is not encoder authority and cannot be constructed by lowering
/// or optimization code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedLoweringWitness {
    checked_source: CheckedSourceIdentity,
    revision: Revision,
    module_digest: ModuleDigest,
    proper_tail_manifest_digest: ModuleDigest,
}

/// The only result of initial validation that carries both the checked module
/// and its independently minted lowering witness.
pub struct CheckedInitialSafeWir {
    module: CanonicalModule<InitialValid>,
    witness: CheckedLoweringWitness,
}

impl CheckedInitialSafeWir {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    pub fn witness(&self) -> &CheckedLoweringWitness {
        &self.witness
    }

    pub fn into_parts(self) -> (CanonicalModule<InitialValid>, CheckedLoweringWitness) {
        (self.module, self.witness)
    }
}

/// Final canonical SafeWIR authority. Its fields are private; only the final
/// checker can mint it, and only the private backend may inspect its module.
pub struct ValidatedSafeWir {
    module: CanonicalModule<OptimizedValid>,
    checked_lowering: CheckedLoweringWitness,
    optimization_history: OptimizationHistoryWitness,
}

impl ValidatedSafeWir {
    pub(crate) fn module(&self) -> &CanonicalModule<OptimizedValid> {
        &self.module
    }

    pub fn checked_source(&self) -> CheckedSourceIdentity {
        self.module.checked_source()
    }

    pub fn revision(&self) -> Revision {
        self.module.revision()
    }

    pub fn optimization_history(&self) -> &OptimizationHistoryWitness {
        &self.optimization_history
    }

    pub fn checked_lowering(&self) -> &CheckedLoweringWitness {
        &self.checked_lowering
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalValidationError {
    InitialModuleDoesNotMatchWitness,
    InitialTailManifestDoesNotMatchWitness,
    OptimizedSourceDoesNotMatchInitial,
    Safety(InitialCheckError),
    History(HistoryError),
}

impl fmt::Display for FinalValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR final validation failed: {self:?}")
    }
}

impl std::error::Error for FinalValidationError {}

pub fn check_initial_with_witness(
    candidate: CanonicalModule<LoweredCandidate>,
    expected_proper_tail_manifest_digest: [u8; 32],
) -> Result<CheckedInitialSafeWir, InitialCheckError> {
    let expected = ModuleDigest::from_bytes(expected_proper_tail_manifest_digest);
    let module = super::check::check_initial(candidate)?;
    let actual = tail_digest(&module);
    if actual != expected {
        return Err(InitialCheckError::TailManifestDigestMismatch {
            expected: expected.bytes(),
            actual: actual.bytes(),
            actual_site_count: module.proper_tail_sites().len(),
        });
    }
    let witness = CheckedLoweringWitness {
        checked_source: module.checked_source(),
        revision: module.revision(),
        module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
    };
    Ok(CheckedInitialSafeWir { module, witness })
}

/// Recheck the exact final graph and verify both prior authority chains before
/// minting the only typestate accepted by the canonical backend.
pub fn validate_final(
    initial: &CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    optimized: OptimizationOutcome,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    if checked_lowering.checked_source != initial.checked_source()
        || checked_lowering.revision != initial.revision()
        || checked_lowering.module_digest != module_digest(initial)
    {
        return Err(FinalValidationError::InitialModuleDoesNotMatchWitness);
    }
    if checked_lowering.proper_tail_manifest_digest != tail_digest(initial) {
        return Err(FinalValidationError::InitialTailManifestDoesNotMatchWitness);
    }

    let (module, optimization_history) = optimized.into_parts();
    if module.checked_source() != initial.checked_source() {
        return Err(FinalValidationError::OptimizedSourceDoesNotMatchInitial);
    }
    super::check::recheck_final(&module).map_err(FinalValidationError::Safety)?;
    optimization_history
        .validate(initial, &module)
        .map_err(FinalValidationError::History)?;

    Ok(ValidatedSafeWir {
        module,
        checked_lowering,
        optimization_history,
    })
}

#[cfg(test)]
pub(crate) fn check_initial_with_test_witness(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CheckedInitialSafeWir, InitialCheckError> {
    let expected = tail_digest(&candidate).bytes();
    check_initial_with_witness(candidate, expected)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, BlockId, CallableContract, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation, ResultContract, Terminator, ValueId,
        ValueType, optimize_unreachable_blocks,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};

    fn candidate() -> CanonicalModule<LoweredCandidate> {
        let value = ValueId::new(0);
        CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(1),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![FunctionSignature {
                id: FunctionSignatureId::new(0),
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: vec![ResultContract::Value(ValueType::Int)],
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: FunctionId::new(0),
                signature: FunctionSignatureId::new(0),
                entry: BlockId::new(0),
                value_types: [(value, ValueType::Int)].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: BlockId::new(0),
                    parameters: Vec::new(),
                    operations: vec![Operation::ConstInt { destination: value, value: 3 }],
                    terminator: Terminator::Return { values: vec![value] },
                }],
            }],
            proper_tail_sites: Vec::new(),
        })
    }

    #[test]
    fn final_authority_requires_the_exact_initial_witness_and_history() {
        let checked = check_initial_with_test_witness(candidate()).unwrap();
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();
        let final_module = validate_final(&initial_snapshot, witness, optimized).unwrap();
        assert_eq!(final_module.checked_source(), initial_snapshot.checked_source());
        assert!(final_module.optimization_history().is_zero_pass());
    }

    #[test]
    fn witness_from_another_checked_module_is_rejected() {
        let checked = check_initial_with_test_witness(candidate()).unwrap();
        let initial_snapshot = checked.module().clone();
        let (initial, _) = checked.into_parts();
        let optimized = optimize_unreachable_blocks(initial).unwrap();

        let mut other = candidate();
        other.functions[0].blocks[0].operations[0] =
            Operation::ConstInt { destination: ValueId::new(0), value: 4 };
        let (_, wrong_witness) = check_initial_with_test_witness(other).unwrap().into_parts();
        assert!(matches!(
            validate_final(&initial_snapshot, wrong_witness, optimized),
            Err(FinalValidationError::InitialModuleDoesNotMatchWitness)
        ));
    }

    #[test]
    fn initial_checker_rejects_an_independent_tail_manifest_mismatch() {
        let candidate = candidate();
        assert!(matches!(
            check_initial_with_witness(candidate, [0xff; 32]),
            Err(InitialCheckError::TailManifestDigestMismatch { .. })
        ));
    }

    #[test]
    fn tail_manifest_digest_is_bound_into_zero_pass_history() {
        let candidate = candidate();
        let expected = tail_digest(&candidate).bytes();
        let checked = check_initial_with_witness(candidate, expected).unwrap();
        let initial_snapshot = checked.module().clone();
        let (initial, witness) = checked.into_parts();
        let optimized = super::super::optimize_unreachable_blocks(initial).unwrap();
        let final_module = validate_final(&initial_snapshot, witness, optimized).unwrap();
        assert_eq!(
            final_module.optimization_history().proper_tail_manifest_digest().bytes(),
            expected
        );
    }
}
