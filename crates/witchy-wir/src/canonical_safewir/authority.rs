use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};

use super::identity::{ModuleDigest, module_digest, tail_digest};
use super::{
    CanonicalModule, CheckedSourceIdentity, InitialCheckError, InitialValid, LoweredCandidate,
    ModuleStage, Revision,
};

mod candidate_abi;
use self::candidate_abi::*;
mod task_behavior;
mod source_abi;
pub(crate) use self::source_abi::*;
mod signature_relations;
use self::signature_relations::*;
mod layout_match;
pub(crate) use self::layout_match::*;
mod affine_abi;
use self::affine_abi::affine_abi_proofs;

/// Evidence minted by the initial checker for one exact lowered SafeWIR
/// revision. It is not encoder authority and cannot be constructed by lowering
/// or optimization code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedLoweringWitness {
    checked_source: CheckedSourceIdentity,
    revision: Revision,
    module_digest: ModuleDigest,
    proper_tail_manifest_digest: ModuleDigest,
    checked_source_witness: CheckedSourceWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactModuleWitness([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedSourceWitness {
    checked_source: CheckedSourceIdentity,
    initial_module_digest: ModuleDigest,
    proper_tail_manifest_digest: ModuleDigest,
    callable_abi_manifest_digest: [u8; 32],
    closure_witness_digest: [u8; 32],
    task_behavior_manifest_digest: [u8; 32],
    launch_contract_digest: [u8; 32],
    obligations_digest: [u8; 32],
}

/// Evidence returned only by the independent final SafeWIR checker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalSafeWirWitness {
    exact_module: ExactModuleWitness,
    final_module_digest: ModuleDigest,
    final_revision: Revision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OptimizationKind {
    SameBlockRedundantBoundsGuards,
}

impl OptimizationKind {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::SameBlockRedundantBoundsGuards => b"same-block-redundant-bounds-guards-v1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedOptimizationWitness {
    kind: OptimizationKind,
    checked_source: CheckedSourceIdentity,
    input_revision: Revision,
    input_module_digest: ModuleDigest,
    output_revision: Revision,
    output_module_digest: ModuleDigest,
    seal: ModuleDigest,
}

/// The only result of initial validation that carries both the checked module
/// and its independently minted lowering witness.
pub struct CheckedInitialSafeWir {
    module: CanonicalModule<InitialValid>,
    witness: CheckedLoweringWitness,
    affine_abi: Option<std::sync::Arc<super::check::AffineAbiProofs>>,
}

impl CheckedInitialSafeWir {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    pub fn witness(&self) -> &CheckedLoweringWitness {
        &self.witness
    }

}

/// A relation-checked and fully revalidated optimized module. Its fields and
/// witness are private; final validation must verify the complete lineage.
pub struct CheckedOptimizedSafeWir {
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    optimizations: Vec<CheckedOptimizationWitness>,
    affine_abi: Option<std::sync::Arc<super::check::AffineAbiProofs>>,
}

impl CheckedOptimizedSafeWir {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    #[cfg(feature = "raw-encoder-test-api")]
    fn into_parts_for_test(
        self,
    ) -> (
        CanonicalModule<InitialValid>,
        CheckedLoweringWitness,
        Vec<CheckedOptimizationWitness>,
    ) {
        (self.module, self.checked_lowering, self.optimizations)
    }
}

/// One independently checked SafeWIR revision that may enter another pass.
/// The variants preserve whether authority came directly from lowering or
/// from a digest-linked sequence of relation-checked transformations.
pub enum CheckedSafeWirRevision {
    Initial(CheckedInitialSafeWir),
    Optimized(CheckedOptimizedSafeWir),
}

impl CheckedSafeWirRevision {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        match self {
            Self::Initial(checked) => checked.module(),
            Self::Optimized(checked) => checked.module(),
        }
    }

    fn checked_lowering(&self) -> &CheckedLoweringWitness {
        match self {
            Self::Initial(checked) => &checked.witness,
            Self::Optimized(checked) => &checked.checked_lowering,
        }
    }

    /// The source-affine proofs the initial check ran under. A pass re-checks
    /// its candidate under them so unchanged functions replay their verdicts;
    /// final validation still derives its own proofs from the source.
    pub(super) fn affine_abi(&self) -> Option<&std::sync::Arc<super::check::AffineAbiProofs>> {
        match self {
            Self::Initial(checked) => checked.affine_abi.as_ref(),
            Self::Optimized(checked) => checked.affine_abi.as_ref(),
        }
    }

    fn optimizations(&self) -> &[CheckedOptimizationWitness] {
        match self {
            Self::Initial(_) => &[],
            Self::Optimized(checked) => &checked.optimizations,
        }
    }

    fn lineage_matches(&self) -> bool {
        match self {
            Self::Initial(checked) => {
                checked.witness.checked_source == checked.module.checked_source()
                    && checked.witness.revision == checked.module.revision()
                    && checked.witness.module_digest == module_digest(&checked.module)
            }
            Self::Optimized(checked) => optimization_chain_matches(
                &checked.module,
                &checked.checked_lowering,
                &checked.optimizations,
            ),
        }
    }

    pub fn validate_final(
        self,
        source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    ) -> Result<ValidatedSafeWir, FinalValidationError> {
        match self {
            Self::Initial(checked) => validate_final(checked, source),
            Self::Optimized(checked) => validate_optimized_final(checked, source),
        }
    }
}

impl From<CheckedInitialSafeWir> for CheckedSafeWirRevision {
    fn from(checked: CheckedInitialSafeWir) -> Self {
        Self::Initial(checked)
    }
}

impl From<CheckedOptimizedSafeWir> for CheckedSafeWirRevision {
    fn from(checked: CheckedOptimizedSafeWir) -> Self {
        Self::Optimized(checked)
    }
}

/// Final canonical SafeWIR authority. Its fields are private; only the final
/// checker can mint it, and only the private backend may inspect its module.
pub struct ValidatedSafeWir {
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    optimizations: Vec<CheckedOptimizationWitness>,
    exact_module: ExactModuleWitness,
    checked_source_witness: CheckedSourceWitness,
    final_safety_witness: FinalSafeWirWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionWitnessMismatch {
    ExactModule,
    CheckedSource,
    FinalSafety,
}

impl ValidatedSafeWir {
    pub(crate) fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    pub fn checked_source(&self) -> CheckedSourceIdentity {
        self.module.checked_source()
    }

    pub fn revision(&self) -> Revision {
        self.module.revision()
    }

    pub fn checked_lowering(&self) -> &CheckedLoweringWitness {
        &self.checked_lowering
    }

    pub(crate) fn final_module_digest(&self) -> ModuleDigest {
        module_digest(&self.module)
    }

    pub(crate) fn authority_digest(&self) -> ModuleDigest {
        let domain = if self.optimizations.is_empty() {
            b"witchy-safewir-zero-pass-authority-v1".as_slice()
        } else {
            b"witchy-safewir-optimized-authority-v1".as_slice()
        };
        witness_digest(domain, &[&self.exact_module.0])
    }

    pub(crate) fn validate_pre_admission(
        &self,
    ) -> Result<(), AdmissionWitnessMismatch> {
        let expected_exact_module = if self.optimizations.is_empty() {
            if self.checked_lowering.revision != self.module.revision()
                || self.checked_lowering.module_digest != module_digest(&self.module)
            {
                return Err(AdmissionWitnessMismatch::ExactModule);
            }
            ExactModuleWitness::derive_zero_pass(&self.module, &self.checked_lowering)
        } else {
                if !optimization_chain_matches(
                    &self.module,
                    &self.checked_lowering,
                    &self.optimizations,
                ) {
                    return Err(AdmissionWitnessMismatch::ExactModule);
                }
                ExactModuleWitness::derive_optimized(
                    &self.module,
                    &self.checked_lowering,
                    &self.optimizations,
                )
        };
        if self.exact_module != expected_exact_module {
            return Err(AdmissionWitnessMismatch::ExactModule);
        }
        if self.checked_source_witness != self.checked_lowering.checked_source_witness
            || self.checked_source_witness.checked_source != self.module.checked_source()
            || self.checked_source_witness.initial_module_digest
                != self.checked_lowering.module_digest
            || self.checked_source_witness.proper_tail_manifest_digest
                != self.checked_lowering.proper_tail_manifest_digest
            || self.checked_lowering.checked_source != self.module.checked_source()
        {
            return Err(AdmissionWitnessMismatch::CheckedSource);
        }
        if self.final_safety_witness.exact_module != self.exact_module
            || self.final_safety_witness.final_module_digest != module_digest(&self.module)
            || self.final_safety_witness.final_revision != self.module.revision()
        {
            return Err(AdmissionWitnessMismatch::FinalSafety);
        }
        Ok(())
    }

    pub(crate) fn validate_external_source_witness(
        &self,
        source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    ) -> Result<(), AdmissionWitnessMismatch> {
        let witness = source.source_witness();
        if witness.source_bundle_digest() != self.checked_source().digest()
            || witness.proper_tail_manifest_digest()
                != self.checked_lowering.proper_tail_manifest_digest.bytes()
            || witness.callable_abi_manifest_digest()
                != self.checked_source_witness.callable_abi_manifest_digest
            || witness.closure_witness_digest()
                != self.checked_source_witness.closure_witness_digest
            || witness.task_behavior_manifest_digest()
                != self.checked_source_witness.task_behavior_manifest_digest
            || witness.launch_contract_digest()
                != self.checked_source_witness.launch_contract_digest
            || witness.obligations_digest()
                != self.checked_source_witness.obligations_digest
            || self.module.launch_contract_payload() != source.launch_contract_payload()
        {
            return Err(AdmissionWitnessMismatch::CheckedSource);
        }
        Ok(())
    }
}

impl ExactModuleWitness {
    fn derive_zero_pass(
        module: &CanonicalModule<InitialValid>,
        checked_lowering: &CheckedLoweringWitness,
    ) -> Self {
        Self(
            witness_digest(
                b"witchy-safewir-zero-pass-exact-module-v1",
                &[
                    &module.checked_source().digest(),
                    &module_digest(module).bytes(),
                    &checked_lowering.module_digest.bytes(),
                    &checked_lowering.proper_tail_manifest_digest.bytes(),
                    &checked_lowering.checked_source_witness.callable_abi_manifest_digest,
                    &checked_lowering.checked_source_witness.closure_witness_digest,
                    &checked_lowering.checked_source_witness.task_behavior_manifest_digest,
                    &checked_lowering.checked_source_witness.obligations_digest,
                ],
            )
            .bytes(),
        )
    }

    fn derive_optimized(
        module: &CanonicalModule<InitialValid>,
        checked_lowering: &CheckedLoweringWitness,
        optimizations: &[CheckedOptimizationWitness],
    ) -> Self {
        let chain = optimization_chain_digest(optimizations);
        Self(
            witness_digest(
                b"witchy-safewir-optimized-exact-module-v1",
                &[
                    &module.checked_source().digest(),
                    &module_digest(module).bytes(),
                    &module.revision().get().to_le_bytes(),
                    &checked_lowering.module_digest.bytes(),
                    &chain.bytes(),
                ],
            )
            .bytes(),
        )
    }
}

impl CheckedOptimizationWitness {
    fn derive(
        kind: OptimizationKind,
        input: &CanonicalModule<InitialValid>,
        output: &CanonicalModule<InitialValid>,
        proper_tail_manifest_digest: ModuleDigest,
    ) -> Self {
        let checked_source = output.checked_source();
        let input_revision = input.revision();
        let input_module_digest = module_digest(input);
        let output_revision = output.revision();
        let output_module_digest = module_digest(output);
        let seal = witness_digest(
            b"witchy-safewir-checked-optimization-v1",
            &[
                kind.tag(),
                &checked_source.digest(),
                &input_revision.get().to_le_bytes(),
                &input_module_digest.bytes(),
                &output_revision.get().to_le_bytes(),
                &output_module_digest.bytes(),
                &proper_tail_manifest_digest.bytes(),
            ],
        );
        Self {
            kind,
            checked_source,
            input_revision,
            input_module_digest,
            output_revision,
            output_module_digest,
            seal,
        }
    }

    fn matches_link(
        &self,
        checked_source: CheckedSourceIdentity,
        input_revision: Revision,
        input_module_digest: ModuleDigest,
        proper_tail_manifest_digest: ModuleDigest,
    ) -> bool {
        if input_revision.checked_next() != Some(self.output_revision)
            || self.input_module_digest == self.output_module_digest
            || self.checked_source != checked_source
            || self.input_revision != input_revision
            || self.input_module_digest != input_module_digest
        {
            return false;
        }
        let seal = witness_digest(
            b"witchy-safewir-checked-optimization-v1",
            &[
                self.kind.tag(),
                &self.checked_source.digest(),
                &self.input_revision.get().to_le_bytes(),
                &self.input_module_digest.bytes(),
                &self.output_revision.get().to_le_bytes(),
                &self.output_module_digest.bytes(),
                &proper_tail_manifest_digest.bytes(),
            ],
        );
        self.seal == seal
    }
}

fn optimization_chain_matches(
    module: &CanonicalModule<InitialValid>,
    checked_lowering: &CheckedLoweringWitness,
    optimizations: &[CheckedOptimizationWitness],
) -> bool {
    if optimizations.is_empty()
        || checked_lowering.checked_source != module.checked_source()
    {
        return false;
    }
    let mut revision = checked_lowering.revision;
    let mut digest = checked_lowering.module_digest;
    for optimization in optimizations {
        if !optimization.matches_link(
            checked_lowering.checked_source,
            revision,
            digest,
            checked_lowering.proper_tail_manifest_digest,
        ) {
            return false;
        }
        revision = optimization.output_revision;
        digest = optimization.output_module_digest;
    }
    revision == module.revision() && digest == module_digest(module)
}

fn optimization_chain_digest(optimizations: &[CheckedOptimizationWitness]) -> ModuleDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"witchy/canonical-safewir/optimization-chain\0");
    hasher.update((optimizations.len() as u64).to_le_bytes());
    for optimization in optimizations {
        hasher.update(optimization.seal.bytes());
    }
    ModuleDigest::from_bytes(hasher.finalize().into())
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
    OptimizationDoesNotMatchWitness,
    SourceWitnessDoesNotMatchInitial,
    Safety(InitialCheckError),
    CandidateAbi(CandidateAbiBindingError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationAuthorityError {
    InitialAuthorityMismatch,
    CheckedSourceMismatch,
    TailManifestMismatch,
    NonProgressingRevision,
    RewriteRelation(super::optimize::RewriteRelationError),
}

impl fmt::Display for OptimizationAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR optimization authority failed: {self:?}")
    }
}

impl std::error::Error for OptimizationAuthorityError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateAbiBindingError {
    MissingBinding { source_declaration: u32 },
    UnexpectedBinding { source_declaration: u32 },
    DuplicateSourceDeclaration { source_declaration: u32 },
    DuplicateCandidateFunction { function: super::FunctionId },
    MissingCandidateFunction { function: super::FunctionId },
    MissingCandidateSignature { function: super::FunctionId },
    SourceAbiMismatch { source_declaration: u32 },
    MissingCheckedReferenceRelation { source_declaration: u32 },
    CandidateSignatureMismatch { function: super::FunctionId },
    MissingCanonicalLayouts,
    MissingClosureWitness { closure: u32 },
    CapturingClosureBinding { closure: u32 },
    ClosureSignatureMismatch { closure: u32 },
    MissingClosureEnvironmentParameter { function: super::FunctionId },
    AdapterShapeMismatch { adapter: super::FunctionId },
    TaskBehaviorMismatch { function: super::FunctionId },
    VmService(String),
    CompilerSession(String),
}

impl fmt::Display for CandidateAbiBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR candidate ABI binding failed: {self:?}")
    }
}

impl std::error::Error for CandidateAbiBindingError {}

#[derive(Debug)]
pub enum InitialWitnessError {
    Safety(InitialCheckError),
    CandidateAbi(CandidateAbiBindingError),
    LaunchContractMismatch,
    ExternalImportMismatch,
}

impl fmt::Display for InitialWitnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(error) => error.fmt(formatter),
            Self::CandidateAbi(error) => error.fmt(formatter),
            Self::LaunchContractMismatch => formatter.write_str("canonical SafeWIR launch contract does not match checked source"),
            Self::ExternalImportMismatch => formatter.write_str("canonical SafeWIR external imports do not match the authenticated checked-source environment"),
        }
    }
}

impl std::error::Error for InitialWitnessError {}

impl fmt::Display for FinalValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR final validation failed: {self:?}")
    }
}

impl std::error::Error for FinalValidationError {}

pub fn check_initial_with_witness(
    candidate: CanonicalModule<LoweredCandidate>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<CheckedInitialSafeWir, InitialWitnessError> {
    let source_digest = source.source_witness().source_bundle_digest();
    let expected_source = source_digest;
    validate_candidate_abi_bindings(&candidate, source)
        .map_err(InitialWitnessError::CandidateAbi)?;
    let affine_abi = affine_abi_proofs(&candidate, source)
        .map_err(InitialWitnessError::CandidateAbi)?;
    validate_external_imports(&candidate, source)?;
    if candidate.launch_contract_payload() != source.launch_contract_payload() {
        return Err(InitialWitnessError::LaunchContractMismatch);
    }
    let module = super::check::check_initial_with_affine_abi(candidate, &affine_abi)
        .map_err(InitialWitnessError::Safety)?;
    if module.checked_source().digest() != expected_source {
        return Err(InitialWitnessError::Safety(InitialCheckError::CheckedSourceWitnessMismatch {
            expected: expected_source,
            actual: module.checked_source().digest(),
        }));
    }
    let expected = ModuleDigest::from_bytes(
        source.source_witness().proper_tail_manifest_digest(),
    );
    let actual = tail_digest(&module);
    if actual != expected {
        return Err(InitialWitnessError::Safety(InitialCheckError::TailManifestDigestMismatch {
            expected: expected.bytes(),
            actual: actual.bytes(),
            actual_site_count: module.proper_tail_sites().len(),
        }));
    }
    let checked_source_witness = CheckedSourceWitness {
        checked_source: module.checked_source(),
        initial_module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        callable_abi_manifest_digest: source.callable_abi_manifest().digest(),
        closure_witness_digest: source.source_witness().closure_witness_digest(),
        task_behavior_manifest_digest: source.source_witness().task_behavior_manifest_digest(),
        launch_contract_digest: source.source_witness().launch_contract_digest(),
        obligations_digest: source
            .source_witness()
            .obligations_digest(),
    };
    let witness = CheckedLoweringWitness {
        checked_source: module.checked_source(),
        revision: module.revision(),
        module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        checked_source_witness,
    };
    Ok(CheckedInitialSafeWir { module, witness, affine_abi: Some(std::sync::Arc::new(affine_abi)) })
}

/// Consume the immutable initially-checked graph and recheck that exact graph
/// independently before minting the only typestate accepted by the backend.
/// There is intentionally no transform or history input in milestone one.
pub fn validate_final(
    checked: CheckedInitialSafeWir,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_final_inner(checked.module, checked.witness, Some(source))
}

pub(super) fn bind_checked_same_block_bounds_guard_optimization(
    checked: &CheckedSafeWirRevision,
    module: CanonicalModule<InitialValid>,
) -> Result<CheckedOptimizedSafeWir, OptimizationAuthorityError> {
    if !checked.lineage_matches() {
        return Err(OptimizationAuthorityError::InitialAuthorityMismatch);
    }
    if module.checked_source() != checked.module().checked_source() {
        return Err(OptimizationAuthorityError::CheckedSourceMismatch);
    }
    if tail_digest(&module) != checked.checked_lowering().proper_tail_manifest_digest {
        return Err(OptimizationAuthorityError::TailManifestMismatch);
    }
    if checked.module().revision().checked_next() != Some(module.revision())
        || module_digest(checked.module()) == module_digest(&module)
    {
        return Err(OptimizationAuthorityError::NonProgressingRevision);
    }
    let relation_candidate: CanonicalModule<super::OptimizationCandidate> =
        module.clone().into_stage();
    super::optimize::check_rewrite_relation(checked.module(), &relation_candidate)
        .map_err(OptimizationAuthorityError::RewriteRelation)?;
    let optimization = CheckedOptimizationWitness::derive(
        OptimizationKind::SameBlockRedundantBoundsGuards,
        checked.module(),
        &module,
        checked.checked_lowering().proper_tail_manifest_digest,
    );
    let mut optimizations = checked.optimizations().to_vec();
    optimizations.push(optimization);
    Ok(CheckedOptimizedSafeWir {
        module,
        checked_lowering: checked.checked_lowering().clone(),
        optimizations,
        affine_abi: checked.affine_abi().cloned(),
    })
}

pub fn validate_optimized_final(
    checked: CheckedOptimizedSafeWir,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_optimized_final_inner(checked, Some(source))
}

fn validate_final_inner(
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    source: Option<&witchy_canonical_source::source::CanonicalSourceView<'_>>,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    if checked_lowering.checked_source != module.checked_source()
        || checked_lowering.revision != module.revision()
        || checked_lowering.module_digest != module_digest(&module)
    {
        return Err(FinalValidationError::InitialModuleDoesNotMatchWitness);
    }
    if checked_lowering.proper_tail_manifest_digest != tail_digest(&module) {
        return Err(FinalValidationError::InitialTailManifestDoesNotMatchWitness);
    }
    let affine_abi = if let Some(source) = source {
        if module.checked_source().digest() != source.source_witness().source_bundle_digest()
            || checked_lowering.proper_tail_manifest_digest.bytes()
                != source.source_witness().proper_tail_manifest_digest()
            || checked_lowering.checked_source_witness.callable_abi_manifest_digest
                != source.callable_abi_manifest().digest()
            || checked_lowering.checked_source_witness.closure_witness_digest
                != source.source_witness().closure_witness_digest()
            || checked_lowering.checked_source_witness.task_behavior_manifest_digest
                != source.source_witness().task_behavior_manifest_digest()
            || checked_lowering.checked_source_witness.launch_contract_digest
                != source.source_witness().launch_contract_digest()
            || module.launch_contract_payload() != source.launch_contract_payload()
        {
            return Err(FinalValidationError::SourceWitnessDoesNotMatchInitial);
        }
        validate_candidate_abi_bindings(&module, source)
            .map_err(FinalValidationError::CandidateAbi)?;
        Some(affine_abi_proofs(&module, source).map_err(FinalValidationError::CandidateAbi)?)
    } else { None };
    match affine_abi.as_ref() {
        Some(proofs) => super::check::recheck_final_with_affine_abi(&module, proofs),
        None => super::check::recheck_final(&module),
    }.map_err(FinalValidationError::Safety)?;
    let exact_module = ExactModuleWitness::derive_zero_pass(&module, &checked_lowering);
    let final_safety_witness = FinalSafeWirWitness {
        exact_module,
        final_module_digest: module_digest(&module),
        final_revision: module.revision(),
    };

    let checked_source_witness = checked_lowering.checked_source_witness.clone();

    Ok(ValidatedSafeWir {
        module,
        checked_lowering,
        optimizations: Vec::new(),
        exact_module,
        checked_source_witness,
        final_safety_witness,
    })
}

fn validate_optimized_final_inner(
    checked: CheckedOptimizedSafeWir,
    source: Option<&witchy_canonical_source::source::CanonicalSourceView<'_>>,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    let CheckedOptimizedSafeWir {
        module,
        checked_lowering,
        optimizations,
        affine_abi: _,
    } = checked;
    if !optimization_chain_matches(&module, &checked_lowering, &optimizations) {
        return Err(FinalValidationError::OptimizationDoesNotMatchWitness);
    }
    if checked_lowering.proper_tail_manifest_digest != tail_digest(&module) {
        return Err(FinalValidationError::InitialTailManifestDoesNotMatchWitness);
    }
    let affine_abi = if let Some(source) = source {
        if module.checked_source().digest() != source.source_witness().source_bundle_digest()
            || checked_lowering.proper_tail_manifest_digest.bytes()
                != source.source_witness().proper_tail_manifest_digest()
            || checked_lowering.checked_source_witness.callable_abi_manifest_digest
                != source.callable_abi_manifest().digest()
            || checked_lowering.checked_source_witness.closure_witness_digest
                != source.source_witness().closure_witness_digest()
            || checked_lowering.checked_source_witness.task_behavior_manifest_digest
                != source.source_witness().task_behavior_manifest_digest()
            || checked_lowering.checked_source_witness.launch_contract_digest
                != source.source_witness().launch_contract_digest()
            || module.launch_contract_payload() != source.launch_contract_payload()
        {
            return Err(FinalValidationError::SourceWitnessDoesNotMatchInitial);
        }
        validate_candidate_abi_bindings(&module, source)
            .map_err(FinalValidationError::CandidateAbi)?;
        Some(affine_abi_proofs(&module, source).map_err(FinalValidationError::CandidateAbi)?)
    } else { None };
    match affine_abi.as_ref() {
        Some(proofs) => super::check::recheck_final_with_affine_abi(&module, proofs),
        None => super::check::recheck_final(&module),
    }.map_err(FinalValidationError::Safety)?;
    let exact_module =
        ExactModuleWitness::derive_optimized(&module, &checked_lowering, &optimizations);
    let final_safety_witness = FinalSafeWirWitness {
        exact_module,
        final_module_digest: module_digest(&module),
        final_revision: module.revision(),
    };
    let checked_source_witness = checked_lowering.checked_source_witness.clone();
    Ok(ValidatedSafeWir {
        module,
        checked_lowering,
        optimizations,
        exact_module,
        checked_source_witness,
        final_safety_witness,
    })
}

#[cfg(feature = "raw-encoder-test-api")]
pub(crate) fn validate_final_for_test(
    checked: CheckedInitialSafeWir,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_final_inner(checked.module, checked.witness, None)
}

#[cfg(feature = "raw-encoder-test-api")]
fn validate_optimized_final_parts_for_test(
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    optimizations: Vec<CheckedOptimizationWitness>,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_optimized_final_inner(
        CheckedOptimizedSafeWir {
            module,
            checked_lowering,
            optimizations,
            affine_abi: None,
        },
        None,
    )
}

#[cfg(feature = "raw-encoder-test-api")]
pub(crate) fn check_initial_with_test_witness(
    candidate: CanonicalModule<LoweredCandidate>,
) -> Result<CheckedInitialSafeWir, InitialCheckError> {
    let expected = tail_digest(&candidate).bytes();
    check_initial_with_expected_digest(candidate, expected)
}

#[cfg(feature = "raw-encoder-test-api")]
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
    let checked_source_witness = CheckedSourceWitness {
        checked_source: module.checked_source(),
        initial_module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        callable_abi_manifest_digest: [0; 32],
        closure_witness_digest: [0; 32],
        task_behavior_manifest_digest: [0; 32],
        obligations_digest: [0; 32],
        launch_contract_digest: [0; 32],
    };
    let witness = CheckedLoweringWitness {
        checked_source: module.checked_source(),
        revision: module.revision(),
        module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        checked_source_witness,
    };
    Ok(CheckedInitialSafeWir { module, witness, affine_abi: None })
}

// Authority cases that need a real checked source (RFC-0170 P4).
#[cfg(feature = "checked-source-test-api")]
#[doc(hidden)]
pub mod checked_source_cases;

/// The one-function `fn() -> Int` candidate the authority tests and the
/// checked-source cases perturb.
#[cfg(feature = "checked-source-test-api")]
fn authority_test_candidate() -> CanonicalModule<LoweredCandidate> {
    use super::{
        BasicBlock, BlockId, CallableContract, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation, ResultContract, Terminator, ValueId,
        ValueType,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};
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

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use std::collections::BTreeMap;

    use super::*;
    use super::authority_test_candidate as candidate;
    use crate::canonical_safewir::{
        BasicBlock, BlockId, CallableContract, ComparisonOperator, Function, FunctionId,
        FunctionSignature, FunctionSignatureId, GuardKind, LoweredModuleParts, Operation,
        ResultContract, Terminator, ValueId, ValueType,
    };
    use crate::layout::{LayoutBundle, LayoutInterner};

    // Cases that compile witchy source moved to `checked_source_cases.rs`;
    // `crates/witchy-lower/tests/safewir_checked_source.rs` runs them.

    pub fn physically_matching_plain_list_cannot_stand_in_for_retained_view_list() {
        use crate::layout::{PointerSlot, RcHeader, ScalarKind};
        use witchy_syntax::ast::{Type, TypeQual};

        let source = Type::Named("List".into(), vec![Type::Qualified(
            TypeQual::View, Box::new(Type::Named("String".into(), Vec::new())),
        )]);
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = layouts.intern_runtime_buffer(byte).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(string, PointerSlot::Owned).unwrap();
        let plain = layouts.intern_packed_list(slot, RcHeader::Required).unwrap();
        let records = BTreeMap::new();

        assert!(!layout_matches_source(plain, &source, &layouts, &records));
        let key = witchy_canonical_source::structural_identity::recursive_nominal_layout_key(&source).unwrap();
        let nominal = layouts.intern_opaque_nominal(key).unwrap();
        layouts.bind_nominal(nominal, plain).unwrap();
        assert!(layout_matches_source(nominal, &source, &layouts, &records));
    }

    fn optimizable_candidate() -> CanonicalModule<LoweredCandidate> {
        let mut module = candidate();
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let first_condition = ValueId::new(2);
        let second_condition = ValueId::new(3);
        module.signatures[0].results.clear();
        module.signatures[0].contract.results.clear();
        module.functions[0].value_types = [
            (left, ValueType::Size),
            (right, ValueType::Size),
            (first_condition, ValueType::Bool),
            (second_condition, ValueType::Bool),
        ]
        .into_iter()
        .collect();
        module.functions[0].blocks[0].operations = vec![
            Operation::ConstSize {
                destination: left,
                value: 3,
            },
            Operation::ConstSize {
                destination: right,
                value: 4,
            },
            Operation::Compare {
                destination: first_condition,
                operator: ComparisonOperator::LessUnsigned,
                left,
                right,
            },
            Operation::Guard {
                condition: first_condition,
                kind: GuardKind::Bounds,
            },
            Operation::Compare {
                destination: second_condition,
                operator: ComparisonOperator::LessUnsigned,
                left,
                right,
            },
            Operation::Guard {
                condition: second_condition,
                kind: GuardKind::Bounds,
            },
        ];
        module.functions[0].blocks[0].terminator = Terminator::Return { values: Vec::new() };
        module
    }

    /// One adapter over a target with an `Int` parameter, plus the closure
    /// layout its environment lane names. The adapter's body is supplied so a
    /// refusal test can perturb exactly it.
    fn adapter_candidate(
        operations: Vec<Operation>,
        terminator: Terminator,
    ) -> (CanonicalModule<LoweredCandidate>, FunctionId, FunctionId) {
        use crate::canonical_safewir::{
            BlockParameter, CandidateAdapterBinding, CandidateAdapterBindings,
            ParameterContract, ParameterOwnership,
        };
        use crate::layout::ScalarKind;

        let adapter_id = FunctionId::new(0);
        let target_id = FunctionId::new(1);
        let adapter_signature = FunctionSignatureId::new(0);
        let target_signature = FunctionSignatureId::new(1);
        let environment = ValueId::new(0);
        let argument = ValueId::new(1);
        let result = ValueId::new(2);

        let mut layouts = LayoutInterner::new();
        let callable = layouts.intern_scalar(ScalarKind::Callable).unwrap();
        let closure = layouts.intern_packed_record(vec![callable]).unwrap();
        let bundle = LayoutBundle::from_interner(&layouts, [closure]).unwrap();

        let target_contract = CallableContract {
            parameters: vec![ParameterContract {
                value_type: ValueType::Int,
                ownership: ParameterOwnership::Value,
            }],
            results: vec![ResultContract::Value(ValueType::Int)],
            parameter_effects: BTreeMap::new(),
        };
        let mut adapter_contract = target_contract.clone();
        adapter_contract.parameters.insert(0, ParameterContract {
            value_type: ValueType::Ref(closure),
            ownership: ParameterOwnership::Shared,
        });
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(1),
            checked_source: CheckedSourceIdentity::from_digest([7; 32]),
            layouts: bundle,
            signatures: vec![
                FunctionSignature {
                    id: adapter_signature,
                    parameters: vec![ValueType::Ref(closure), ValueType::Int],
                    results: vec![ValueType::Int],
                    contract: adapter_contract,
                },
                FunctionSignature {
                    id: target_signature,
                    parameters: vec![ValueType::Int],
                    results: vec![ValueType::Int],
                    contract: target_contract,
                },
            ],
            imports: Vec::new(),
            functions: vec![
                Function {
                    id: adapter_id,
                    signature: adapter_signature,
                    entry: BlockId::new(0),
                    value_types: [
                        (environment, ValueType::Ref(closure)),
                        (argument, ValueType::Int),
                        (result, ValueType::Int),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: BlockId::new(0),
                        parameters: vec![
                            BlockParameter { value: environment, value_type: ValueType::Ref(closure), ownership: None },
                            BlockParameter { value: argument, value_type: ValueType::Int, ownership: None },
                        ],
                        operations,
                        terminator,
                    }],
                },
                Function {
                    id: target_id,
                    signature: target_signature,
                    entry: BlockId::new(0),
                    value_types: [(argument, ValueType::Int)].into_iter().collect(),
                    blocks: vec![BasicBlock {
                        id: BlockId::new(0),
                        parameters: vec![BlockParameter { value: argument, value_type: ValueType::Int, ownership: None }],
                        operations: Vec::new(),
                        terminator: Terminator::Return { values: vec![argument] },
                    }],
                },
            ],
            proper_tail_sites: Vec::new(),
        })
        .with_closure_layouts([(closure, adapter_signature)].into_iter().collect())
        .with_adapter_bindings(CandidateAdapterBindings::new(vec![
            CandidateAdapterBinding::new(adapter_id, target_id),
        ]));
        (module, adapter_id, target_id)
    }

    fn forwarding_body() -> (Vec<Operation>, Terminator) {
        use crate::canonical_safewir::CallArgument;
        (
            vec![Operation::Call {
                destinations: vec![ValueId::new(2)],
                callee: FunctionId::new(1),
                arguments: vec![CallArgument::Value(ValueId::new(1))],
            }],
            Terminator::Return { values: vec![ValueId::new(2)] },
        )
    }

    fn adapter_shape_holds(module: &CanonicalModule<LoweredCandidate>, adapter: FunctionId, target: FunctionId) -> bool {
        let functions = module.functions().iter().map(|function| (function.id, function)).collect::<BTreeMap<_, _>>();
        let adapter_function = functions[&adapter];
        let Some(binding) = module.adapter_bindings().entries().iter().find(|binding| binding.adapter() == adapter) else { return false };
        if binding.target() != target { return false; }
        let signature = |id| module.signatures().iter().find(|signature| signature.id == id).expect("signature");
        let lanes = strip_closure_environment(signature(adapter_function.signature), module);
        let target_signature = signature(functions[&target].signature);
        let owner_family = module.owner_families().iter().find(|family| {
            family.signature == adapter_function.signature
                && adapter_function.value_types.get(&ValueId::new(0)) == Some(&ValueType::Gc(family.owner_type))
        });
        lanes.is_some_and(|lanes| signature_lanes_match(&lanes, target_signature))
            && owner_family.is_none_or(|family| {
                family.declaration_signature == target_signature.id
                    && family.constructors.iter().any(|constructor| {
                        constructor.function == adapter && constructor.witness.is_none()
                            && constructor.environment_tag == 0 && constructor.captures.is_empty()
                    })
            })
            && adapter_body_forwards(adapter_function, target, target_signature, owner_family.map(|family| family.signature))
    }


    pub fn adapter_binding_rejects_a_body_that_does_extra_work() {
        let (mut operations, terminator) = forwarding_body();
        operations.push(Operation::ConstInt { destination: ValueId::new(3), value: 1 });
        let (module, adapter, target) = adapter_candidate(operations, terminator);
        assert!(!adapter_shape_holds(&module, adapter, target));
    }



    pub fn source_recursive_layout_graph_refuses_unproved_cycles_and_mismatched_children() {
        use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};
        use witchy_syntax::ast::Type;
        use witchy_canonical_source::aggregates::ScalarAggregateShape;
        let named = |name: &str| Type::Named(name.into(), vec![]);
        let definition = |name: &str, fields: Vec<Type>| {
            let mut definition = ScalarAggregateShape {
                name: name.into(), fields, variants: vec![], tagged: false, packed: false,
            }.packed_view();
            definition.packed = false;
            definition
        };
        let records = BTreeMap::from([
            ("Tree".into(), definition("Tree", vec![named("Tree"), named("Int")])),
            ("Other".into(), definition("Other", vec![named("Other"), named("Float")])),
        ]);
        let mut layouts = LayoutInterner::new();
        let nominal = layouts.intern_opaque_nominal([17; 32]).unwrap();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(nominal, PointerSlot::Owned).unwrap();
        let physical = layouts.intern_packed_record(vec![slot, int]).unwrap();
        assert!(!layout_matches_source(nominal, &named("Tree"), &layouts, &records), "an unbound identity cannot close a recursive obligation");
        layouts.bind_nominal(nominal, physical).unwrap();
        assert!(!layout_matches_source(nominal, &named("Tree"), &layouts, &records),
            "a bounded structural carrier cannot prove this recursive nominal cycle");
        assert!(!layout_matches_source(nominal, &named("Other"), &layouts, &records), "a cycle does not excuse a mismatched sibling field");
        let pair = layouts.intern_packed_record(vec![slot, slot]).unwrap();
        assert!(!layout_matches_source(pair, &Type::Tuple(vec![named("Tree"), named("Other")]), &layouts, &records), "visited keys must include the source type, not just the layout");
        assert!(!layout_matches_source(nominal, &Type::Named("Option".into(), vec![named("Tree")]), &layouts, &records), "recursive identity cannot substitute for a tagged nullable layout");
    }

    pub fn source_generic_layout_graph_checks_instantiated_payload_types() {
        use crate::layout::{LayoutInterner, PointerSlot, RcHeader, ScalarKind};
        use witchy_syntax::ast::Type;
        use witchy_canonical_source::aggregates::ScalarAggregateShape;
        let named = |name: &str| Type::Named(name.into(), vec![]);
        let mut definition = ScalarAggregateShape {
            name: "Box".into(), fields: vec![named("a")], variants: vec![],
            tagged: false, packed: false,
        }.packed_view();
        definition.packed = false;
        definition.params = vec!["a".into()];
        let records = BTreeMap::from([("Box".into(), definition)]);
        let mut layouts = LayoutInterner::new();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = layouts.intern_runtime_buffer(byte).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(string, PointerSlot::Owned).unwrap();
        let int_box = layouts.intern_packed_record(vec![int]).unwrap();
        let string_box = layouts.intern_packed_record(vec![slot]).unwrap();
        for (argument, correct, wrong) in [("Int", int_box, string_box), ("String", string_box, int_box)] {
            let source = Type::Named("Box".into(), vec![named(argument)]);
            assert!(value_type_matches_source(
                super::super::ValueType::Ref(correct), &source, &layouts,
                &[], &mut CorrespondenceContext::default(), &records, &BTreeMap::new(),
            ));
            assert!(!value_type_matches_source(
                super::super::ValueType::Ref(wrong), &source, &layouts,
                &[], &mut CorrespondenceContext::default(), &records, &BTreeMap::new(),
            ));
        }
        assert!(!layout_matches_source(int_box, &Type::Named("Box".into(), vec![named("Missing")]), &layouts, &records));
        let source_list = Type::Named("List".into(), vec![named("Int")]);
        let retained_list = layouts.intern_packed_list(int, RcHeader::Required).unwrap();
        let headerless_list = layouts.intern_packed_list(int, RcHeader::Elided).unwrap();
        assert!(layout_matches_source(retained_list, &source_list, &layouts, &records));
        assert!(!layout_matches_source(headerless_list, &source_list, &layouts, &records),
            "source List correspondence requires a retained header");
    }











    pub fn final_authority_requires_the_exact_initial_checked_module() {
        let checked = check_initial_with_test_witness(optimizable_candidate()).unwrap();
        let initial_snapshot = checked.module().clone();
        let final_module = validate_final_for_test(checked).unwrap();
        assert_eq!(final_module.checked_source(), initial_snapshot.checked_source());
    }



    pub fn initial_checker_rejects_an_independent_tail_manifest_mismatch() {
        let candidate = candidate();
        assert!(matches!(
            check_initial_with_expected_digest(candidate, [0xff; 32]),
            Err(InitialCheckError::TailManifestDigestMismatch { .. })
        ));
    }


    pub fn optimized_authority_refuses_a_mutated_output_module() {
        let checked = check_initial_with_test_witness(optimizable_candidate()).unwrap();
        let optimized = crate::canonical_safewir::optimize::
            eliminate_same_block_redundant_bounds_guards_checked(checked)
            .unwrap();
        let (mut module, lowering, witness) = optimized.into_parts_for_test();
        module.functions[0].blocks[0].operations[0] =
            Operation::ConstInt { destination: ValueId::new(0), value: 99 };
        assert!(matches!(
            validate_optimized_final_parts_for_test(module, lowering, witness),
            Err(FinalValidationError::OptimizationDoesNotMatchWitness)
        ));
    }


}
