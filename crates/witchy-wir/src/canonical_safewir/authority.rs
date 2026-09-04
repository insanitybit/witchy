use std::fmt;

use sha2::{Digest, Sha256};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CandidateId([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedSourceWitness {
    candidate: CandidateId,
    checked_source: CheckedSourceIdentity,
    checked_lowering_digest: ModuleDigest,
}

/// Evidence returned only by the independent final SafeWIR checker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalSafeWirWitness {
    candidate: CandidateId,
    final_module_digest: ModuleDigest,
    final_revision: Revision,
}

/// Evidence returned only after canonical SafeWIR-to-WasmIR correspondence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityWirWitness {
    candidate: CandidateId,
    final_module_digest: ModuleDigest,
    correspondence_digest: [u8; 32],
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
    candidate: CandidateId,
    checked_source_witness: CheckedSourceWitness,
    final_safety_witness: FinalSafeWirWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionWitnessMismatch {
    Candidate,
    CheckedSource,
    FinalSafety,
    ZeroPassRevision,
    AuthorityWir,
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

    pub(crate) fn final_module_digest(&self) -> ModuleDigest {
        module_digest(&self.module)
    }

    pub(crate) fn history_digest(&self) -> ModuleDigest {
        self.optimization_history.identity_digest()
    }

    pub(crate) fn mint_authority_wir(
        &self,
        correspondence_digest: [u8; 32],
    ) -> AuthorityWirWitness {
        AuthorityWirWitness::from_correspondence(
            self.candidate,
            module_digest(&self.module),
            correspondence_digest,
        )
    }

    pub(crate) fn validate_pre_admission(
        &self,
    ) -> Result<(), AdmissionWitnessMismatch> {
        let expected_candidate = CandidateId::derive(&self.module, &self.optimization_history);
        if self.candidate != expected_candidate {
            return Err(AdmissionWitnessMismatch::Candidate);
        }
        if self.checked_source_witness.candidate != self.candidate
            || self.checked_source_witness.checked_source != self.module.checked_source()
            || self.checked_source_witness.checked_lowering_digest
                != self.checked_lowering.identity_digest()
            || self.checked_lowering.checked_source != self.module.checked_source()
        {
            return Err(AdmissionWitnessMismatch::CheckedSource);
        }
        if self.final_safety_witness.candidate != self.candidate
            || self.final_safety_witness.final_module_digest != module_digest(&self.module)
            || self.final_safety_witness.final_revision != self.module.revision()
        {
            return Err(AdmissionWitnessMismatch::FinalSafety);
        }
        if self.optimization_history.is_zero_pass()
            && self.checked_lowering.revision != self.module.revision()
        {
            return Err(AdmissionWitnessMismatch::ZeroPassRevision);
        }
        Ok(())
    }

    pub(crate) fn validate_authority_wir(
        &self,
        witness: &AuthorityWirWitness,
        correspondence_digest: [u8; 32],
    ) -> Result<(), AdmissionWitnessMismatch> {
        if witness.candidate != self.candidate
            || witness.final_module_digest != module_digest(&self.module)
            || witness.correspondence_digest != correspondence_digest
        {
            return Err(AdmissionWitnessMismatch::AuthorityWir);
        }
        Ok(())
    }
}

impl CheckedLoweringWitness {
    fn identity_digest(&self) -> ModuleDigest {
        witness_digest(
            b"witchy/checked-lowering-witness/v1",
            &[
                &self.checked_source.digest(),
                &self.revision.get().to_le_bytes(),
                &self.module_digest.bytes(),
                &self.proper_tail_manifest_digest.bytes(),
            ],
        )
    }
}

impl CandidateId {
    fn derive(
        module: &CanonicalModule<OptimizedValid>,
        history: &OptimizationHistoryWitness,
    ) -> Self {
        Self(
            witness_digest(
                b"witchy-safewir-candidate-v1",
                &[
                    &module.checked_source().digest(),
                    &module_digest(module).bytes(),
                    &history.identity_digest().bytes(),
                ],
            )
            .bytes(),
        )
    }
}

impl AuthorityWirWitness {
    pub(crate) fn from_correspondence(
        candidate: CandidateId,
        final_module_digest: ModuleDigest,
        correspondence_digest: [u8; 32],
    ) -> Self {
        Self { candidate, final_module_digest, correspondence_digest }
    }
}

fn witness_digest(domain: &[u8], parts: &[&[u8]]) -> ModuleDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"witchy/canonical-safewir/witness\0");
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    ModuleDigest::from_bytes(hasher.finalize().into())
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
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<CheckedInitialSafeWir, InitialCheckError> {
    let source_digest = source.source_witness().source_bundle_digest();
    let expected_source = source_digest;
    let module = super::check::check_initial(candidate)?;
    if module.checked_source().digest() != expected_source {
        return Err(InitialCheckError::CheckedSourceWitnessMismatch {
            expected: expected_source,
            actual: module.checked_source().digest(),
        });
    }
    let expected = ModuleDigest::from_bytes(
        source.source_witness().proper_tail_manifest_digest(),
    );
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
    let candidate = CandidateId::derive(&module, &optimization_history);
    super::check::recheck_final(&module).map_err(FinalValidationError::Safety)?;
    let final_safety_witness = FinalSafeWirWitness {
        candidate,
        final_module_digest: module_digest(&module),
        final_revision: module.revision(),
    };
    optimization_history
        .validate(initial, &module)
        .map_err(FinalValidationError::History)?;

    let checked_source_witness = CheckedSourceWitness {
        candidate,
        checked_source: initial.checked_source(),
        checked_lowering_digest: checked_lowering.identity_digest(),
    };

    Ok(ValidatedSafeWir {
        module,
        checked_lowering,
        optimization_history,
        candidate,
        checked_source_witness,
        final_safety_witness,
    })
}

#[cfg(test)]
pub(crate) fn check_initial_with_test_witness(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CheckedInitialSafeWir, InitialCheckError> {
    let expected = tail_digest(&candidate).bytes();
    check_initial_with_expected_digest(candidate, expected)
}

#[cfg(test)]
fn check_initial_with_expected_digest(
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
            check_initial_with_expected_digest(candidate, [0xff; 32]),
            Err(InitialCheckError::TailManifestDigestMismatch { .. })
        ));
    }

    #[test]
    fn tail_manifest_digest_is_bound_into_zero_pass_history() {
        let candidate = candidate();
        let expected = tail_digest(&candidate).bytes();
        let checked = check_initial_with_expected_digest(candidate, expected).unwrap();
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
