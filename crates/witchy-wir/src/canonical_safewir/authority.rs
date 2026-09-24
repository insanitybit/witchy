use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

use sha2::{Digest, Sha256};

use super::identity::{ModuleDigest, module_digest, tail_digest};
use super::{
    CanonicalModule, CheckedSourceIdentity, InitialCheckError, InitialValid, LoweredCandidate,
    ModuleStage, Revision,
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
}

impl CheckedInitialSafeWir {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    pub fn witness(&self) -> &CheckedLoweringWitness {
        &self.witness
    }

    #[cfg(test)]
    fn into_parts_for_test(self) -> (CanonicalModule<InitialValid>, CheckedLoweringWitness) {
        (self.module, self.witness)
    }

}

/// A relation-checked and fully revalidated optimized module. Its fields and
/// witness are private; final validation must verify the complete lineage.
pub struct CheckedOptimizedSafeWir {
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    optimizations: Vec<CheckedOptimizationWitness>,
}

impl CheckedOptimizedSafeWir {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        &self.module
    }

    #[cfg(test)]
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
    VmService(String),
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
    validate_external_imports(&candidate, source)?;
    if candidate.launch_contract_payload() != source.launch_contract_payload() {
        return Err(InitialWitnessError::LaunchContractMismatch);
    }
    let module = super::check::check_initial(candidate).map_err(InitialWitnessError::Safety)?;
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
    Ok(CheckedInitialSafeWir { module, witness })
}

fn validate_external_imports(
    candidate: &CanonicalModule<LoweredCandidate>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), InitialWitnessError> {
    let expected = source.precompiled_exports().iter().map(|export| {
        (
            format!("witchy.precompiled.{}", export.library_alias),
            export.export.clone(),
            super::ExternalWasmIdentity {
                library_binding_digest: export.library_binding_digest,
                wasm_sha256: export.wasm_sha256,
                sidecar_sha256: export.sidecar_sha256,
            },
        )
    }).collect::<HashSet<_>>();
    let actual = candidate.imports().iter().filter_map(|import| {
        let super::ImportContractIdentity::AuthenticatedExternal(identity) = &import.abi_identity else {
            return None;
        };
        Some((import.module.clone(), import.name.clone(), *identity))
    }).collect::<HashSet<_>>();
    (actual == expected).then_some(()).ok_or(InitialWitnessError::ExternalImportMismatch)
}

fn validate_candidate_abi_bindings<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<(), CandidateAbiBindingError> {
    use witchy_canonical_source::abi::CanonicalTargetPolicy;
    super::vm_service::validate(candidate, source).map_err(CandidateAbiBindingError::VmService)?;
    super::owner_authority::validate(candidate, source)?;

    let source_entries = source.callable_abi_manifest().entries().iter().filter_map(|entry| {
        let CanonicalTargetPolicy::ClosedLocal(target) = entry.target_policy else { return None };
        Some((target.function_ordinal, entry.key.as_bytes()))
    }).collect::<BTreeMap<_, _>>();
    let records = source.type_definitions();
    // Signature correspondence reads a `&String` field as its String token;
    // computed once, used only by the two layout-correspondence matches.
    let string_view_records = erase_record_string_views(&records);
    let access_facts = source.access_facts()
        .ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
    // A direct exclusive ABI has no syntax-only fallback: a stale or absent
    // checked loan graph must not be silently treated as an empty one.
    let loan_facts_available = source.loans_checked();
    // One decode of the candidate's layout bundle serves every signature
    // comparison and the closure catalog below.
    let (_bundle, layouts) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) {
        Ok(value) => value,
        Err(_) => return Err(CandidateAbiBindingError::MissingCanonicalLayouts),
    };
    let mut bindings = BTreeMap::new();
    let mut bound_functions = BTreeMap::new();
    for binding in candidate.source_abi_bindings().entries() {
        if bindings.insert(binding.source_declaration_ordinal(), binding).is_some() {
            return Err(CandidateAbiBindingError::DuplicateSourceDeclaration {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        if bound_functions.insert(binding.function(), binding).is_some() {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.function() });
        }
    }
    for source_declaration in source_entries.keys() {
        let Some(binding) = bindings.get(source_declaration) else {
            return Err(CandidateAbiBindingError::MissingBinding { source_declaration: *source_declaration });
        };
        let _ = binding;
    }
    for source_declaration in bindings.keys() {
        if !source_entries.contains_key(source_declaration) {
            return Err(CandidateAbiBindingError::UnexpectedBinding { source_declaration: *source_declaration });
        }
    }
    let mut functions = BTreeMap::new();
    for function in candidate.functions() {
        if functions.insert(function.id, function).is_some() {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: function.id });
        }
    }
    for binding in candidate.source_abi_bindings().entries() {
        let Some(function) = functions.get(&binding.function()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() });
        };
        let Some(signature) = candidate.signatures().iter().find(|signature| signature.id == function.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() });
        };
        let Some(source_signature) = source.closed_callable_signature(binding.source_declaration_ordinal()) else {
            return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: binding.source_declaration_ordinal() });
        };
        let source_declaration = source.closed_callable_declaration(binding.source_declaration_ordinal());
        if source_declaration.is_some_and(|declaration| {
            generic_shared_relay_template_candidate(source, &declaration.name)
                && !exact_generic_shared_relay_binding(source, source_signature, declaration, signature)
        }) {
            return Err(CandidateAbiBindingError::MissingCheckedReferenceRelation {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        let projected_scalar_relay = source_declaration.is_some_and(|declaration| {
            authenticated_projected_scalar_relay_relation_is_checked(
                declaration,
                source.module(),
                access_facts,
                &records,
            )
        });
        if !direct_reference_binding_is_checked(
            &source_signature,
            source_declaration,
            source_declaration.and_then(|declaration| access_facts.declaration(&declaration.name)),
            loan_facts_available,
            &records,
        ) && !projected_scalar_relay {
            return Err(CandidateAbiBindingError::MissingCheckedReferenceRelation {
                source_declaration: binding.source_declaration_ordinal(),
            });
        }
        let mut source_signature = restore_source_abi_qualifiers(
            source_signature,
            source_declaration,
            &records,
        );
        restore_authenticated_projected_scalar_relay_abi(
            &mut source_signature,
            source_declaration,
            source.module(),
            access_facts,
            &records,
        );
        let source_signature = erase_scalar_value_views(&erase_nested_string_views(&source_signature));
        let source_matches = candidate_signature_matches_source_with_layouts(
            signature,
            &source_signature,
            candidate,
            &string_view_records,
            &layouts,
        );
        let fresh_list_push_matches = !source_matches
            && source_declaration.is_some_and(|declaration|
                declaration.name.starts_with("list.push__")
                    || declaration.name.starts_with("list.__push__"))
            && {
                // The compiler-owned list.push helper always allocates its
                // replacement. Compare its physical ABI against source after
                // erasing only that stronger result fact; the safety checker
                // must still prove the original Unique contract from the body.
                let mut source_shaped = (*signature).clone();
                for result in &mut source_shaped.contract.results {
                    if let super::ResultContract::Unique { layout } = result {
                        *result = super::ResultContract::Shared { layout: *layout };
                    }
                }
                candidate_signature_matches_source_with_layouts(
                    &source_shaped,
                    &source_signature,
                    candidate,
                    &string_view_records,
                    &layouts,
                )
            };
        let projected_contract_matches = source_declaration.is_none_or(|declaration| {
            projected_exclusive_result_contract_matches(signature, declaration, &records)
        });
        if (!source_matches || !projected_contract_matches) && !fresh_list_push_matches {
            return Err(CandidateAbiBindingError::CandidateSignatureMismatch { function: binding.function() });
        }
    }
    // A function synthesized for a lambda is bound to that lambda's closure
    // witness instead of a source declaration.  The uniform closure record
    // owns a tagged environment record; captures are its checked scalar or
    // managed-reference payloads, never capabilities or an erased value.
    let closures = source.closure_witness().entries();
    let closure_layout_catalog = &layouts;
    let mut closure_bound = BTreeMap::new();
    for binding in candidate.closure_bindings().entries() {
        if bound_functions.contains_key(&binding.function())
            || closure_bound.insert(binding.function(), binding).is_some()
        {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.function() });
        }
        let Some(witness) = closures.get(binding.closure() as usize) else {
            return Err(CandidateAbiBindingError::MissingClosureWitness { closure: binding.closure() });
        };
        let Some(function) = functions.get(&binding.function()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() });
        };
        let Some(signature) = candidate.signatures().iter().find(|signature| signature.id == function.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() });
        };
        let source_signature = witness.declared_signature().clone();
        if let Some(super::ValueType::Gc(owner)) = signature.parameters.first() {
            if !candidate.owner_families().iter().any(|family| matches!(family.source, super::closure_owner_contract::OwnerFamilySource::Closure)
                && family.owner_type == *owner && family.signature == signature.id
                && family.constructors.iter().any(|constructor| constructor.function == function.id && constructor.witness == Some(binding.closure())))
            {
                return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() });
            }
            let mut declaration = signature.clone();
            declaration.parameters.remove(0);
            if declaration.contract.parameters.is_empty() {
                return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
            }
            declaration.contract.parameters.remove(0);
            declaration.contract.parameter_effects = declaration.contract.parameter_effects.into_iter()
                .filter_map(|(index, effects)| index.checked_sub(1).map(|index| (index, effects))).collect();
            declaration.contract.results = strip_closure_result_contracts(&declaration.contract.results)
                .ok_or(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() })?;
            if !candidate_signature_matches_source_with_layouts(&declaration, &source_signature, candidate, &records, &layouts) {
                return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
            }
            continue;
        }
        let family_key = source_closure_family_key(&source_signature, &records);
        let family = closures.iter().filter(|entry| {
            entry.signature().cloned()
                .is_some_and(|signature| source_closure_family_key(&signature, &records) == family_key)
        }).collect::<Vec<_>>();
        let Some(tag) = family.iter().position(|entry| entry.id() == witness.id()) else {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        };
        let environment = match signature.parameters.first() {
            Some(super::ValueType::Ref(layout)) => closure_layout_catalog
                .get(*layout)
                .and_then(|descriptor| match descriptor.kind() {
                    crate::layout::LayoutKind::PackedRecord { fields } if fields.len() == 2 => {
                        let crate::layout::LayoutKind::RuntimePointerValueSlot {
                            pointee, semantics: crate::layout::PointerSlot::Owned,
                        } = closure_layout_catalog.get(fields[1])?.kind() else { return None };
                        let physical = closure_layout_catalog.resolve_nominal(*pointee).ok()?;
                        closure_layout_catalog.get(physical).and_then(|environment| match environment.kind() {
                            crate::layout::LayoutKind::PackedRecord { fields } => Some((*pointee, fields.clone())),
                            _ => None,
                        })
                    }
                    _ => None,
                })
                .ok_or(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() })?,
            _ => return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() }),
        };
        let (environment_layout, environment_fields) = environment;
        let expected = family.iter().flat_map(|entry| entry.captures()).collect::<Vec<_>>();
        let own_offset = family[..tag].iter().map(|entry| entry.captures().len()).sum::<usize>();
        let tag_is_int = environment_fields.first().is_some_and(|field| matches!(
            closure_layout_catalog.get(*field).map(|field| field.kind()),
            Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))
        ));
        let mut correspondence = CorrespondenceContext::with_owners(candidate);
        let expected_field_types = expected.iter().enumerate().map(|(index, capture)| {
                let field = *environment_fields.get(index + 1)?;
                let actual = match closure_layout_catalog.get(field).map(|field| field.kind()) {
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration)) => super::ValueType::Int,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float)) => super::ValueType::Float,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool)) => super::ValueType::Bool,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit)) => super::ValueType::Unit,
                    Some(crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::OwnedNullable }) => super::ValueType::Ref(*pointee),
                    _ => return None,
                };
                let transfer_matches = matches!(
                    (actual, capture.transfer()),
                    (super::ValueType::Ref(_), witchy_canonical_source::closure::ClosureCaptureTransfer::ManagedReferenceByValue)
                        | (super::ValueType::Int | super::ValueType::Float | super::ValueType::Bool | super::ValueType::Unit,
                           witchy_canonical_source::closure::ClosureCaptureTransfer::CopyScalar)
                ) || (
                    // A whole-root view (`&String`, a record or sum, a scalar
                    // List) is its retained object token, so its capture is a
                    // managed reference slot.
                    matches!(actual, super::ValueType::Ref(_))
                        && matches!(capture.transfer(), witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue)
                        && is_direct_rooted_view_whole_root(capture.checked_type(), &records)
                        && !matches!(capture.checked_type(),
                            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                if witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner))
                ) || (
                    // A `&Float`/`&Bool`/`&Duration` view is its value
                    // (`ScalarValue`), so its capture is a copied scalar slot.
                    matches!(actual, super::ValueType::Int | super::ValueType::Float | super::ValueType::Bool)
                        && matches!(capture.transfer(), witchy_canonical_source::closure::ClosureCaptureTransfer::RootedViewByValue)
                        && matches!(capture.checked_type(),
                            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                                if witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner))
                );
                (transfer_matches && value_type_matches_source(
                    actual, capture.checked_type(), closure_layout_catalog, candidate.signatures(), &mut correspondence,
                    &records, candidate.closure_layouts(),
                )).then_some(actual)
            }).collect::<Vec<_>>();
        let fields_match = environment_fields.len() == expected.len() + 1
            && expected_field_types.iter().all(Option::is_some);
        // Tag zero is reserved for the adapter environment.  The binding's
        // source-witness ordinal therefore selects its exact nonzero variant.
        if !tag_is_int
            || !fields_match
            || !closure_environment_prefix_matches(
                function,
                environment_layout,
                i64::try_from(tag + 1).ok(),
                u32::try_from(own_offset + 1).ok(),
                &expected_field_types[own_offset..own_offset + witness.captures().len()],
            )
        {
            return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() });
        }
        // The synthesized function takes the closure object first; only the
        // remaining lanes correspond to the source lambda's own signature.
        let Some(source_lanes) = strip_closure_environment(signature, candidate) else {
            return Err(CandidateAbiBindingError::MissingClosureEnvironmentParameter { function: binding.function() });
        };
        if !candidate_signature_matches_source_with_layouts(&source_lanes, &source_signature, candidate, &records, &layouts) {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        }
    }
    // A function synthesized for a named function used as a value forwards to
    // its target under the environment-taking ABI and does nothing else.
    let mut adapter_bound = BTreeMap::new();
    for binding in candidate.adapter_bindings().entries() {
        if bound_functions.contains_key(&binding.adapter())
            || closure_bound.contains_key(&binding.adapter())
            || adapter_bound.insert(binding.adapter(), binding).is_some()
        {
            return Err(CandidateAbiBindingError::DuplicateCandidateFunction { function: binding.adapter() });
        }
        let Some(adapter) = functions.get(&binding.adapter()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.adapter() });
        };
        let Some(target) = functions.get(&binding.target()) else {
            return Err(CandidateAbiBindingError::MissingCandidateFunction { function: binding.target() });
        };
        let Some(adapter_signature) = candidate.signatures().iter().find(|signature| signature.id == adapter.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.adapter() });
        };
        let Some(target_signature) = candidate.signatures().iter().find(|signature| signature.id == target.signature) else {
            return Err(CandidateAbiBindingError::MissingCandidateSignature { function: binding.target() });
        };
        if strip_closure_environment(adapter_signature, candidate)
            .is_none_or(|lanes| !signature_lanes_match(&lanes, target_signature))
        {
            return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
        }
        let owner_family = candidate.owner_families().iter().find(|family| {
            matches!(family.source, super::closure_owner_contract::OwnerFamilySource::Closure)
                && family.signature == adapter.signature
                && adapter_signature.parameters.first() == Some(&super::ValueType::Gc(family.owner_type))
        });
        if let Some(family) = owner_family {
            let Some(constructor) = family.constructors.iter().find(|constructor| {
                constructor.function == adapter.id && constructor.witness.is_none()
            }) else {
                return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
            };
            if constructor.environment_tag != 0
                || !constructor.captures.is_empty()
                || target.signature != family.declaration_signature
            {
                return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
            }
        }
        if !adapter_body_forwards(adapter, target.id, target_signature, owner_family.map(|family| family.signature)) {
            return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: binding.adapter() });
        }
    }
    if functions.len() != bound_functions.len() + closure_bound.len() + adapter_bound.len() {
        let function = functions.keys()
            .find(|function| !bound_functions.contains_key(function)
                && !closure_bound.contains_key(function)
                && !adapter_bound.contains_key(function))
            .copied().unwrap_or_else(|| *bound_functions.keys().next().expect("nonempty mismatch"));
        return Err(CandidateAbiBindingError::MissingCandidateFunction { function });
    }
    Ok(())
}

/// The signature that remains when the leading closure-environment parameter
/// is removed. `None` when the first parameter is not a shared reference to a
/// layout the module registered as carrying this very signature -- so a
/// candidate cannot smuggle an arbitrary extra parameter past the comparison.
fn strip_closure_environment<S: ModuleStage>(
    signature: &super::FunctionSignature,
    candidate: &CanonicalModule<S>,
) -> Option<super::FunctionSignature> {
    match signature.parameters.first()? {
        super::ValueType::Ref(layout) => {
            if candidate.closure_layouts().get(layout) != Some(&signature.id) {
                return None;
            }
        }
        super::ValueType::Gc(owner) => {
            if !candidate.owner_families().iter().any(|family| {
                matches!(family.source, super::closure_owner_contract::OwnerFamilySource::Closure)
                    && family.owner_type == *owner && family.signature == signature.id
            }) {
                return None;
            }
        }
        _ => return None,
    }
    let environment = signature.contract.parameters.first()?;
    if environment.value_type != signature.parameters[0]
        || environment.ownership != super::ParameterOwnership::Shared
    {
        return None;
    }
    Some(super::FunctionSignature {
        id: signature.id,
        parameters: signature.parameters[1..].to_vec(),
        results: signature.results.clone(),
        contract: super::CallableContract {
            parameters: signature.contract.parameters[1..].to_vec(),
            results: strip_closure_result_contracts(&signature.contract.results)?,
            parameter_effects: signature
                .contract
                .parameter_effects
                .iter()
                .filter_map(|(parameter, effects)| {
                    parameter.checked_sub(1).map(|shifted| (shifted, effects.clone()))
                })
                .collect(),
        },
    })
}

fn strip_closure_environment_with(
    signature: &super::FunctionSignature,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> Option<super::FunctionSignature> {
    let super::ValueType::Ref(layout) = *signature.parameters.first()? else { return None };
    if closure_layouts.get(&layout) != Some(&signature.id) {
        return None;
    }
    let environment = signature.contract.parameters.first()?;
    if environment.value_type != super::ValueType::Ref(layout)
        || environment.ownership != super::ParameterOwnership::Shared
    {
        return None;
    }
    Some(super::FunctionSignature {
        id: signature.id,
        parameters: signature.parameters[1..].to_vec(),
        results: signature.results.clone(),
        contract: super::CallableContract {
            parameters: signature.contract.parameters[1..].to_vec(),
            results: strip_closure_result_contracts(&signature.contract.results)?,
            parameter_effects: signature
                .contract
                .parameter_effects
                .iter()
                .filter_map(|(parameter, effects)| {
                    parameter.checked_sub(1).map(|shifted| (shifted, effects.clone()))
                })
                .collect(),
        },
    })
}

/// Closure ABI parameter zero is the environment record.  Remove it from
/// result-alias targets together with the parameter lane; a bare physical
/// layout must never make an alias point at that hidden owner.
fn strip_closure_result_contracts(
    results: &[super::ResultContract],
) -> Option<Vec<super::ResultContract>> {
    results
        .iter()
        .cloned()
        .map(|result| match result {
            super::ResultContract::AliasesParameter { parameter, layout } => {
                Some(super::ResultContract::AliasesParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                })
            }
            super::ResultContract::AliasesWriteBackParameter { parameter, layout, projection } => {
                Some(super::ResultContract::AliasesWriteBackParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                    projection,
                })
            }
            super::ResultContract::ReallocatesParameter { parameter, layout } => {
                Some(super::ResultContract::ReallocatesParameter {
                    parameter: parameter.checked_sub(1)?,
                    layout,
                })
            }
            result => Some(result),
        })
        .collect()
}

/// Two signatures carry the same lanes: the identity differs (an adapter has
/// its own signature id) but every parameter, result, and contract does not.
fn signature_lanes_match(
    actual: &super::FunctionSignature,
    expected: &super::FunctionSignature,
) -> bool {
    actual.parameters == expected.parameters
        && actual.results == expected.results
        && actual.contract.parameters == expected.contract.parameters
        && actual.contract.results == expected.contract.results
        && actual.contract.parameter_effects == expected.contract.parameter_effects
}

/// The adapter's body is exactly one forwarding call: one block, one `Call` to
/// the target passing the adapter's parameters 1.. in order, optionally one
/// release of the environment token it was handed, and a `Return` of the
/// call's destinations. Anything else -- a second call, arithmetic, a
/// reordered argument, a different terminator -- refuses.
fn adapter_body_forwards(
    adapter: &super::Function,
    target: super::FunctionId,
    target_signature: &super::FunctionSignature,
    owner_family: Option<super::FunctionSignatureId>,
) -> bool {
    let [block] = adapter.blocks.as_slice() else { return false };
    if block.id != adapter.entry {
        return false;
    }
    let parameters = block.parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
    let Some((environment, forwarded)) = parameters.split_first() else { return false };
    let (call, tail) = match block.operations.split_first() {
        Some((call, tail)) => (call, tail),
        None => return false,
    };
    let super::Operation::Call { destinations, callee, arguments } = call else { return false };
    if *callee != target || arguments.len() != forwarded.len() {
        return false;
    }
    // A reference lane is forwarded by moving the token; a scalar lane is
    // passed by value. The convention is the target's, not the adapter's.
    if !arguments.iter().zip(forwarded).enumerate().all(|(index, (argument, parameter))| {
        match (argument, target_signature.parameters.get(index)) {
            (super::CallArgument::Move(value), Some(super::ValueType::Ref(_) | super::ValueType::Gc(_))) => value == parameter,
            (super::CallArgument::Value(value), Some(value_type)) => {
                !matches!(value_type, super::ValueType::Ref(_)) && value == parameter
            }
            _ => false,
        }
    }) {
        return false;
    }
    // Exactly one release of the environment token the adapter was handed:
    // every call site clones the record for the callee, so an adapter that
    // kept its token would leak one reference per call.
    match tail {
        [super::Operation::Release { object } | super::Operation::DropRef { object }]
            if owner_family.is_none() && object == environment => {}
        [super::Operation::ClosureOwner(super::closure_owner_contract::OwnerOperation::Drop { owner, family })]
            if owner_family == Some(*family) && owner == environment => {}
        _ => return false,
    }
    matches!(&block.terminator, super::Terminator::Return { values } if values == destinations)
}

/// The candidate itself is not trusted to pair a lambda's code with its
/// environment.  The source witness ordinal fixes the nonzero tag and the
/// lambda must check that tag before it can load any capture payload.
fn closure_environment_prefix_matches(
    function: &super::Function,
    environment_layout: crate::layout::LayoutId,
    expected_tag: Option<i64>,
    first_capture_field: Option<u32>,
    fields: &[Option<super::ValueType>],
) -> bool {
    let Some(expected_tag) = expected_tag else { return false };
    let Some(first_capture_field) = first_capture_field else { return false };
    let Some(block) = function.blocks.iter().find(|block| block.id == function.entry) else { return false };
    let Some(closure) = block.parameters.first().map(|parameter| parameter.value) else { return false };
    let Some((environment_load, tail)) = block.operations.split_first() else { return false };
    let super::Operation::Load { destination: environment, place, transfer } = environment_load else { return false };
    if place.object() != closure
        || !matches!(place.projection(), super::Projection::Field { index: 1 })
        || place.value_type() != super::ValueType::Ref(environment_layout)
        || place.access() != super::AccessMode::Read
        || *transfer != super::LoadTransfer::CloneShared
    {
        return false;
    }
    let Some((tag_load, tail)) = tail.split_first() else { return false };
    let super::Operation::Load { destination: tag, place, transfer } = tag_load else { return false };
    if place.object() != *environment
        || !matches!(place.projection(), super::Projection::Field { index: 0 })
        || place.value_type() != super::ValueType::Int
        || place.access() != super::AccessMode::Read
        || *transfer != super::LoadTransfer::CopyScalar
    {
        return false;
    }
    // Only a capture-free lambda can release the cloned environment before
    // its guard: no subsequent operation can load an environment payload.
    let tail = if fields.is_empty() {
        match tail.split_first() {
            Some((super::Operation::Release { object } | super::Operation::DropRef { object }, rest)) if *object == *environment => rest,
            _ => tail,
        }
    } else {
        tail
    };
    let Some((constant, tail)) = tail.split_first() else { return false };
    let super::Operation::ConstInt { destination: expected, value } = constant else { return false };
    if *value != expected_tag { return false; }
    let Some((comparison, tail)) = tail.split_first() else { return false };
    let super::Operation::Compare { destination: condition, operator, left, right } = comparison else { return false };
    if *operator != super::ComparisonOperator::Equal
        || !((*left == *tag && *right == *expected) || (*left == *expected && *right == *tag))
    {
        return false;
    }
    let Some((guard, capture_loads)) = tail.split_first() else { return false };
    if !matches!(guard, super::Operation::Guard { condition: guard_condition, kind: super::GuardKind::DynamicType } if *guard_condition == *condition) {
        return false;
    }
    let capture_start = if fields.is_empty() && matches!(block.operations.get(2), Some(super::Operation::Release { object } | super::Operation::DropRef { object }) if *object == *environment) { 6 } else { 5 };
    let captures_match = fields.iter().enumerate().all(|(offset, field)| {
        let Some(value_type) = field else { return false };
        let Some(super::Operation::Load { place, transfer, .. }) = capture_loads.get(offset) else { return false };
        place.object() == *environment
            && matches!(place.projection(), super::Projection::Field { index } if *index == first_capture_field + offset as u32)
            && place.value_type() == *value_type
            && place.access() == super::AccessMode::Read
            && match value_type {
                super::ValueType::Ref(_) => *transfer == super::LoadTransfer::ClonePresent,
                _ => *transfer == super::LoadTransfer::CopyScalar,
            }
    });
    captures_match
        && hidden_environment_uses_are_closed(function, closure, *environment, capture_start, fields.len())
}

/// The synthesized closure and environment parameters are compiler-private.
/// After the authenticated prefix, only their affine cleanup may mention them;
/// no alias, reload, call argument, branch argument, or later place can expose
/// an inactive family payload.
fn hidden_environment_uses_are_closed(
    function: &super::Function,
    closure: super::ValueId,
    environment: super::ValueId,
    capture_start: usize,
    capture_count: usize,
) -> bool {
    for block in &function.blocks {
        if crate::canonical_safewir::check::terminator_uses(&block.terminator)
            .iter().any(|value| *value == closure || *value == environment)
        {
            return false;
        }
        for (index, operation) in block.operations.iter().enumerate() {
            let mentions_hidden = crate::canonical_safewir::check::operation_uses(operation)
                .iter().any(|value| *value == closure || *value == environment);
            if !mentions_hidden { continue; }
            let prefix_use = block.id == function.entry
                && (index == 0 || index == 1 || (capture_start..capture_start + capture_count).contains(&index));
            let cleanup = matches!(operation,
                super::Operation::Release { object } | super::Operation::DropRef { object }
                if *object == closure || *object == environment
            );
            if !prefix_use && !cleanup { return false; }
        }
    }
    true
}

/// The checked module's type definitions, keyed by bare and entry-qualified
/// names. A nominal scalar aggregate (a record, a sum type, a scalar
/// `Option`/`Result`) binds to a candidate reference whose layout carries
/// exactly the slots `witchy_canonical_source::aggregates` derives for it.
type ScalarRecordFields = BTreeMap<String, witchy_syntax::ast::TypeDef>;

/// The only source form permitted to carry an exclusive result through a
/// record is a final `&mut parameter.named_string_field`.  This descriptor is
/// deliberately source-derived: it selects neither a candidate layout nor a
/// candidate contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProjectedExclusiveReturn {
    parameter: usize,
    field: u32,
}

fn projected_exclusive_return_source(
    declaration: &witchy_syntax::ast::Function,
    records: &ScalarRecordFields,
) -> Option<ProjectedExclusiveReturn> {
    use witchy_syntax::ast::{Expr, Stmt, Type, TypeQual, UnOp};

    let result = declaration.ret.as_ref()?;
    let (lifetime, result_referent) = direct_exclusive_reference_parts(result)?;
    if !is_projected_exclusive_leaf(result_referent) {
        return None;
    }
    let Some(Stmt::Expr(Expr::Unary { op: UnOp::BorrowMut, expr })) = declaration.body.stmts.last() else {
        return None;
    };
    let Expr::Field { base, field } = expr.as_ref() else { return None };
    let Expr::Var(owner_name) = base.as_ref() else { return None };
    let owners = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        let Type::Qualified(TypeQual::BorrowMut(candidate_lifetime), owner) = parameter.ty.as_ref()? else {
            return None;
        };
        (parameter.convention == witchy_syntax::ast::Convention::Let
            && candidate_lifetime == lifetime
            && parameter.name == *owner_name
            && matches!(owner.unqualified(), Type::Named(_, arguments) if arguments.is_empty()))
            .then_some((index, owner.as_ref()))
    }).collect::<Vec<_>>();
    let [(parameter, owner)] = owners.as_slice() else { return None };
    let Type::Named(name, arguments) = owner.unqualified() else { return None };
    if !arguments.is_empty() { return None; }
    let definition = records.get(name).or_else(|| name.rsplit('.').next().and_then(|bare| records.get(bare)))?;
    if definition.packed || definition.variants.len() != 1 { return None; }
    let variant = &definition.variants[0];
    let field_index = variant.field_names.iter().position(|candidate| candidate == field)?;
    if !variant.fields.get(field_index).is_some_and(is_projected_exclusive_leaf) {
        return None;
    }
    Some(ProjectedExclusiveReturn {
        parameter: *parameter,
        field: u32::try_from(field_index).ok()?,
    })
}

fn projected_exclusive_result_contract_matches(
    signature: &super::FunctionSignature,
    declaration: &witchy_syntax::ast::Function,
    records: &ScalarRecordFields,
) -> bool {
    let Some(expected) = projected_exclusive_return_source(declaration, records) else { return true };
    matches!(signature.contract.results.first(),
        Some(super::ResultContract::AliasesWriteBackParameter {
            parameter,
            projection: Some(field),
            ..
        }) if usize::try_from(*parameter).ok() == Some(expected.parameter) && *field == expected.field)
}

/// A family selects environment variants, not callable authority. Resolve
/// checked nominal aliases and spell out default conventions before grouping;
/// full signature, capture, transfer, and tag checks remain independent above.
pub(super) fn source_closure_family_key(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::{CallableQualifiers, Convention, Type};
    fn normalize(source: &Type) -> Type {
        if let Some(view) = witchy_canonical_source::aggregates::owner_carried_view_identity(source) {
            return view;
        }
        match source.unqualified() {
            Type::Fn(parameters, result, conventions, _) => Type::Fn(
                parameters.iter().map(normalize).collect(),
                Box::new(normalize(result)),
                if conventions.is_empty() { vec![Convention::Let; parameters.len()] }
                else { conventions.clone() },
                CallableQualifiers::default(),
            ),
            Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(normalize).collect()),
            Type::Tuple(fields) => Type::Tuple(fields.iter().map(normalize).collect()),
            other => other.clone(),
        }
    }
    normalize(&witchy_canonical_source::aggregates::concrete_aggregate_identity(source, records)
        .unwrap_or_else(|| source.unqualified().clone()))
}

const MAX_SOURCE_CORRESPONDENCE_PAIRS: usize = 65_536;

#[derive(Default)]
pub(super) struct CorrespondenceContext {
    owner_families: Vec<super::closure_owner_contract::OwnerFamily>,
    active_callable: HashSet<(super::FunctionSignatureId, witchy_syntax::ast::Type)>,
    completed_callable: HashSet<(super::FunctionSignatureId, witchy_syntax::ast::Type)>,
    active_layout: HashSet<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    completed_layout: HashSet<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    pairs: usize,
}

enum CorrespondenceVisit {
    New,
    Active,
    Completed,
    Limit,
}

impl CorrespondenceContext {
    pub(super) fn with_owners<S: ModuleStage>(candidate: &CanonicalModule<S>) -> Self {
        Self { owner_families: candidate.owner_families().to_vec(), ..Self::default() }
    }
    fn enter_callable(
        &mut self,
        key: (super::FunctionSignatureId, witchy_syntax::ast::Type),
    ) -> CorrespondenceVisit {
        if self.completed_callable.contains(&key) {
            return CorrespondenceVisit::Completed;
        }
        if self.active_callable.contains(&key) {
            return CorrespondenceVisit::Active;
        }
        if self.pairs >= MAX_SOURCE_CORRESPONDENCE_PAIRS {
            return CorrespondenceVisit::Limit;
        }
        self.pairs += 1;
        self.active_callable.insert(key);
        CorrespondenceVisit::New
    }

    fn finish_callable(
        &mut self,
        key: (super::FunctionSignatureId, witchy_syntax::ast::Type),
        matched: bool,
    ) {
        self.active_callable.remove(&key);
        if matched {
            self.completed_callable.insert(key);
        }
    }

    fn enter_layout(
        &mut self,
        key: (crate::layout::LayoutId, witchy_syntax::ast::Type),
    ) -> CorrespondenceVisit {
        if self.completed_layout.contains(&key) {
            return CorrespondenceVisit::Completed;
        }
        if self.active_layout.contains(&key) {
            return CorrespondenceVisit::Active;
        }
        if self.pairs >= MAX_SOURCE_CORRESPONDENCE_PAIRS {
            return CorrespondenceVisit::Limit;
        }
        self.pairs += 1;
        self.active_layout.insert(key);
        CorrespondenceVisit::New
    }

    fn finish_layout(
        &mut self,
        key: (crate::layout::LayoutId, witchy_syntax::ast::Type),
        matched: bool,
    ) {
        self.active_layout.remove(&key);
        if matched {
            self.completed_layout.insert(key);
        }
    }
}

fn candidate_signature_matches_source<S: ModuleStage>(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
) -> bool {
    candidate_signature_matches_source_with_records(actual, source, candidate, &BTreeMap::new())
}

pub(super) fn candidate_signature_matches_source_with_records<S: ModuleStage>(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
    records: &ScalarRecordFields,
) -> bool {
    let (_, layouts) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) { Ok(value) => value, Err(_) => return false };
    candidate_signature_matches_source_with_layouts(actual, source, candidate, records, &layouts)
}

/// `candidate_signature_matches_source_with_records` over an already decoded
/// layout catalog: a binding pass compares hundreds of signatures against one
/// bundle and must not decode it once per comparison.
pub(super) fn candidate_signature_matches_source_with_layouts<S: ModuleStage>(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
    records: &ScalarRecordFields,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    candidate_signature_matches_source_inner(actual, source, parameters, result, conventions, layouts, candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), records, candidate.closure_layouts())
}

/// `List(&Int)`: the `&Int` family's owner list. Each element is one retained,
/// authenticated place carrier; the list itself is an ordinary owner list.
fn is_list_of_direct_int_views(ty: &witchy_syntax::ast::Type) -> bool {
    matches!(ty.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "List" && arguments.len() == 1 && is_direct_rooted_view_int(&arguments[0]))
}

/// Check the original source graph before any ABI comparison erases wrappers.
/// A root Unique on an own/var input or result is represented by the callable
/// ownership contract. The exact Result-Ok collection carrier and a
/// `List(&Int)` parameter or result are the authenticated nested exceptions;
/// all other nested qualifiers remain unsupported.
fn source_abi_qualifiers_supported(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_syntax::ast::{Convention, Type, TypeQual};
    let Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    // The direct shared-reference slice carries an already-addressable
    // reference-counted shell (String/Bytes/List(Int)) or the separately
    // authenticated scalar-place owner. Direct exclusive references are a
    // second shallow exception: only a plain declaration parameter receives
    // the authenticated hidden write-back lane. It is deliberately not a
    // recursive qualifier exception.
    let mut pending = (!(is_direct_shared_reference(result)
        || is_direct_rooted_view_int(result) || is_direct_rooted_view_slice(result)
        || is_list_of_direct_int_views(result)
        || is_direct_rooted_view_whole_root(result, records)
        || is_direct_exclusive_reference(result)
        || is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records)
        || witchy_syntax::ast::is_direct_unique_result_ok_collection(result)))
        .then_some((result.as_ref(), true))
        .into_iter()
        .collect::<Vec<_>>();
    pending.extend(parameters.iter().enumerate().filter_map(|(index, parameter)| {
        (!(
            is_direct_shared_reference(parameter)
            || is_direct_rooted_view_int(parameter) || is_direct_rooted_view_slice(parameter)
            || is_list_of_direct_int_views(parameter)
            || is_direct_rooted_view_whole_root(parameter, records)
            || is_direct_rooted_view_aggregate_carrier(parameter, records)
            || (matches!(conventions.get(index), Some(Convention::Let | Convention::Own))
                && is_direct_exclusive_reference(parameter))
            || projected_exclusive_signature_parameter(parameters, result, conventions) == Some(index)
        )).then_some((
            parameter,
            matches!(conventions.get(index), Some(Convention::Own | Convention::Var)),
        ))
    }));
    let mut definitions = HashSet::new();
    let mut visited = 0;
    while let Some((ty, var_parameter)) = pending.pop() {
        visited += 1;
        if visited > MAX_SOURCE_CORRESPONDENCE_PAIRS { return false; }
        match ty {
            Type::Qualified(TypeQual::Frozen, inner) => pending.push((inner, false)),
            Type::Qualified(TypeQual::Unique, inner) if var_parameter => pending.push((inner, false)),
            // A direct RFC-0167 view nested in a callable or generic argument
            // (`fn(&String) -> &String`, `Task(&Int)`) is a value-semantic
            // snapshot carrier with an authenticated retained-root family.
            // Its lanes are still bound by value correspondence; only the
            // qualifier itself is not a nested loan to refuse here.
            Type::Qualified(TypeQual::View, inner)
                if !matches!(inner.as_ref(), Type::Qualified(qualifier, _) if *qualifier != TypeQual::Frozen)
                    && (is_direct_rooted_view_int(ty) || is_direct_rooted_view_slice(ty)
                        || is_direct_rooted_view_whole_root(ty, records)) => {}
            Type::Qualified(_, _) => return false,
            Type::Fn(parameters, result, conventions, _) => {
                // A callable value can carry the one direct mutable String
                // lane only through the adapter contract checked below.  Do
                // not recurse into its `&mut` spelling: this is not a general
                // nested-reference exception, and all other callable shapes
                // remain unsupported at this source boundary.
                if is_direct_callable_writeback_carrier(ty) {
                    continue;
                }
                if !conventions.is_empty() && conventions.len() != parameters.len() { return false; }
                pending.push((result, true));
                pending.extend(parameters.iter().enumerate().map(|(index, parameter)| {
                    (parameter, matches!(conventions.get(index), Some(Convention::Own | Convention::Var)))
                }));
            }
            Type::Named(name, arguments) => {
                pending.extend(arguments.iter().map(|argument| (argument, false)));
                // Generic arguments are checked above; qualifiers written in
                // the declaration must also survive nominal layout recursion.
                if definitions.insert(name) && let Some(definition) = records.get(name) {
                    pending.extend(definition.variants.iter().flat_map(|variant|
                        variant.fields.iter().map(|field| (field, false))));
                }
            }
            Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                pending.extend(arguments.iter().map(|argument| (argument, false)));
            }
            Type::Slice(inner) => pending.push((inner, false)),
            Type::RecordCompose { base, fields } => {
                pending.push((base, false));
                pending.extend(fields.iter().map(|(_, field)| (field, false)));
            }
        }
    }
    true
}

/// The only nested callable ABI whose hidden write-back lane is presently
/// authenticated by canonical lowering and the adapter binding checker.
/// This is intentionally structural *only after* the outer source declaration
/// has been bound to its checked access contract; it never admits shared,
/// returned, projected, generic, or multi-parameter reference callables.
fn is_direct_callable_writeback_carrier(ty: &witchy_syntax::ast::Type) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = ty.unqualified() else {
        return false;
    };
    matches!(parameters.as_slice(), [parameter]
        if matches!(conventions.as_slice(), [] | [witchy_syntax::ast::Convention::Let])
            && is_direct_exclusive_reference(parameter))
        && matches!(result.unqualified(),
            witchy_syntax::ast::Type::Named(name, arguments)
                if name == "String" && arguments.is_empty())
}

fn is_direct_shared_int(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
}

/// The initial RFC-0167 view carrier is deliberately narrower than the
/// legacy reference ABI: only a lifetime-free `&Int` maps to the registered
/// opaque root-plus-projection owner family.  This is an exact source-type
/// match, not a representation or shape fallback.
fn is_direct_rooted_view_int(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
}

/// A `&String` view is its retained String token (`StringRoot`), so inside
/// an aggregate (`Option(&String)`, `(&String, Int)`, a record field) it has
/// exactly the String layout.  The callable's own direct `&String` lanes keep
/// their dedicated whole-root correspondence and are left as written.
fn erase_nested_string_views(signature: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    let lane = |ty: &Type| -> Type {
        match ty {
            Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(erase_string_view).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(erase_string_view).collect()),
            other => other.clone(),
        }
    };
    match signature {
        Type::Fn(parameters, result, conventions, qualifiers) => Type::Fn(
            parameters.iter().map(lane).collect(),
            Box::new(lane(result)),
            conventions.clone(),
            *qualifiers,
        ),
        other => other.clone(),
    }
}

/// A `&Float`/`&Bool`/`&Duration` view is carried as the scalar value
/// (`ScalarValue`), at every position, so correspondence reads it as the
/// scalar.
fn erase_scalar_value_views(ty: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner) =>
        {
            inner.unqualified().clone()
        }
        Type::Qualified(qualifier, inner) => Type::Qualified(qualifier.clone(), Box::new(erase_scalar_value_views(inner))),
        Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(erase_scalar_value_views).collect()),
        Type::Tuple(items) => Type::Tuple(items.iter().map(erase_scalar_value_views).collect()),
        Type::Fn(parameters, result, conventions, qualifiers) => Type::Fn(
            parameters.iter().map(erase_scalar_value_views).collect(),
            Box::new(erase_scalar_value_views(result)),
            conventions.clone(),
            *qualifiers,
        ),
        other => other.clone(),
    }
}

/// Read every `&String` in `ty` as `String`: the view's carrier is the token.
fn erase_string_view(ty: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), Type::Named(name, arguments) if name == "String" && arguments.is_empty()) =>
        {
            Type::Named("String".into(), Vec::new())
        }
        Type::Qualified(qualifier, inner) => Type::Qualified(qualifier.clone(), Box::new(erase_string_view(inner))),
        Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(erase_string_view).collect()),
        Type::Tuple(items) => Type::Tuple(items.iter().map(erase_string_view).collect()),
        other => other.clone(),
    }
}

/// The same reading for record and sum fields, used only for signature
/// correspondence: a `&String` field is laid out as a String field.
fn erase_record_string_views(records: &ScalarRecordFields) -> ScalarRecordFields {
    records.iter().map(|(name, definition)| {
        let mut definition = definition.clone();
        for variant in &mut definition.variants {
            variant.fields = variant.fields.iter().map(erase_string_view).collect();
        }
        (name.clone(), definition)
    }).collect()
}

/// A whole-root view carried as the retained object token: `&String`
/// (`StringRoot`) or a view of a declared, non-packed, argument-less user
/// record or sum (`RecordRoot`), or exact `Dict(String, Int)` (`DictRoot`).
/// None has a projection.
pub(super) fn is_direct_rooted_view_whole_root(source: &witchy_syntax::ast::Type, records: &ScalarRecordFields) -> bool {
    is_direct_rooted_view_string(source)
        || matches!(source,
            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                if witchy_canonical_source::aggregates::is_record_root_payload(inner, records)
                    || witchy_canonical_source::aggregates::is_list_root_payload(inner)
                    || witchy_canonical_source::aggregates::is_dict_string_int_root_payload(inner)
                    || witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner))
}

/// A whole immutable `&String` view: carried as the retained String token
/// itself (`StringRoot`), so it corresponds to the plain String layout.
fn is_direct_rooted_view_string(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "String" && arguments.is_empty()))
}

/// A direct `&[T]` rooted slice view of an admitted element (`ListSlice`).
fn is_direct_rooted_view_slice(source: &witchy_syntax::ast::Type) -> bool {
    super::closure_owner_contract::rooted_slice_element(source).is_some()
}

/// The only non-scalar reference carriers the current canonical ABI can move
/// without manufacturing a place token.  Their source relation is still
/// authenticated by the retained access signature and an alias-result
/// contract; this predicate is intentionally shallow so a borrowed field,
/// aggregate, callable, or mutable handle cannot inherit the exception.
fn is_direct_shared_owned_reference(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(inner.unqualified(),
                witchy_syntax::ast::Type::Named(name, arguments)
                    if (matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty())
                        || (name == "List" && arguments.len() == 1
                            && matches!(arguments[0].unqualified(),
                                witchy_syntax::ast::Type::Named(element, element_arguments)
                                    if element == "Int" && element_arguments.is_empty()))))
}

fn is_direct_shared_reference(source: &witchy_syntax::ast::Type) -> bool {
    is_direct_shared_int(source) || is_direct_shared_owned_reference(source)
}

/// The physical aggregate ABI can store only these bounded reference leaves.
/// This deliberately proves no alias authority by itself: a declaration must
/// also pass `aggregate_reference_result_relation_is_checked` against its
/// pointer-bound access facts before this structural exception is usable.
fn is_aggregate_reference_carrier(source: &witchy_syntax::ast::Type) -> bool {
    match source.unqualified() {
        witchy_syntax::ast::Type::Tuple(items) => !items.is_empty()
            && items.iter().all(|item| {
                is_direct_shared_owned_reference(item) || is_direct_exclusive_reference(item)
            }),
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "List" && arguments.len() == 1 =>
        {
            is_direct_shared_owned_reference(&arguments[0])
        }
        // `Result` carries a tagged owned shell.  The only aggregate Result
        // admitted by this ABI is an `Ok` tuple of direct shared roots with a
        // String `Err` lane.  Its tag and every payload projection are then
        // checked below against the retained source relation product.
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "Result" && arguments.len() == 2 =>
        {
            is_shared_reference_tuple_carrier(&arguments[0])
                && is_string_source_type(&arguments[1])
        }
        // `Option(&Int)` has one rooted-view payload in `Some` and no payload
        // in `None`.  Its opaque aggregate owner is checked against the exact
        // source identity below; do not erase it into the linear Option(Int)
        // physical layout.
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "Option" && arguments.len() == 1 =>
        {
            witchy_canonical_source::aggregates::is_rooted_int_option_payload_carrier(&arguments[0], &BTreeMap::new())
        }
        _ => false,
    }
}

/// A non-packed aggregate of `&Int` views, or a named record with one direct
/// exact `&[Int]` field (see `witchy_canonical_source::aggregates::is_rooted_int_view_aggregate`). The opaque
/// aggregate family and constructor are revalidated independently; this
/// predicate only prevents source correspondence from erasing the exact
/// checked aggregate identity.
fn is_direct_rooted_view_aggregate_carrier(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    witchy_canonical_source::aggregates::is_rooted_int_view_aggregate(source, records)
}

fn is_shared_reference_tuple_carrier(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Tuple(items)
        if !items.is_empty() && items.iter().all(is_direct_shared_owned_reference))
}

fn is_string_source_type(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "String" && arguments.is_empty())
}

fn is_direct_exclusive_referent(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(),
        witchy_syntax::ast::Type::Named(name, arguments)
            if arguments.is_empty() && matches!(name.as_str(), "String" | "Int"))
}

// This is a leaf prefilter only.  Projected return authority is minted below
// from the declaration's final field expression and its pointer-bound loan,
// never from this physical type predicate.
fn is_projected_exclusive_leaf(source: &witchy_syntax::ast::Type) -> bool {
    is_direct_exclusive_referent(source)
}

fn is_direct_exclusive_reference(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(_), inner)
            if is_direct_exclusive_referent(inner))
}

fn is_root_unique(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _))
}

fn direct_shared_int_matches(
    actual: super::ValueType,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let super::ValueType::Ref(layout) = actual else { return false };
    is_direct_shared_int(source)
        && layouts.resolve_nominal(layout).ok()
            .and_then(|physical| layouts.get(physical))
            .is_some_and(|descriptor| descriptor.kind() == &crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))
}

fn restore_source_abi_qualifiers(
    checked: &witchy_syntax::ast::Type,
    declaration: Option<&witchy_syntax::ast::Function>,
    records: &ScalarRecordFields,
) -> witchy_syntax::ast::Type {
    let mut restored = checked.clone();
    let (witchy_syntax::ast::Type::Fn(parameters, result, conventions, _), Some(declaration)) =
        (&mut restored, declaration)
    else {
        return restored;
    };
    let projected = projected_exclusive_return_source(declaration, records);
    for (index, (parameter, declared)) in parameters.iter_mut().zip(&declaration.params).enumerate() {
        if let Some(ty) = &declared.ty
            && (is_direct_shared_reference(ty)
                || is_direct_rooted_view_int(ty) || is_direct_rooted_view_slice(ty)
                || is_direct_rooted_view_whole_root(ty, records)
                || is_direct_rooted_view_aggregate_carrier(ty, records)
                || (matches!(declared.convention,
                    witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
                    && is_direct_exclusive_reference(ty))
                || is_direct_callable_writeback_carrier(ty)
                || projected.is_some_and(|projected| projected.parameter == index)
                || is_root_unique(ty))
        {
            *parameter = ty.clone();
        }
        // The closed callable signature records the source default `Let`.
        // Only this separately authenticated field-carrier has the physical
        // checked write-back ABI convention; other source Lets stay Lets.
        if projected.is_some_and(|projected| projected.parameter == index) {
            conventions[index] = witchy_syntax::ast::Convention::Var;
        }
    }
    if let Some(declared) = &declaration.ret
        && (is_direct_shared_reference(declared)
            || is_direct_rooted_view_int(declared) || is_direct_rooted_view_slice(declared)
            || is_direct_rooted_view_whole_root(declared, records)
            || is_direct_exclusive_reference(declared)
            || is_aggregate_reference_carrier(declared)
            || is_direct_rooted_view_aggregate_carrier(declared, records)
            || is_root_unique(declared))
    {
        **result = declared.clone();
    }
    restored
}

/// A generated trait relay may return the fixed projected scalar selected by
/// its concrete implementation rather than spelling that field in its own
/// body. The relay remains admissible only when all three pointer-bound access
/// products agree: the relay declaration, its exact body call, and the
/// selected concrete callee. This is a relation check, never a trait-shape or
/// layout inference.
fn authenticated_projected_scalar_relay_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    module: &witchy_syntax::ast::Module,
    access_facts: &dyn witchy_canonical_source::source::CheckedAccessQuery,
    records: &ScalarRecordFields,
) -> bool {
    let [witchy_syntax::ast::Stmt::Expr(call @ witchy_syntax::ast::Expr::Call { name, args })] = declaration.body.stmts.as_slice() else {
        return false;
    };
    let [witchy_syntax::ast::Expr::Var(receiver), witchy_syntax::ast::Expr::Var(owner)] = args.as_slice() else {
        return false;
    };
    let [receiver_parameter, owner_parameter] = declaration.params.as_slice() else {
        return false;
    };
    if receiver != &receiver_parameter.name
        || owner != &owner_parameter.name
        || owner_parameter.ty.is_none()
    {
        return false;
    }
    let Some(callee) = module.items.iter().find_map(|item| match item {
        witchy_syntax::ast::Item::Function(candidate) if candidate.name == *name => Some(candidate),
        _ => None,
    }) else {
        return false;
    };
    if callee.ret.is_none()
        || !projected_exclusive_return_source(callee, records)
            .is_some_and(|projected| projected.parameter == 1)
    {
        return false;
    }
    let (Some(result), Some(caller), Some(call_contract), Some(callee_contract)) = (
        declaration.ret.as_ref(),
        access_facts.declaration(&declaration.name),
        access_facts.call_at(module, call),
        access_facts.declaration(name),
    ) else {
        return false;
    };
    caller.verify_exact(call_contract).is_ok()
        && callee_contract.verify_exact(call_contract).is_ok()
        && direct_exclusive_reference_parts(result).is_some_and(|(_, leaf)| {
            matches!(leaf.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty())
        })
}

/// Reinstall the physical root lane for the one projected-scalar relay only
/// after proving the exact generated body call and its selected concrete
/// callee.  This deliberately does not infer a field from a trait shape,
/// result layout, or convention: the pointer-bound caller/call/callee access
/// products and the callee's final field witness must all agree.
fn restore_authenticated_projected_scalar_relay_abi(
    restored: &mut witchy_syntax::ast::Type,
    declaration: Option<&witchy_syntax::ast::Function>,
    module: &witchy_syntax::ast::Module,
    access_facts: &dyn witchy_canonical_source::source::CheckedAccessQuery,
    records: &ScalarRecordFields,
) {
    let (witchy_syntax::ast::Type::Fn(parameters, result, conventions, _), Some(declaration)) =
        (restored, declaration)
    else { return };
    let [witchy_syntax::ast::Stmt::Expr(call @ witchy_syntax::ast::Expr::Call { name, args })] = declaration.body.stmts.as_slice() else {
        return;
    };
    let [witchy_syntax::ast::Expr::Var(receiver), witchy_syntax::ast::Expr::Var(owner)] = args.as_slice() else { return };
    let [receiver_parameter, owner_parameter] = declaration.params.as_slice() else { return };
    if receiver != &receiver_parameter.name || owner != &owner_parameter.name { return; }
    let Some(callee) = module.items.iter().find_map(|item| match item {
        witchy_syntax::ast::Item::Function(candidate) if candidate.name == *name => Some(candidate),
        _ => None,
    }) else { return };
    let Some(_callee_result) = callee.ret.as_ref() else { return };
    let Some(projected) = projected_exclusive_return_source(callee, records) else { return };
    if projected.parameter != 1 { return; }
    let (Some(caller), Some(call_contract), Some(callee_contract)) = (
        access_facts.declaration(&declaration.name),
        access_facts.call_at(module, call),
        access_facts.declaration(name),
    ) else { return };
    if caller.verify_exact(call_contract).is_err() || callee_contract.verify_exact(call_contract).is_err() {
        return;
    }
    if !direct_exclusive_reference_parts(result)
        .is_some_and(|(_, leaf)| matches!(leaf.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
            if name == "Int" && arguments.is_empty()))
    {
        return;
    }
    let Some(owner_type) = owner_parameter.ty.as_ref() else { return };
    if parameters.len() != declaration.params.len() || conventions.len() != parameters.len() { return; }
    parameters[1] = owner_type.clone();
    conventions[1] = witchy_syntax::ast::Convention::Var;
}

fn candidate_signature_matches_source_inner(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    if !source_abi_qualifiers_supported(source, records) { return false; }
    let key = (actual.id, source.unqualified().clone());
    match context.enter_callable(key.clone()) {
        CorrespondenceVisit::Active | CorrespondenceVisit::Completed => return true,
        CorrespondenceVisit::Limit => return false,
        CorrespondenceVisit::New => {}
    }
    if actual.parameters.len() != parameters.len() || actual.contract.parameters.len() != parameters.len() {
        context.finish_callable(key, false);
        return false;
    }
    let convention_at = |index| conventions.get(index).copied().unwrap_or_default();
    if !actual.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        (matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own)
                && (ordinary_own_source_type_supported(source)
                    || matches!(actual, super::ValueType::Ref(_) | super::ValueType::Gc(_))))
        && (direct_shared_int_matches(*actual, source, layouts)
            || value_type_matches_source(*actual, source, layouts, signatures, context, records, closure_layouts))
    }) {
        context.finish_callable(key, false);
        return false;
    }
    if !actual.contract.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        if !(matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own)
                && (ordinary_own_source_type_supported(source)
                    || matches!(actual.value_type, super::ValueType::Ref(_) | super::ValueType::Gc(_))))
            || !(direct_shared_int_matches(actual.value_type, source, layouts)
                || value_type_matches_source(actual.value_type, source, layouts, signatures, context, records, closure_layouts)) { return false; }
        if direct_shared_int_matches(actual.value_type, source, layouts) {
            return actual.ownership == super::ParameterOwnership::Shared;
        }
        if is_direct_exclusive_reference(source) {
            // The hidden lane is authenticated below against this exact plain
            // declaration parameter. An `own` direct handle consumes the
            // handle but still writes its referent replacement back through
            // that lane. No `var` alias, projection, or callable ABI can
            // borrow this exception.
            return matches!(convention,
                witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
                && actual.ownership == super::ParameterOwnership::WriteBack;
        }
        if is_direct_shared_owned_reference(source) {
            // The checked reference is already a linear-memory RC token.  It
            // is transferred as a value token, while an aliasing result below
            // records the retained source relation instead of relabeling this
            // ordinary owner as an untracked shared allocation.
            // An authenticated aggregate result differs narrowly: lowering
            // must clone every source root into a distinct aggregate field,
            // so the callee receives an explicit shared token to consume or
            // release.  `aggregate_reference_result_relation_is_checked`
            // separately proves the source relation product; a matching Ref
            // layout or parameter spelling is never sufficient.
            return actual.ownership == if is_aggregate_reference_carrier(result) {
                super::ParameterOwnership::Shared
            } else {
                super::ParameterOwnership::Value
            };
        }
        if matches!(source, witchy_syntax::ast::Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique, _
        )) && convention == witchy_syntax::ast::Convention::Own {
            // Root `unique` is a source-level affine fact. Managed roots
            // carry it in SafeWIR's ownership token, while an inline scalar
            // has no addressable storage and therefore retains its ordinary
            // value lane. Keep the distinction in the candidate witness:
            // accepting `Value` is confined to the scalar representations
            // which cannot carry or erase a managed ownership token.
            return root_unique_parameter_ownership_matches(actual.value_type, actual.ownership);
        }
        match (actual.value_type, convention) {
            // `Borrow` is the explicitly written `let` convention; `Let` is the
            // default one. Both are non-owning reads of the same GC-carried
            // value, so they take the same Shared ownership. Without `Borrow`
            // here a `let value: &Int` parameter fell through to the `Value`
            // arm and its signature was refused as a mismatch.
            (super::ValueType::Gc(_), witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            // `var view: &T` replaces the caller's immutable GC carrier by
            // write-back; the viewed value is never mutated through it.
            // The replacement lane is authenticated below as the exact same
            // source type, so the lane cannot change the carrier's family.
            (super::ValueType::Gc(_), witchy_syntax::ast::Convention::Var) => actual.ownership == super::ParameterOwnership::WriteBack,
            (super::ValueType::Gc(_), _) => false,
            (super::ValueType::Ref(_), witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            (super::ValueType::Ref(_), witchy_syntax::ast::Convention::Var)
                if matches!(source, witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _)) =>
                    actual.ownership == super::ParameterOwnership::UniqueWriteBack,
            (_, witchy_syntax::ast::Convention::Var) => actual.ownership == super::ParameterOwnership::WriteBack,
            (_, witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Value),
            (super::ValueType::Ref(_), _) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            _ => matches!(actual.ownership, super::ParameterOwnership::Value),
        }
    }) {
        context.finish_callable(key, false);
        return false;
    }
    let result_is_unit = is_unit_source_type(result);
    // The Result wrapper has the ordinary physical layout. Its authenticated
    // Unique contract above proves the exact declared Ok carrier, so only this
    // bounded case compares the physical projection rather than recursively
    // erasing nested qualifiers during correspondence.
    let physical_result = witchy_syntax::ast::direct_unique_result_ok_collection_physical_type(result);
    let ordinary_results = usize::from(!result_is_unit);
    let write_back_parameters = parameters.iter().enumerate()
        .filter(|(index, _)| matches!(convention_at(*index), witchy_syntax::ast::Convention::Var))
        .collect::<Vec<_>>();
    let direct_exclusive_parameters = parameters.iter().enumerate()
        .filter(|(index, parameter)| {
            matches!(convention_at(*index),
                witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
                && is_direct_exclusive_reference(parameter)
        })
        .collect::<Vec<_>>();
    // The only extra ordinary ABI lane is the authenticated Bool selector for
    // the bounded two-root direct result. It precedes every hidden write-back
    // lane, so no generic arity arithmetic can mistake it for a replacement.
    let disjunctive_selector_lanes = usize::from(
        direct_exclusive_parameters.len() == 2
            && direct_exclusive_reference_parts(result).is_some(),
    );
    let abi_ordinary_results = ordinary_results + disjunctive_selector_lanes;
    let source_unique_result = matches!(result,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _))
        || witchy_syntax::ast::is_direct_unique_result_ok_collection(result);
    let unique_results_match = actual.contract.results.iter().enumerate().all(|(index, contract)| {
        let unique_parameter = index.checked_sub(abi_ordinary_results)
            .and_then(|offset| write_back_parameters.get(offset))
            .is_some_and(|(parameter, _)| actual.contract.parameters[*parameter].ownership == super::ParameterOwnership::UniqueWriteBack);
        let expected_unique = (index == 0 && !result_is_unit && source_unique_result
            && actual.results.get(index).is_some_and(root_unique_result_requires_token))
            || unique_parameter;
        expected_unique == matches!(contract, super::ResultContract::Unique { .. })
    });
    let direct_alias_matches = direct_shared_alias_result_matches(
        actual,
        parameters,
        result,
        layouts,
    );
    let direct_exclusive_alias_matches = direct_exclusive_alias_result_matches(
        actual,
        parameters,
        result,
        conventions,
        layouts,
    );
    // A checked `&mut Record.fixed_int` has no physical Int address to pass
    // around.  The candidate carries the authenticated record root instead;
    // its only readable/writable child remains the exact result-contract
    // projection, independently bound by the declaration's loan relation.
    let projected_scalar_result_matches = projected_exclusive_signature_parameter(
        parameters,
        result,
        conventions,
    ).is_some_and(|parameter| {
        direct_exclusive_reference_parts(result)
            .is_some_and(|(_, leaf)| matches!(leaf.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
            && matches!(
                (actual.results.first(), actual.contract.results.first(), actual.parameters.get(parameter)),
                (
                    Some(super::ValueType::Ref(root)),
                    Some(super::ResultContract::AliasesWriteBackParameter {
                        parameter: result_parameter,
                        layout,
                        projection: Some(_),
                    }),
                    Some(super::ValueType::Ref(parameter_root)),
                ) if *root == *layout && *root == *parameter_root
                    && usize::try_from(*result_parameter).ok() == Some(parameter)
            )
    });
    let ordinary_result_matches = result_is_unit
        || projected_scalar_result_matches
        || value_type_matches_source(
            actual.results[0],
            physical_result.as_ref().unwrap_or(result),
            layouts,
            signatures,
            context,
            records,
            closure_layouts,
        );
    let matched = unique_results_match && direct_alias_matches && direct_exclusive_alias_matches
        && aggregate_reference_result_contract_matches(actual, result, records)
        && actual.results.len() == abi_ordinary_results + write_back_parameters.len() + direct_exclusive_parameters.len()
        && ordinary_result_matches
        && write_back_parameters.iter().enumerate().all(|(offset, (parameter, source))| {
            value_type_matches_source(actual.results[abi_ordinary_results + offset], source, layouts, signatures, context, records, closure_layouts)
                && matches!(actual.contract.parameters[*parameter].ownership, super::ParameterOwnership::WriteBack | super::ParameterOwnership::UniqueWriteBack)
        })
        && direct_exclusive_parameters.iter().enumerate().all(|(offset, (parameter, source))| {
            let result_index = abi_ordinary_results + write_back_parameters.len() + offset;
            value_type_matches_source(actual.results[result_index], source, layouts, signatures, context, records, closure_layouts)
                && actual.contract.parameters[*parameter].ownership == super::ParameterOwnership::WriteBack
                && direct_exclusive_result_contract_matches(
                    actual.contract.results.get(result_index), actual.results[result_index],
                )
        })
        && actual.contract.parameter_effects.iter().all(|(parameter, effects)| {
            let read_only = effects.iter().all(|effect| matches!(effect, super::MemoryEffect::Read | super::MemoryEffect::Retain | super::MemoryEffect::Release));
            // A source `var` GC carrier is replaced whole through its hidden
            // lane; it is immutable, so nothing may be written through it.
            let gc_var = matches!(actual.parameters.get(*parameter as usize), Some(super::ValueType::Gc(_)))
                && convention_at(*parameter as usize) == witchy_syntax::ast::Convention::Var;
            matches!(actual.parameters.get(*parameter as usize), Some(super::ValueType::Ref(_) | super::ValueType::Gc(_)))
                && (!gc_var && matches!(actual.contract.parameters.get(*parameter as usize).map(|parameter| parameter.ownership), Some(super::ParameterOwnership::WriteBack | super::ParameterOwnership::UniqueWriteBack))
                    || read_only)
        });
    context.finish_callable(key, matched);
    matched
}

fn direct_exclusive_result_contract_matches(
    contract: Option<&super::ResultContract>,
    value_type: super::ValueType,
) -> bool {
    match (contract, value_type) {
        (Some(super::ResultContract::Shared { layout }), super::ValueType::Ref(actual)) => *layout == actual,
        (Some(super::ResultContract::Value(actual)), value_type) => *actual == value_type,
        _ => false,
    }
}

/// The aggregate shell is an independently owned, shared result token.  The
/// leaf-to-owner proof lives in the retained source `BorrowRelation` product;
/// this SafeWIR-side check merely refuses to let that proven carrier degrade
/// into a fresh/value result with the same physical layout.
fn aggregate_reference_result_contract_matches(
    actual: &super::FunctionSignature,
    result: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    if !(is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records)) {
        return true;
    }
    if is_direct_rooted_view_aggregate_carrier(result, records) {
        return matches!(
            (actual.results.first(), actual.contract.results.first()),
            (Some(super::ValueType::Gc(owner)), Some(super::ResultContract::Value(super::ValueType::Gc(contract_owner))))
                if owner == contract_owner
        );
    }
    matches!(
        (actual.results.first(), actual.contract.results.first()),
        (Some(super::ValueType::Ref(value_layout)), Some(super::ResultContract::Shared { layout }))
            if value_layout == layout
    )
}

/// An inline scalar has no addressable storage and no SafeWIR ownership token.
/// This is deliberately the same narrow physical family canonical lowering
/// permits for a root `unique` result. All managed values must still carry the
/// explicit `Unique` parameter/result contract.
const fn inline_scalar_value_type(value: super::ValueType) -> bool {
    matches!(
        value,
        super::ValueType::Int
            | super::ValueType::Float
            | super::ValueType::Bool
            | super::ValueType::Unit
            | super::ValueType::Size
            | super::ValueType::V128
    )
}

/// The source witness cannot let an inline scalar impersonate a managed owner,
/// nor a managed (`Ref` or `Gc`) owner erase its token.
fn root_unique_parameter_ownership_matches(
    value: super::ValueType,
    ownership: super::ParameterOwnership,
) -> bool {
    match value {
        super::ValueType::Ref(_) | super::ValueType::Gc(_) => {
            ownership == super::ParameterOwnership::Unique
        }
        value if inline_scalar_value_type(value) => ownership == super::ParameterOwnership::Value,
        _ => false,
    }
}

fn root_unique_result_requires_token(value: &super::ValueType) -> bool {
    matches!(value, super::ValueType::Ref(_) | super::ValueType::Gc(_))
}

/// A direct shared result must retain the exact one checked input relation.
/// This is intentionally narrower than general loan lowering: it accepts only
/// the direct, whole-value carriers admitted above.  A projected/nested or
/// ambiguous result stays unsupported instead of being made to look like an
/// ordinary owned result.
fn direct_shared_alias_result_matches(
    actual: &super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Some((lifetime, referent)) = direct_shared_reference_parts(result) else {
        return !is_direct_shared_reference(result);
    };
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        direct_shared_reference_parts(parameter).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    let [parameter] = candidates.as_slice() else { return false };
    let Some(super::ResultContract::AliasesParameter { parameter: actual_parameter, layout }) =
        actual.contract.results.first()
    else {
        return false;
    };
    usize::try_from(*actual_parameter).ok() == Some(*parameter)
        && matches!(actual.results.first(), Some(super::ValueType::Ref(actual_layout)) if actual_layout == layout)
        && layouts.resolve_nominal(*layout).is_ok()
}

fn direct_shared_reference_parts(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type)> {
    let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(lifetime), referent) = source else {
        return None;
    };
    is_direct_shared_reference(source).then_some((lifetime, referent))
}

fn direct_exclusive_reference_parts(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type)> {
    let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(lifetime), referent) = source else {
        return None;
    };
    is_direct_exclusive_referent(referent).then_some((lifetime, referent))
}

/// The direct result is the same checked loan relation as one exact direct
/// exclusive input. The distinct hidden write-back lane is checked separately
/// below; this ordinary lane may never be forged from matching physical refs.
fn direct_exclusive_alias_result_matches(
    actual: &super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Some((lifetime, referent)) = direct_exclusive_reference_parts(result) else {
        return true;
    };
    if !is_projected_exclusive_leaf(referent) {
        return false;
    }
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        direct_exclusive_reference_parts(parameter).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime
                && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    if let [parameter] = candidates.as_slice() {
        let Some(super::ResultContract::AliasesWriteBackParameter {
            parameter: actual_parameter,
            layout,
            projection: None,
        }) = actual.contract.results.first()
        else {
            return false;
        };
        return usize::try_from(*actual_parameter).ok() == Some(*parameter)
            && actual.contract.parameters.get(*parameter).is_some_and(|contract| {
                contract.ownership == super::ParameterOwnership::WriteBack
                    && contract.value_type == super::ValueType::Ref(*layout)
            })
            && matches!(actual.results.first(), Some(super::ValueType::Ref(actual_layout)) if actual_layout == layout)
            && layouts.resolve_nominal(*layout).is_ok();
    }
    if candidates.len() >= 2 {
        let expected = candidates
            .iter()
            .map(|parameter| u32::try_from(*parameter).ok())
            .collect::<Option<Vec<_>>>();
        let Some(expected) = expected else { return false };
        let Some(super::ResultContract::AliasesOneOfWriteBackParameters {
            parameters: actual_parameters,
            layout,
        }) = actual.contract.results.first()
        else {
            return false;
        };
        return expected.len() == 2
            && actual_parameters == &expected
            && matches!(actual.contract.results.get(1), Some(super::ResultContract::DisjunctiveWriteBackSelector {
                carrier_result: 0,
                parameters,
                selector_parameter,
            }) if parameters == &expected
                && matches!(actual.results.get(1), Some(super::ValueType::Bool))
                && matches!(actual.contract.parameters.get(*selector_parameter as usize), Some(super::ParameterContract {
                    value_type: super::ValueType::Bool,
                    ownership: super::ParameterOwnership::Value,
                })))
            && actual.contract.parameters.iter().enumerate().all(|(index, contract)| {
                !expected.contains(&(index as u32))
                    || (contract.ownership == super::ParameterOwnership::WriteBack
                        && contract.value_type == super::ValueType::Ref(*layout))
            })
            && matches!(actual.results.first(), Some(super::ValueType::Ref(actual_layout)) if actual_layout == layout)
            && layouts.resolve_nominal(*layout).is_ok();
    }
    let Some(parameter) = projected_exclusive_signature_parameter(parameters, result, conventions) else {
        return false;
    };
    let Some(super::ResultContract::AliasesWriteBackParameter {
        parameter: actual_parameter,
        layout,
        projection: Some(_),
    }) =
        actual.contract.results.first()
    else {
        return false;
    };
    usize::try_from(*actual_parameter).ok() == Some(parameter)
        && actual.contract.parameters.get(parameter).is_some_and(|contract| {
            contract.ownership == super::ParameterOwnership::WriteBack
                && matches!(contract.value_type, super::ValueType::Ref(_))
        })
        && matches!(actual.results.first(), Some(super::ValueType::Ref(actual_layout)) if actual_layout == layout)
        && layouts.resolve_nominal(*layout).is_ok()
}

/// Type-level prefilter only. `direct_exclusive_result_relation_is_checked`
/// binds this candidate to the declaration's exact field relation; a function
/// type cannot itself name a record field.
fn projected_exclusive_signature_parameter(
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
) -> Option<usize> {
    let (lifetime, result_inner) = direct_exclusive_reference_parts(result)?;
    if !is_projected_exclusive_leaf(result_inner) {
        return None;
    }
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(candidate_lifetime), owner) = parameter else {
            return None;
        };
        (conventions.get(index).copied().unwrap_or_default() == witchy_syntax::ast::Convention::Var
            && candidate_lifetime == lifetime
            && matches!(owner.unqualified(), witchy_syntax::ast::Type::Named(_, arguments) if arguments.is_empty())
            && !is_direct_exclusive_referent(owner))
            .then_some(index)
    }).collect::<Vec<_>>();
    let [parameter] = candidates.as_slice() else { return None };
    Some(*parameter)
}

/// A source signature that returns a direct shared reference has no safe
/// syntax-only fallback.  Admission must be grounded in both the closed
/// declaration and the pointer-bound access product for that declaration.
fn direct_reference_binding_is_checked(
    source_signature: &witchy_syntax::ast::Type,
    declaration: Option<&witchy_syntax::ast::Function>,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    loan_facts_available: bool,
    records: &ScalarRecordFields,
) -> bool {
    let witchy_syntax::ast::Type::Fn(_, checked_result, _, _) = source_signature.unqualified() else {
        return false;
    };
    // Closed callable signatures may erase the reference leaves of an owned
    // aggregate shell.  The declaration is the source contract whose retained
    // access product must authorize those leaves; never let the erased closed
    // signature turn a missing Result/tuple/list relation into a value-only
    // fallback.
    let result = declaration
        .and_then(|declaration| declaration.ret.as_ref())
        .unwrap_or(checked_result);
    let shared_ok = !is_direct_shared_reference(result)
        || declaration.is_some_and(|declaration| {
            direct_shared_reference_relation_is_checked(declaration, access)
        });
    let exclusive_result_ok = !is_direct_exclusive_reference(result)
        || declaration.is_some_and(|declaration| {
            direct_exclusive_result_relation_is_checked(declaration, access, records)
        });
    let aggregate_result_ok = !(is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records))
        || declaration.is_some_and(|declaration| {
            aggregate_reference_result_relation_is_checked(declaration, access, records)
        });
    shared_ok && exclusive_result_ok && aggregate_result_ok && declaration.map_or_else(
        || !source_signature_contains_direct_exclusive_reference(source_signature),
        |declaration| direct_exclusive_reference_relation_is_checked(declaration, access, loan_facts_available),
    )
}

/// Authenticate the bounded aggregate result carrier from the checker-owned
/// relation product.  A tuple has one exact output slot per reference leaf;
/// a homogeneous shared list has one `AnyIndex` slot whose complete owner set
/// is explicit.  No aggregate layout, field name, or matching physical Ref is
/// ever treated as an owner relation.
fn aggregate_reference_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    records: &ScalarRecordFields,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return false };
    let Some(access) = access else { return false };
    if is_option_rooted_int_view_carrier(result) {
        return option_rooted_int_view_result_relation_is_checked(declaration, access);
    }
    if is_direct_rooted_view_aggregate_carrier(result, records) {
        // Structural and builtin aggregates retain the complete immediate
        // ownership tree in the checker product. A nominal type carries no
        // expanded field tree here; its exact source identity and each field
        // storage class are instead bound by the opaque-owner family.
        // A builtin or generic shell whose only views sit inside such a
        // nominal (`Option(Inner)`) has no structural view, so the checker
        // tracks no ownership state for it (its output is then necessarily
        // absent; the second conjunct only restates that). The same family
        // binding covers it. A structurally visible view always requires the
        // exact tree below.
        return matches!(result.unqualified(), witchy_syntax::ast::Type::Named(_, arguments)
            if arguments.is_empty())
            || (!witchy_canonical_source::access::type_requires_ownership_state(result)
                && access.result().ownership_output().is_none())
            || witchy_canonical_source::access::ownership_state_class(result).ok().flatten().as_ref()
                == access.result().ownership_output();
    }
    if let witchy_syntax::ast::Type::Named(name, arguments) = result.unqualified()
        && name == "Result"
        && arguments.len() == 2
    {
        let (ok, error) = (&arguments[0], &arguments[1]);
        // The first `Result` alternative is the exact `Ok` tag.  The access
        // catalog represents builtin positional constructor payloads by their
        // constructor field index, so both the tag and inner tuple slot must
        // be present in order.  A missing, swapped, or stale relation cannot
        // authorize this carrier.
        return is_string_source_type(error)
            && aggregate_tuple_reference_result_relation_is_checked(
                declaration,
                access,
                ok,
                &[witchy_canonical_source::access::LoanProjectionStep::Tuple(0)],
            );
    }
    match result.unqualified() {
        witchy_syntax::ast::Type::Tuple(_) => {
            aggregate_tuple_reference_result_relation_is_checked(declaration, access, result, &[])
        }
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "List" && arguments.len() == 1 =>
        {
            let Some((lifetime, referent, kind)) = aggregate_reference_leaf(&arguments[0]) else {
                return false;
            };
            if kind != witchy_canonical_source::access::BorrowKind::Shared {
                return false;
            }
            matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == kind
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps
                        == [witchy_canonical_source::access::LoanProjectionStep::AnyIndex]
                    && aggregate_relation_owners_match(declaration, relation.owners(), lifetime, referent, kind))
        }
        _ => false,
    }
}

/// `&Int` is an opaque rooted carrier rather than a lifetime-derived alias.
/// Consequently an `Option(&Int)` result has no borrow relation to inspect;
/// its checked access identity must instead retain the exact rooted `Some`
/// child. The owner-family and constructor checks independently authenticate
/// where that child came from, so parameter count never grants authority.
fn option_rooted_int_view_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: &witchy_canonical_source::access::AccessSignature,
) -> bool {
    declaration.ret.as_ref().is_some_and(|result| {
        is_option_rooted_int_view_carrier(result)
            && access.result().ownership_output().is_some_and(|class| {
                option_rooted_view_ownership_matches(result, class)
            })
    })
}

/// The checker's ownership tree for an `Option` view carrier mirrors its
/// source nesting exactly: each `Option` shell is one layout-dependent node
/// with a single payload child, and the innermost leaf is the authenticated
/// `&Int` owner. Any other child shape rejects.
fn option_rooted_view_ownership_matches(
    source: &witchy_syntax::ast::Type,
    class: &witchy_canonical_source::access::OwnershipStateClass,
) -> bool {
    use witchy_canonical_source::access::OwnershipStateClass;

    if is_direct_rooted_view_int(source) {
        return matches!(class, OwnershipStateClass::RootedViewCarrier);
    }
    let witchy_syntax::ast::Type::Named(name, arguments) = source.unqualified() else { return false };
    let (true, [payload]) = (name == "Option", arguments.as_slice()) else { return false };
    matches!(class, OwnershipStateClass::LayoutDependent { children }
        if matches!(children.as_slice(), [Some(child)] if option_rooted_view_ownership_matches(payload, child)))
}

/// `Option(&Int)`, or an `Option` of such a carrier nested to any depth.
fn is_option_rooted_int_view_carrier(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "Option" && matches!(arguments.as_slice(), [payload]
            if is_direct_rooted_view_int(payload) || is_option_rooted_int_view_carrier(payload)))
}

/// Match the complete, ordered leaf-to-owner relation product for one tuple
/// carrier.  `prefix` is empty for a bare tuple and `[Tuple(0)]` for the
/// builtin `Result` `Ok` payload.  It is supplied by the caller rather than
/// inferred from physical layout so a tagged Result cannot be mistaken for a
/// bare aggregate with the same reference fields.
fn aggregate_tuple_reference_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: &witchy_canonical_source::access::AccessSignature,
    payload: &witchy_syntax::ast::Type,
    prefix: &[witchy_canonical_source::access::LoanProjectionStep],
) -> bool {
    let witchy_syntax::ast::Type::Tuple(items) = payload.unqualified() else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| is_direct_shared_owned_reference(item) || is_direct_exclusive_reference(item))
        // The access product is itself the complete result authority for this
        // carrier.  Compare its full ordered multiset against the source
        // slots: accepting an `any` match here would let a duplicate relation
        // satisfy two slots, or let an unrelated relation survive.
        && access.borrow_relations().len() == items.len()
        && access.borrow_relations().iter().zip(items).enumerate().all(|(slot, (relation, item))| {
            let Some((lifetime, referent, kind)) = aggregate_reference_leaf(item) else {
                return false;
            };
            let [owner] = relation.owners() else { return false };
            let Some(parameter) = declaration.params.get(owner.position()) else {
                return false;
            };
            let mut expected_projection = prefix.to_vec();
            expected_projection.push(witchy_canonical_source::access::LoanProjectionStep::Tuple(slot));
            relation.lifetime() == lifetime
                && relation.kind() == kind
                && relation.storage_type().unqualified() == referent.unqualified()
                && relation.output_projection().steps == expected_projection
                && owner.input_projection().steps.is_empty()
                && parameter.ty.as_ref().and_then(aggregate_reference_leaf)
                    .is_some_and(|(candidate_lifetime, candidate_referent, candidate_kind)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                            && candidate_kind == kind
                    })
        })
}

/// Relation owner positions are source declarations, not type-shaped aliases.
/// The checker publishes them in declaration order; require a strictly
/// increasing distinct sequence so a duplicated or reordered owner cannot
/// manufacture a list carrier authority.
fn aggregate_relation_owners_match(
    declaration: &witchy_syntax::ast::Function,
    owners: &[witchy_canonical_source::access::BorrowOwnerRelation],
    lifetime: &str,
    referent: &witchy_syntax::ast::Type,
    kind: witchy_canonical_source::access::BorrowKind,
) -> bool {
    !owners.is_empty()
        && owners.windows(2).all(|pair| pair[0].position() < pair[1].position())
        && owners.iter().all(|owner| {
            owner.input_projection().steps.is_empty()
                && declaration.params.get(owner.position())
                    .and_then(|parameter| parameter.ty.as_ref())
                    .and_then(aggregate_reference_leaf)
                    .is_some_and(|(candidate_lifetime, candidate_referent, candidate_kind)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                            && candidate_kind == kind
                    })
        })
}

fn aggregate_reference_leaf(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type, witchy_canonical_source::access::BorrowKind)> {
    if let Some((lifetime, referent)) = direct_shared_reference_parts(source) {
        return Some((lifetime, referent, witchy_canonical_source::access::BorrowKind::Shared));
    }
    direct_exclusive_reference_parts(source)
        .map(|(lifetime, referent)| (lifetime, referent, witchy_canonical_source::access::BorrowKind::Exclusive))
}

/// A generic shared return may use the ordinary `AliasesParameter` contract
/// only when the source witness retains this exact monomorphized forwarding
/// identity.  Do not reconstruct it from the mangled function name or from a
/// matching physical `Ref` layout: either would let a different generic body
/// borrow the direct carrier's authority.
fn exact_generic_shared_relay_binding(
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    source_signature: &witchy_syntax::ast::Type,
    declaration: &witchy_syntax::ast::Function,
    candidate_signature: &super::FunctionSignature,
) -> bool {
    use witchy_syntax::ast::{Convention, Expr, Stmt, Type, TypeQual};

    let Some(identity) = source.generic_specialization(&declaration.name) else {
        return false;
    };
    let Some(template) = identity.template() else {
        return false;
    };
    let ([type_parameter], [Some(template_parameter)], Some(template_result), [concrete]) = (
        template.type_parameters(),
        template.parameters(),
        template.result(),
        identity.concrete_types(),
    ) else {
        return false;
    };
    let Some((parameter_lifetime, parameter_inner)) = direct_shared_reference_parts(template_parameter) else {
        return false;
    };
    let Some((result_lifetime, result_inner)) = direct_shared_reference_parts(template_result) else {
        return false;
    };
    if parameter_lifetime != result_lifetime
        || !matches!(parameter_inner.unqualified(), Type::Named(name, arguments)
            if name == type_parameter && arguments.is_empty())
        || !matches!(result_inner.unqualified(), Type::Named(name, arguments)
            if name == type_parameter && arguments.is_empty())
    {
        return false;
    }
    let concrete_reference = Type::Qualified(
        TypeQual::Borrow(parameter_lifetime.to_owned()),
        Box::new(concrete.clone()),
    );
    if !is_direct_shared_owned_reference(&concrete_reference) {
        return false;
    }
    let Type::Fn(parameters, result, conventions, _) = source_signature.unqualified() else {
        return false;
    };
    let convention = conventions.first().copied().unwrap_or(Convention::Let);
    let Some(parameter) = declaration.params.first() else {
        return false;
    };
    let ([super::ValueType::Ref(parameter_layout)], [super::ValueType::Ref(result_layout)]) = (
        candidate_signature.parameters.as_slice(),
        candidate_signature.results.as_slice(),
    ) else {
        return false;
    };
    let ([parameter_contract], [result_contract]) = (
        candidate_signature.contract.parameters.as_slice(),
        candidate_signature.contract.results.as_slice(),
    ) else {
        return false;
    };
    parameters.as_slice() == [concrete.clone()]
        && result.as_ref() == concrete
        && conventions.len() <= 1
        && convention == Convention::Let
        && declaration.params.len() == 1
        && parameter.convention == Convention::Let
        && parameter.ty.as_ref() == Some(&concrete_reference)
        && declaration.ret.as_ref() == Some(&concrete_reference)
        && matches!(declaration.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(value))] if value == &parameter.name)
        && *parameter_layout == *result_layout
        && parameter_contract.value_type == super::ValueType::Ref(*parameter_layout)
        && parameter_contract.ownership == super::ParameterOwnership::Value
        && matches!(
            result_contract,
            super::ResultContract::AliasesParameter { parameter: 0, layout }
                if *layout == *parameter_layout
        )
        && candidate_signature.contract.parameter_effects
            == BTreeMap::from([(
                0,
                vec![
                    super::MemoryEffect::Read,
                    super::MemoryEffect::Retain,
                    super::MemoryEffect::Release,
                ],
            )])
}

/// Identify only the specialization class whose retained template proves the
/// source `&'l a -> &'l a` relation. The closed source ABI below is physical
/// `[Concrete] -> Concrete`, so it cannot serve as this predicate itself.
fn generic_shared_relay_template_candidate(
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    name: &str,
) -> bool {
    use witchy_syntax::ast::Type;

    let Some(identity) = source.generic_specialization(name) else {
        return false;
    };
    let Some(template) = identity.template() else {
        return false;
    };
    let ([type_parameter], [Some(template_parameter)], Some(template_result), [concrete]) = (
        template.type_parameters(),
        template.parameters(),
        template.result(),
        identity.concrete_types(),
    ) else {
        return false;
    };
    let Some((parameter_lifetime, parameter_inner)) = direct_shared_reference_parts(template_parameter) else {
        return false;
    };
    let Some((result_lifetime, result_inner)) = direct_shared_reference_parts(template_result) else {
        return false;
    };
    parameter_lifetime == result_lifetime
        && matches!(parameter_inner.unqualified(), Type::Named(parameter, arguments)
            if parameter == type_parameter && arguments.is_empty())
        && matches!(result_inner.unqualified(), Type::Named(parameter, arguments)
            if parameter == type_parameter && arguments.is_empty())
        && is_direct_shared_owned_reference(&Type::Qualified(
            witchy_syntax::ast::TypeQual::Borrow(parameter_lifetime.to_owned()),
            Box::new(concrete.clone()),
        ))
}

fn source_signature_contains_direct_exclusive_reference(
    source_signature: &witchy_syntax::ast::Type,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source_signature.unqualified() else {
        return false;
    };
    is_direct_exclusive_reference(result) || parameters.iter().enumerate().any(|(index, parameter)| {
        matches!(conventions.get(index).copied().unwrap_or_default(),
            witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
            && is_direct_exclusive_reference(parameter)
    })
}

/// Exact checked declaration/access correspondence for the sole exclusive
/// carrier admitted in this phase.  The loan product must also be retained by
/// this exact source; its availability is intentionally not inferred from a
/// lowerer's reconstructed AST traversal.
fn direct_exclusive_reference_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    loan_facts_available: bool,
) -> bool {
    let direct = declaration.params.iter().enumerate().filter(|(_, parameter)| {
        matches!(parameter.convention,
            witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
            && parameter.ty.as_ref().is_some_and(is_direct_exclusive_reference)
    }).collect::<Vec<_>>();
    if direct.is_empty() {
        return true;
    }
    loan_facts_available && access.is_some_and(|access| {
        direct.iter().all(|(index, parameter)| {
            let expected_kind = match parameter.convention {
                witchy_syntax::ast::Convention::Let => witchy_canonical_source::access::AccessKind::ExclusiveWriteback,
                witchy_syntax::ast::Convention::Own => witchy_canonical_source::access::AccessKind::Consuming,
                _ => return false,
            };
            access.params().get(*index).is_some_and(|actual| {
                actual.ty() == parameter.ty.as_ref().expect("filtered above")
                    && actual.kind() == expected_kind
                    && actual.qualifiers().iter().any(|qualifier|
                        matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(_)))
            })
        })
    })
}

/// A direct exclusive result has an exact checked owner set in the
/// pointer-bound access product. This is the authority mint for the direct
/// carrier; matching Ref layouts or a merely present loan graph do not
/// suffice.  A multi-owner relation stays finite and explicit rather than
/// degrading to a shape-derived shared result.
fn direct_exclusive_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    records: &ScalarRecordFields,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return true };
    let Some((lifetime, referent)) = direct_exclusive_reference_parts(result) else {
        return true;
    };
    if !is_projected_exclusive_leaf(referent) {
        return false;
    }
    let candidates = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        parameter.ty.as_ref().and_then(direct_exclusive_reference_parts).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime
                && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    if let [owner] = candidates.as_slice() {
        return access.is_some_and(|access| {
        access.result().ty() == result
            && access.result().qualifiers().iter().any(|qualifier| {
                matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
            })
            && matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps.is_empty()
                    && matches!(relation.owners(), [candidate]
                        if candidate.position() == *owner && candidate.input_projection().steps.is_empty()))
        });
    }

    if candidates.len() >= 2 {
        return access.is_some_and(|access| {
            access.result().ty() == result
                && access.result().qualifiers().iter().any(|qualifier| {
                    matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
                })
                && matches!(access.borrow_relations(), [relation]
                    if relation.lifetime() == lifetime
                        && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                        && relation.storage_type().unqualified() == referent.unqualified()
                        && relation.output_projection().steps.is_empty()
                        && relation.owners().iter().map(|owner| owner.position()).eq(candidates.iter().copied())
                        && relation.owners().iter().all(|owner| owner.input_projection().steps.is_empty()))
        });
    }

    let Some(projected) = projected_exclusive_return_source(declaration, records) else { return false };
    access.is_some_and(|access| {
        access.result().ty() == result
            && access.result().qualifiers().iter().any(|qualifier| {
                matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
            })
            && matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps.is_empty()
                    && matches!(relation.owners(), [candidate]
                        if candidate.position() == projected.parameter
                            && candidate.input_projection().steps.is_empty()))
    })
}

/// Consult the type-checker-owned access product before accepting the one
/// source-level alias contract the direct carrier can express.  This is not a
/// lowering-time reconstruction: the product is pointer-bound to the exact
/// typed module and was already used to reject stale or missing facts.
fn direct_shared_reference_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return true };
    let Some((lifetime, referent)) = direct_shared_reference_parts(result) else {
        // A shared-reference argument which is not returned does not create an
        // output alias. Its ABI contract is checked by the ordinary parameter
        // correspondence below; no checked borrow relation is required here.
        return true;
    };
    let candidates = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        parameter.ty.as_ref().and_then(direct_shared_reference_parts).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    let [owner] = candidates.as_slice() else { return false };
    access.is_some_and(|access| access.borrow_relations().iter().any(|relation| {
        relation.lifetime() == lifetime
            && relation.kind() == witchy_canonical_source::access::BorrowKind::Shared
            && relation.storage_type().unqualified() == referent.unqualified()
            && relation.output_projection().steps.is_empty()
            && matches!(relation.owners(), [candidate]
                if candidate.position() == *owner && candidate.input_projection().steps.is_empty())
    }))
}

fn ordinary_own_source_type_supported(source: &witchy_syntax::ast::Type) -> bool {
    let source = match source {
        // The outer qualifier is a checked source ownership fact.  Its inner
        // physical lane is still the ordinary own ABI.  Do not recurse
        // through nested qualifiers: an element-level ownership token is not
        // part of this scalar exception.
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, inner) => inner.as_ref(),
        _ => source,
    };
    ordinary_unqualified_own_source_type_supported(source)
}

fn ordinary_unqualified_own_source_type_supported(source: &witchy_syntax::ast::Type) -> bool {
    if is_unit_source_type(source) {
        return true;
    }
    match source {
        // This admits the convention, not a signature or pointer claim:
        // value_type_matches_source independently checks the closure record
        // and its complete registered callable signature.
        witchy_syntax::ast::Type::Fn(..) => true,
        // A capability token is a scalar-like value: copied on pass, never
        // released, so every convention carries it. `Dir` names its rights in
        // type arguments, which change no ABI fact about the token.
        witchy_syntax::ast::Type::Named(name, _) if matches!(name.as_str(), "Dir" | "File" | "Net") => true,
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => {
            matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String" | "Bytes" | "Console" | "Env" | "Listener" | "Socket")
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            ordinary_unqualified_own_source_type_supported(&arguments[0])
        }
        _ => false,
    }
}

/// The canonical `()` spelling is structurally an empty tuple.  Keep legacy
/// names accepted at this compatibility boundary, but never require them for
/// a zero-lane result or parameter correspondence.
fn is_unit_source_type(source: &witchy_syntax::ast::Type) -> bool {
    match source.unqualified() {
        witchy_syntax::ast::Type::Named(name, arguments) => {
            (name == "Nil" || name == "Unit") && arguments.is_empty()
        }
        witchy_syntax::ast::Type::Tuple(items) => items.is_empty(),
        _ => false,
    }
}

pub(super) fn value_type_matches_source(
    actual: super::ValueType,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    if is_unit_source_type(source) {
        return actual == super::ValueType::Unit;
    }
    // A rooted scalar or slice view aggregate is an opaque owner family, including when it
    // enters as a parameter. Its ordinary record fields may happen to look
    // like the erased referents, but that linear layout cannot authenticate
    // the retained roots. The Gc arm below binds the exact source identity.
    if is_direct_rooted_view_aggregate_carrier(source, records)
        && !matches!(actual, super::ValueType::Gc(_)) {
        return false;
    }
    if let super::ValueType::Gc(id) = actual {
        let Some(family) = context.owner_families.iter().find(|f| f.owner_type == id || f.list_type == id).cloned() else { return false };
        if let super::closure_owner_contract::OwnerFamilySource::SharedReference(expected) = &family.source {
            // `List(&Int)` is the `&Int` family's owner list.
            if id == family.list_type {
                return is_direct_rooted_view_int(expected)
                    && matches!(source.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                        if name == "List" && arguments.len() == 1 && is_direct_rooted_view_int(&arguments[0]));
            }
            return id == family.owner_type
                && ((is_direct_rooted_view_int(expected) && is_direct_rooted_view_int(source))
                    || (is_direct_rooted_view_slice(expected)
                        && super::closure_owner_contract::rooted_slice_element(expected)
                            == super::closure_owner_contract::rooted_slice_element(source)));
        }
        if let super::closure_owner_contract::OwnerFamilySource::Aggregate(expected) = &family.source {
            let aggregate = if id == family.list_type {
                match source.unqualified() {
                    witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => &arguments[0],
                    _ => return false,
                }
            } else {
                source
            };
            return witchy_canonical_source::aggregates::canonical_owner_aggregate_identity(aggregate, records)
                .is_some_and(|normalized| &normalized == expected);
        }
        let callable = if id == family.list_type {
            match source.unqualified() {
                witchy_syntax::ast::Type::Named(name, args) if name == "List" && args.len() == 1 => &args[0],
                _ => return false,
            }
        } else { source };
        let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = callable.unqualified() else { return false };
        let Some(declaration) = signatures.iter().find(|s| s.id == family.declaration_signature) else { return false };
        return candidate_signature_matches_source_inner(declaration, callable, parameters, result, conventions,
            layouts, signatures, context, records, closure_layouts);
    }
    if let Some(representation) = witchy_canonical_source::capabilities::representation(source, records) {
        use witchy_canonical_source::capabilities::{CapabilityKind, CapabilityRepresentation};
        let kind = match representation {
            CapabilityRepresentation::Direct(CapabilityKind::Console) | CapabilityRepresentation::Nullable(CapabilityKind::Console) => super::CapabilityKindId::new(0),
            CapabilityRepresentation::Direct(CapabilityKind::Dir) | CapabilityRepresentation::Nullable(CapabilityKind::Dir) => super::DIR_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Env) | CapabilityRepresentation::Nullable(CapabilityKind::Env) => super::ENV_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::File) | CapabilityRepresentation::Nullable(CapabilityKind::File) => super::FILE_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Net) | CapabilityRepresentation::Nullable(CapabilityKind::Net) => super::NET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Listener) | CapabilityRepresentation::Nullable(CapabilityKind::Listener) => super::LISTENER_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Socket) | CapabilityRepresentation::Nullable(CapabilityKind::Socket) => super::SOCKET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Exec) | CapabilityRepresentation::Nullable(CapabilityKind::Exec) => super::EXEC_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Fetch) | CapabilityRepresentation::Nullable(CapabilityKind::Fetch) => super::FETCH_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Clock) | CapabilityRepresentation::Nullable(CapabilityKind::Clock) => super::CLOCK_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::SecretStore) | CapabilityRepresentation::Nullable(CapabilityKind::SecretStore) => super::SECRET_STORE_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Secret) | CapabilityRepresentation::Nullable(CapabilityKind::Secret) => super::SECRET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildOut) | CapabilityRepresentation::Nullable(CapabilityKind::BuildOut) => super::BUILD_OUT_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildRead) | CapabilityRepresentation::Nullable(CapabilityKind::BuildRead) => super::BUILD_READ_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildEnv) | CapabilityRepresentation::Nullable(CapabilityKind::BuildEnv) => super::BUILD_ENV_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildExec) | CapabilityRepresentation::Nullable(CapabilityKind::BuildExec) => super::BUILD_EXEC_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildNet) | CapabilityRepresentation::Nullable(CapabilityKind::BuildNet) => super::BUILD_NET_CAPABILITY_KIND,
        };
        return matches!(
            (representation, actual),
            (CapabilityRepresentation::Direct(_), super::ValueType::Capability(actual_kind))
                | (CapabilityRepresentation::Nullable(_), super::ValueType::NullableCapability(actual_kind))
                if actual_kind == kind
        );
    }
    match source.unqualified() {
        // `Dir` carries its rights as type arguments (`Dir[Read]`), and the
        // rights live in the host's authority object behind the token, so
        // every spelling is the one fixed ABI kind. This arm precedes the
        // argument-free one because `Dir` may appear with no arguments too.
        witchy_syntax::ast::Type::Named(name, _) if name == "Dir" => {
            actual == super::ValueType::Capability(super::DIR_CAPABILITY_KIND)
        }
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Duration" => actual == super::ValueType::Int,
            "Float" => actual == super::ValueType::Float,
            "Bool" => actual == super::ValueType::Bool,
            "Nil" | "Unit" => actual == super::ValueType::Unit,
            // Canonical lowering assigns the authenticated prelude Console
            // capability the fixed ABI kind 0; no candidate-selected kind is
            // accepted at this source boundary.
            "Console" => actual == super::ValueType::Capability(super::CapabilityKindId::new(0)),
            // The process environment grant, the one `Env` token.
            "Env" => actual == super::ValueType::Capability(super::ENV_CAPABILITY_KIND),
            // `Bytes` is the same raw-byte buffer under its own source type.
            "String" | "Bytes" => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
            // A scalar aggregate: one reference to a fixed layout whose slots
            // are exactly the shape's fields, in order.
            _ => witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            }),
        },
        // A dictionary is its canonical tuple of the key list and the value
        // list, so it matches by that shape like any other aggregate.
        witchy_syntax::ast::Type::Named(name, arguments)
            if matches!(name.as_str(), "Option" | "Result") || (name == "Dict" && arguments.len() == 2) =>
        {
            witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            })
        }
        witchy_syntax::ast::Type::Tuple(_) => {
            witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            })
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
        // Concrete user generics use the same independently matched field
        // graph as other aggregates. A type name or its arguments alone never
        // grant a candidate reference its layout authority.
        witchy_syntax::ast::Type::Named(_, _) => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
        // A `fn` value is a closure record: a reference to a layout the
        // module registered with a closure signature, which, past its
        // environment parameter, is the source signature.
        witchy_syntax::ast::Type::Fn(parameters, result, _, _) => match actual {
            super::ValueType::Ref(layout) => {
                let Some(&id) = closure_layouts.get(&layout) else { return false };
                let matched = signatures
                    .iter()
                    .find(|signature| signature.id == id)
                    .and_then(|signature| strip_closure_environment_with(signature, closure_layouts))
                    .is_some_and(|declaration| {
                        let witchy_syntax::ast::Type::Fn(_, _, conventions, _) = source.unqualified() else { return false };
                        candidate_signature_matches_source_inner(&declaration, source, parameters, result, conventions, layouts, signatures, context, records, closure_layouts)
                    });
                matched
            }
            _ => false,
        },
        _ => false,
    }
}

fn record_layout_matches(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    record_layout_matches_with_closures(layout, fields, nullable_fields, layouts, records, &BTreeMap::new(), &[], &mut CorrespondenceContext::default())
}

fn record_layout_matches_with_closures(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    let mut pending = Vec::new();
    if !enqueue_record_fields_with_closures(layout, fields, nullable_fields, layouts, &mut pending, closure_layouts) {
        return false;
    }
    match_layout_graph(pending, layouts, records, closure_layouts, signatures, context)
}

fn enqueue_record_fields(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
) -> bool {
    enqueue_record_fields_with_closures(layout, fields, nullable_fields, layouts, pending, &BTreeMap::new())
}

fn enqueue_record_fields_with_closures(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    _closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    use crate::layout::{LayoutKind, ScalarKind};
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else { return false };
    let LayoutKind::PackedRecord { fields: actual } = descriptor.kind() else { return false };
    actual.len() == fields.len()
        && actual.iter().zip(fields).all(|(actual, field)| {
            let Some(actual) = layouts.get(*actual) else { return false };
            if is_unit_source_type(field) {
                return matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Unit));
            }
            match field.unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Duration" | "Float" | "Bool" | "Nil" | "Unit") => match name.as_str() {
                    "Int" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                    "Duration" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Duration)),
                    "Float" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                    "Bool" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
                    "Nil" | "Unit" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Unit)),
                    _ => false,
                },
                // A string, list, tuple, or aggregate field lives behind an
                // owned pointer slot whose pointee is that value's own layout.
                _ => matches!(actual.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics }
                    if *semantics == if nullable_fields { crate::layout::PointerSlot::OwnedNullable } else { crate::layout::PointerSlot::Owned }
                        && { pending.push((*pointee, field.unqualified().clone())); true }),
            }
        })
}

fn layout_matches_source(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    layout_matches_source_with_closures(layout, source, layouts, records, &BTreeMap::new(), &[], &mut CorrespondenceContext::default())
}

fn layout_matches_source_with_closures(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    match_layout_graph(vec![(layout, source.unqualified().clone())], layouts, records, closure_layouts, signatures, context)
}

/// Check every node in the finite layout/source-type product. A revisited
/// exact pair closes a recursive edge only: all other outgoing obligations
/// still have to pass. Nominal resolution never replaces source validation.
fn match_layout_graph(
    mut pending: Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    while let Some((layout, source)) = pending.pop() {
        let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
        let key = (layout, source.clone());
        match context.enter_layout(key.clone()) {
            CorrespondenceVisit::Active | CorrespondenceVisit::Completed => continue,
            CorrespondenceVisit::Limit => return false,
            CorrespondenceVisit::New => {}
        }
        if !enqueue_layout_children(layout, &source, layouts, records, &mut pending, closure_layouts, signatures, context) {
            context.finish_layout(key, false);
            return false;
        }
        context.finish_layout(key, true);
    }
    true
}

/// A plain packed List layout has no authority to erase a nested view or
/// unique contract, including one hidden in a tuple or nominal field.
/// Frozen is representation-neutral and remains allowed by the source ABI.
/// Authenticated owner-list families are checked before this layout path.
fn list_element_needs_owner_carrier(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_syntax::ast::{Type, TypeQual};
    // Keep this judgment iterative and bounded: candidate source and nominal
    // definitions are untrusted inputs to the SafeWIR authority checker.
    const MAX_LIST_ELEMENT_SCAN_NODES: usize = 1_024;
    let mut pending = vec![source];
    let mut visited_definitions = BTreeSet::new();
    let mut visited_nodes = 0;
    while let Some(node) = pending.pop() {
        visited_nodes += 1;
        if visited_nodes > MAX_LIST_ELEMENT_SCAN_NODES { return true; }
        match node {
            Type::Qualified(TypeQual::Frozen, inner) | Type::Slice(inner) => pending.push(inner),
            Type::Qualified(..) => return true,
            Type::Tuple(items) => pending.extend(items),
            Type::RecordCompose { base, fields } => {
                pending.push(base);
                pending.extend(fields.iter().map(|(_, field)| field));
            }
            Type::Fn(parameters, result, _, _) => {
                pending.extend(parameters);
                pending.push(result);
            }
            Type::Named(name, arguments) | Type::Dyn(name, arguments) => {
                pending.extend(arguments);
                if visited_definitions.insert(name) {
                    if let Some(definition) = records.get(name) {
                        pending.extend(definition.variants.iter().flat_map(|variant| &variant.fields));
                    }
                }
            }
        }
    }
    false
}

fn enqueue_layout_children(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
    signatures: &[super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, ScalarKind};
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else { return false };
    // An aggregate (record, sum, tuple, scalar Option/Result) is a fixed
    // record of its shape's slots.
    if let Some(shape) = witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records) {
        return enqueue_record_fields(layout, &shape.fields, shape.tagged, layouts, pending);
    }
    match source.unqualified() {
        witchy_syntax::ast::Type::Named(name, arguments) if matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty() => {
            let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else { return false };
            layouts.get(*element).is_some_and(|element| matches!(element.kind(), LayoutKind::Scalar(ScalarKind::RawByte)))
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return false };
            let Some(element) = layouts.get(*element) else { return false };
            // A nested qualifier changes the source carrier. In particular,
            // List(&Int) uses its authenticated GC owner-list family; a plain
            // packed List(Int) layout cannot stand in for it. Preserve this
            // boundary for pointer elements such as String too.
            if list_element_needs_owner_carrier(&arguments[0], records) {
                return false;
            }
            if is_unit_source_type(&arguments[0]) {
                return matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Unit));
            }
            match arguments[0].unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                witchy_syntax::ast::Type::Named(name, inner_arguments) if name == "Duration" && inner_arguments.is_empty() => {
                    matches!(&arguments[0], witchy_syntax::ast::Type::Named(name, arguments) if name == "Duration" && arguments.is_empty())
                        && matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Duration))
                }
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Float" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Bool" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
                witchy_syntax::ast::Type::Named(name, arguments) if matches!(name.as_str(), "Nil" | "Unit") && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Unit)),
                inner => matches!(element.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } if { pending.push((*pointee, inner.unqualified().clone())); true }),
            }
        }
        witchy_syntax::ast::Type::Fn(..) => {
            let LayoutKind::PackedRecord { fields } = descriptor.kind() else { return false };
            let Some(&id) = closure_layouts.get(&layout) else { return false };
            let Some(signature) = signatures.iter().find(|signature| signature.id == id) else { return false };
            let Some(declaration) = strip_closure_environment_with(signature, closure_layouts) else { return false };
            let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
            if fields.len() != 2 || !candidate_signature_matches_source_inner(
                &declaration,
                source,
                parameters,
                result,
                conventions,
                layouts,
                signatures,
                context,
                records,
                closure_layouts,
            ) {
                return false;
            }
            let Some(code) = layouts.get(fields[0]) else { return false };
            let Some(environment) = layouts.get(fields[1]) else { return false };
            matches!(code.kind(), LayoutKind::Scalar(ScalarKind::Callable))
                && matches!(environment.kind(), LayoutKind::RuntimePointerValueSlot {
                    semantics: PointerSlot::Owned, ..
                })
        }
        _ => false,
    }
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
    if let Some(source) = source {
        if module.checked_source().digest() != source.source_witness().source_bundle_digest()
            || checked_lowering.proper_tail_manifest_digest.bytes()
                != source.source_witness().proper_tail_manifest_digest()
            || checked_lowering.checked_source_witness.callable_abi_manifest_digest
                != source.callable_abi_manifest().digest()
            || checked_lowering.checked_source_witness.closure_witness_digest
                != source.source_witness().closure_witness_digest()
            || checked_lowering.checked_source_witness.launch_contract_digest
                != source.source_witness().launch_contract_digest()
            || module.launch_contract_payload() != source.launch_contract_payload()
        {
            return Err(FinalValidationError::SourceWitnessDoesNotMatchInitial);
        }
        validate_candidate_abi_bindings(&module, source)
            .map_err(FinalValidationError::CandidateAbi)?;
    }
    super::check::recheck_final(&module).map_err(FinalValidationError::Safety)?;
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
    } = checked;
    if !optimization_chain_matches(&module, &checked_lowering, &optimizations) {
        return Err(FinalValidationError::OptimizationDoesNotMatchWitness);
    }
    if checked_lowering.proper_tail_manifest_digest != tail_digest(&module) {
        return Err(FinalValidationError::InitialTailManifestDoesNotMatchWitness);
    }
    if let Some(source) = source {
        if module.checked_source().digest() != source.source_witness().source_bundle_digest()
            || checked_lowering.proper_tail_manifest_digest.bytes()
                != source.source_witness().proper_tail_manifest_digest()
            || checked_lowering.checked_source_witness.callable_abi_manifest_digest
                != source.callable_abi_manifest().digest()
            || checked_lowering.checked_source_witness.closure_witness_digest
                != source.source_witness().closure_witness_digest()
            || checked_lowering.checked_source_witness.launch_contract_digest
                != source.source_witness().launch_contract_digest()
            || module.launch_contract_payload() != source.launch_contract_payload()
        {
            return Err(FinalValidationError::SourceWitnessDoesNotMatchInitial);
        }
        validate_candidate_abi_bindings(&module, source)
            .map_err(FinalValidationError::CandidateAbi)?;
    }
    super::check::recheck_final(&module).map_err(FinalValidationError::Safety)?;
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

#[cfg(test)]
pub(crate) fn validate_final_for_test(
    checked: CheckedInitialSafeWir,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_final_inner(checked.module, checked.witness, None)
}

#[cfg(test)]
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
        },
        None,
    )
}

#[cfg(test)]
fn validate_final_parts_for_test(
    module: CanonicalModule<InitialValid>,
    witness: CheckedLoweringWitness,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_final_inner(module, witness, None)
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
    let checked_source_witness = CheckedSourceWitness {
        checked_source: module.checked_source(),
        initial_module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        callable_abi_manifest_digest: [0; 32],
        closure_witness_digest: [0; 32],
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
    Ok(CheckedInitialSafeWir { module, witness })
}

// Authority cases that need a real checked source (RFC-0170 P4).
#[cfg(feature = "checked-source-test-api")]
#[doc(hidden)]
pub mod checked_source_cases;

/// The one-function `fn() -> Int` candidate the authority tests and the
/// checked-source cases perturb.
#[cfg(any(test, feature = "checked-source-test-api"))]
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

#[cfg(test)]
mod tests {
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

    fn owner_adapter_candidate() -> (CanonicalModule<LoweredCandidate>, FunctionId, FunctionId) {
        use crate::canonical_safewir::closure_owner_contract::{ConstructorId, OwnerConstructor, OwnerFamily, OwnerOperation};
        use crate::canonical_safewir::{BlockParameter, Ownership};
        use crate::layout::LayoutId;

        let (mut module, adapter, target) = adapter_candidate(forwarding_body().0, forwarding_body().1);
        let owner_type = crate::canonical_safewir::GcTypeId::new(0xc1000000);
        let signature = &mut module.signatures[0];
        signature.parameters[0] = ValueType::Gc(owner_type);
        signature.contract.parameters[0].value_type = ValueType::Gc(owner_type);
        module.functions[0].value_types.insert(ValueId::new(0), ValueType::Gc(owner_type));
        module.functions[0].blocks[0].parameters[0] = BlockParameter {
            value: ValueId::new(0), value_type: ValueType::Gc(owner_type), ownership: Some(Ownership::Shared),
        };
        module.functions[0].blocks[0].operations.push(Operation::ClosureOwner(OwnerOperation::Drop {
            owner: ValueId::new(0), family: FunctionSignatureId::new(0),
        }));
        let family = OwnerFamily {
            source: super::super::closure_owner_contract::OwnerFamilySource::Closure,
            owner_type,
            list_type: crate::canonical_safewir::GcTypeId::new(0xc1000001),
            signature: FunctionSignatureId::new(0),
            declaration_signature: FunctionSignatureId::new(1),
            environment: LayoutId::from_bytes([0; 32]),
            constructors: vec![OwnerConstructor {
                id: ConstructorId(0), function: adapter, witness: None, environment_tag: 0, captures: Vec::new(),
            }],
        };
        (module.with_owner_families(vec![family]), adapter, target)
    }

    #[test]
    fn owner_family_named_adapter_admits_and_mismatched_target_refuses() {
        let (module, adapter, target) = owner_adapter_candidate();
        assert!(adapter_shape_holds(&module, adapter, target));
        let wrong = module.with_adapter_bindings(crate::canonical_safewir::CandidateAdapterBindings::new(vec![
            crate::canonical_safewir::CandidateAdapterBinding::new(adapter, adapter),
        ]));
        assert!(!adapter_shape_holds(&wrong, adapter, target));
    }

    #[test]
    fn adapter_binding_rejects_a_body_that_keeps_the_environment() {
        // Every call site clones the record for the adapter; one that never
        // releases its token would leak a reference per call.
        let (operations, terminator) = forwarding_body();
        let (module, adapter, target) = adapter_candidate(operations, terminator);
        assert!(!adapter_shape_holds(&module, adapter, target));
    }

    #[test]
    fn adapter_binding_rejects_a_body_that_does_extra_work() {
        let (mut operations, terminator) = forwarding_body();
        operations.push(Operation::ConstInt { destination: ValueId::new(3), value: 1 });
        let (module, adapter, target) = adapter_candidate(operations, terminator);
        assert!(!adapter_shape_holds(&module, adapter, target));
    }

    #[test]
    fn adapter_binding_rejects_an_unregistered_environment_layout() {
        let (operations, terminator) = forwarding_body();
        let (module, adapter, target) = adapter_candidate(operations, terminator);
        let module = module.with_closure_layouts(BTreeMap::new());
        assert!(!adapter_shape_holds(&module, adapter, target));
    }

    #[test]
    fn source_sum_nullable_fields_have_exact_canonical_slot_semantics() {
        use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};
        use witchy_syntax::ast::Type;
        let mut layouts = LayoutInterner::new();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = layouts.intern_runtime_buffer(byte).unwrap();
        let source_string = Type::Named("String".into(), vec![]);
        let sum = Type::Named("Option".into(), vec![source_string.clone()]);
        let tuple = Type::Tuple(vec![Type::Named("Int".into(), vec![]), source_string]);
        for semantics in [PointerSlot::Owned, PointerSlot::OwnedNullable, PointerSlot::Raw] {
            let record = layouts.intern_runtime_pointer_value_slot(string, semantics)
                .and_then(|slot| layouts.intern_packed_record(vec![int, slot]));
            if semantics == PointerSlot::Raw {
                assert!(record.is_err(), "raw ownership erasure must fail during layout construction");
                continue;
            }
            let record = record.unwrap();
            assert_eq!(layout_matches_source(record, &sum, &layouts, &BTreeMap::new()), semantics == PointerSlot::OwnedNullable);
            assert_eq!(layout_matches_source(record, &tuple, &layouts, &BTreeMap::new()), semantics == PointerSlot::Owned);
        }
        let wrong = layouts.intern_runtime_pointer_value_slot(int, PointerSlot::OwnedNullable).unwrap();
        let record = layouts.intern_packed_record(vec![int, wrong]).unwrap();
        assert!(!layout_matches_source(record, &sum, &layouts, &BTreeMap::new()));
    }

    #[test]
    fn source_recursive_layout_graph_checks_every_pair_and_child() {
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
        assert!(layout_matches_source(nominal, &named("Tree"), &layouts, &records));
        assert!(!layout_matches_source(nominal, &named("Other"), &layouts, &records), "a cycle does not excuse a mismatched sibling field");
        let pair = layouts.intern_packed_record(vec![slot, slot]).unwrap();
        assert!(!layout_matches_source(pair, &Type::Tuple(vec![named("Tree"), named("Other")]), &layouts, &records), "visited keys must include the source type, not just the layout");
        assert!(!layout_matches_source(nominal, &Type::Named("Option".into(), vec![named("Tree")]), &layouts, &records), "recursive identity cannot substitute for a tagged nullable layout");
    }

    #[test]
    fn source_generic_layout_graph_checks_instantiated_payload_types() {
        use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};
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
    }

    #[test]
    fn root_unique_keeps_gc_owners_tokenized_while_scalars_stay_value_lanes() {
        use crate::canonical_safewir::{GcTypeId, ParameterOwnership};

        let owner = ValueType::Gc(GcTypeId::new(7));
        assert!(root_unique_parameter_ownership_matches(owner, ParameterOwnership::Unique));
        assert!(!root_unique_parameter_ownership_matches(owner, ParameterOwnership::Value));
        assert!(root_unique_result_requires_token(&owner));

        assert!(root_unique_parameter_ownership_matches(ValueType::Int, ParameterOwnership::Value));
        assert!(!root_unique_parameter_ownership_matches(ValueType::Int, ParameterOwnership::Unique));
        assert!(!root_unique_result_requires_token(&ValueType::Int));
    }

    #[test]
    fn result_unique_ok_authority_refuses_nested_or_arbitrary_qualifiers() {
        use witchy_syntax::ast::{Type, TypeQual};
        let int = || Type::Named("Int".into(), Vec::new());
        let string = || Type::Named("String".into(), Vec::new());
        let exact = Type::Named(
            "Result".into(),
            vec![
                Type::Qualified(TypeQual::Unique, Box::new(Type::Named("List".into(), vec![int()]))),
                string(),
            ],
        );
        assert!(witchy_syntax::ast::is_direct_unique_result_ok_collection(&exact));

        let nested = Type::Named(
            "Result".into(),
            vec![
                Type::Qualified(
                    TypeQual::Unique,
                    Box::new(Type::Named(
                        "List".into(),
                        vec![Type::Qualified(TypeQual::Unique, Box::new(int()))],
                    )),
                ),
                string(),
            ],
        );
        assert!(!witchy_syntax::ast::is_direct_unique_result_ok_collection(&nested));

        let arbitrary = Type::Named(
            "Result".into(),
            vec![Type::Qualified(TypeQual::Unique, Box::new(Type::Tuple(vec![int()]))), string()],
        );
        assert!(!witchy_syntax::ast::is_direct_unique_result_ok_collection(&arbitrary));

        let qualified_error = Type::Named(
            "Result".into(),
            vec![
                Type::Qualified(TypeQual::Unique, Box::new(Type::Named("List".into(), vec![int()]))),
                Type::Qualified(TypeQual::Unique, Box::new(Type::Named("List".into(), vec![int()])))
            ],
        );
        assert!(!witchy_syntax::ast::is_direct_unique_result_ok_collection(&qualified_error));
    }

    #[test]
    fn duration_list_layout_correspondence_requires_duration_elements() {
        use crate::layout::{RcHeader, ScalarKind};
        use witchy_syntax::ast::{Type, TypeQual};

        let mut layouts = LayoutInterner::new();
        let duration = layouts.intern_scalar(ScalarKind::Duration).unwrap();
        let integer = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let duration_list = layouts.intern_packed_list(duration, RcHeader::Required).unwrap();
        let integer_list = layouts.intern_packed_list(integer, RcHeader::Required).unwrap();
        let source = Type::Named("List".into(), vec![Type::Named("Duration".into(), vec![])]);
        let mut pending = Vec::new();
        assert!(enqueue_layout_children(
            duration_list, &source, &layouts, &BTreeMap::new(), &mut pending,
            &BTreeMap::new(), &[], &mut CorrespondenceContext::default(),
        ));
        assert!(pending.is_empty(), "a scalar Duration element needs no child owner layout");
        assert!(!enqueue_layout_children(
            integer_list, &source, &layouts, &BTreeMap::new(), &mut Vec::new(),
            &BTreeMap::new(), &[], &mut CorrespondenceContext::default(),
        ), "an Int list cannot authenticate the Duration source layout");
        for qualifier in [TypeQual::View, TypeQual::Unique] {
            let qualified = Type::Named("List".into(), vec![
                Type::Qualified(qualifier, Box::new(Type::Named("Duration".into(), vec![]))),
            ]);
            assert!(!enqueue_layout_children(
                duration_list, &qualified, &layouts, &BTreeMap::new(), &mut Vec::new(),
                &BTreeMap::new(), &[], &mut CorrespondenceContext::default(),
            ), "a qualified Duration source cannot claim the plain scalar layout");
        }
    }

    #[test]
    fn plain_list_layout_cannot_authenticate_a_view_element_carrier() {
        use crate::layout::{PointerSlot, RcHeader, ScalarKind};
        use witchy_syntax::ast::{Type, TypeQual};

        let mut layouts = LayoutInterner::new();
        let integer = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let raw_byte = layouts.intern_scalar(ScalarKind::RawByte).unwrap();
        let string = layouts.intern_runtime_buffer(raw_byte).unwrap();
        let string_slot = layouts.intern_runtime_pointer_value_slot(string, PointerSlot::Owned).unwrap();
        let cases = [
            ("Int", layouts.intern_packed_list(integer, RcHeader::Required).unwrap()),
            ("String", layouts.intern_packed_list(string_slot, RcHeader::Required).unwrap()),
        ];
        for (element, layout) in cases {
            let plain = Type::Named("List".into(), vec![Type::Named(element.into(), vec![])]);
            assert!(layout_matches_source(layout, &plain, &layouts, &BTreeMap::new()),
                "the ordinary List({element}) layout still corresponds");
            let frozen = Type::Named("List".into(), vec![Type::Qualified(
                TypeQual::Frozen, Box::new(Type::Named(element.into(), vec![])),
            )]);
            assert!(layout_matches_source(layout, &frozen, &layouts, &BTreeMap::new()),
                "frozen is representation-neutral for List({element})");
            for qualifier in [TypeQual::View, TypeQual::Unique] {
                let qualified = Type::Named("List".into(), vec![Type::Qualified(
                    qualifier, Box::new(Type::Named(element.into(), vec![])),
                )]);
                assert!(!layout_matches_source(layout, &qualified, &layouts, &BTreeMap::new()),
                    "a plain List({element}) layout must not erase its element qualifier");
                assert!(!value_type_matches_source(
                    ValueType::Ref(layout), &qualified, &layouts, &[],
                    &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new(),
                ), "a forged plain Ref must not satisfy List(qualified {element})");
            }
        }

        let tuple = layouts.intern_packed_record(vec![integer]).unwrap();
        let tuple_slot = layouts.intern_runtime_pointer_value_slot(tuple, PointerSlot::Owned).unwrap();
        let tuple_list = layouts.intern_packed_list(tuple_slot, RcHeader::Required).unwrap();
        let plain_tuple = Type::Named("List".into(), vec![Type::Tuple(vec![Type::Named("Int".into(), vec![])])]);
        assert!(layout_matches_source(tuple_list, &plain_tuple, &layouts, &BTreeMap::new()));
        let viewed_tuple = Type::Named("List".into(), vec![Type::Tuple(vec![Type::Qualified(
            TypeQual::View, Box::new(Type::Named("Int".into(), vec![])),
        )])]);
        assert!(!layout_matches_source(tuple_list, &viewed_tuple, &layouts, &BTreeMap::new()),
            "a tuple cannot hide a rooted view inside an ordinary List layout");
        assert!(!value_type_matches_source(
            ValueType::Ref(tuple_list), &viewed_tuple, &layouts, &[],
            &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new(),
        ));
    }

    #[test]
    fn plain_record_parameter_cannot_impersonate_an_aggregate_view_owner() {
        use crate::layout::ScalarKind;
        use witchy_syntax::ast::{CallableQualifiers, Type, TypeQual};

        let mut layouts = LayoutInterner::new();
        let integer = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let record = layouts.intern_packed_record(vec![integer]).unwrap();
        let plain = Type::Tuple(vec![Type::Named("Int".into(), vec![])]);
        let viewed = Type::Tuple(vec![Type::Qualified(
            TypeQual::View, Box::new(Type::Named("Int".into(), vec![])),
        )]);
        let records = BTreeMap::new();
        assert!(is_direct_rooted_view_aggregate_carrier(&viewed, &records));
        let source = Type::Fn(vec![viewed.clone()], Box::new(Type::Tuple(vec![])),
            vec![], CallableQualifiers::default());
        assert!(source_abi_qualifiers_supported(&source, &records));
        assert!(value_type_matches_source(
            ValueType::Ref(record), &plain, &layouts, &[],
            &mut CorrespondenceContext::default(), &records, &BTreeMap::new(),
        ));
        assert!(!value_type_matches_source(
            ValueType::Ref(record), &viewed, &layouts, &[],
            &mut CorrespondenceContext::default(), &records, &BTreeMap::new(),
        ), "an erased Int slot cannot prove the retained view owner");
    }

    #[test]
    fn direct_shared_result_without_closed_declaration_is_not_admissible() {
        use witchy_syntax::ast::{CallableQualifiers, Convention, Type, TypeQual};
        let string = Type::Named("String".into(), vec![]);
        let shared = Type::Qualified(TypeQual::Borrow("a".into()), Box::new(string.clone()));
        let direct_alias = Type::Fn(
            vec![shared.clone()],
            Box::new(shared),
            vec![Convention::Let],
            CallableQualifiers::default(),
        );
        let owned = Type::Fn(
            vec![string.clone()],
            Box::new(string),
            vec![Convention::Let],
            CallableQualifiers::default(),
        );

        assert!(!direct_reference_binding_is_checked(&direct_alias, None, None, false, &BTreeMap::new()));
        assert!(direct_reference_binding_is_checked(&owned, None, None, false, &BTreeMap::new()));
    }

    #[test]
    fn exclusive_reference_prefilter_keeps_direct_let_and_projected_var_disjoint() {
        use witchy_syntax::ast::{CallableQualifiers, Convention, Type, TypeQual};

        let mutable = |name: &str| Type::Qualified(
            TypeQual::BorrowMut("a".into()),
            Box::new(Type::Named(name.into(), vec![])),
        );
        let callable = |parameter, convention| Type::Fn(
            vec![parameter],
            Box::new(mutable("String")),
            vec![convention],
            CallableQualifiers::default(),
        );
        assert!(source_abi_qualifiers_supported(
            &callable(mutable("String"), Convention::Let), &BTreeMap::new(),
        ));
        assert!(source_abi_qualifiers_supported(
            &callable(mutable("Pair"), Convention::Var), &BTreeMap::new(),
        ));
        assert!(!source_abi_qualifiers_supported(
            &callable(mutable("String"), Convention::Var), &BTreeMap::new(),
        ));
        assert!(!source_abi_qualifiers_supported(
            &callable(mutable("Pair"), Convention::Let), &BTreeMap::new(),
        ));
    }

    #[test]
    fn rooted_view_aggregate_authority_nests_only_through_carrier_fields() {
        let carrier = |source: &str, name: &str| {
            let module = witchy_syntax::parser::parse_module(source).expect("parse aggregate fixture");
            let records = witchy_canonical_source::aggregates::type_definitions(&module);
            is_direct_rooted_view_aggregate_carrier(
                &witchy_syntax::ast::Type::Named(name.into(), Vec::new()),
                &records,
            )
        };

        for (name, source) in [
            ("Holder", "type Holder:\n    view: &Int\n    count: Int\n"),
            ("OptionField", "type OptionField:\n    direct: &Int\n    nested: Option(&Int)\n"),
            ("NominalField", "type Inner:\n    view: &Int\n\ntype NominalField:\n    direct: &Int\n    nested: Inner\n"),
            ("GenericField", "type Box(a):\n    value: a\n\ntype GenericField:\n    direct: &Int\n    nested: Box(&Int)\n"),
        ] {
            assert!(carrier(source, name), "every view-bearing field of {name} is itself a carrier owner");
        }
        for (name, source) in [
            (
                "CallableDecoy",
                "type CallableDecoy:\n    direct: &Int\n    hidden: fn() -> &Int\n",
            ),
            (
                "ListDecoy",
                "type ListDecoy:\n    direct: &Int\n    hidden: List(&Int)\n",
            ),
            (
                "DeepDecoy",
                "type Inner:\n    view: &Int\n    hidden: List(&Int)\n\ntype DeepDecoy:\n    direct: &Int\n    nested: Inner\n",
            ),
        ] {
            assert!(
                !carrier(source, name),
                "the authority predicate must reject the uncarried view in {name}",
            );
        }
    }

    #[test]
    fn canonical_net_source_kinds_reject_integer_and_cross_family_substitution() {
        use witchy_syntax::ast::Type;
        let matches = |actual, source: &Type| value_type_matches_source(actual, source,
            &LayoutInterner::new(), &[], &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new());
        for (name, expected) in [("Net", 4), ("Listener", 5), ("Socket", 6)] {
            let rights = if name == "Net" { vec![Type::Named("Listen".into(), vec![]), Type::Named("Tcp".into(), vec![])] } else { vec![] };
            let source = Type::Named(name.into(), rights);
            for candidate in 0..8 {
                assert_eq!(matches(ValueType::Capability(super::super::CapabilityKindId::new(candidate)), &source), candidate == expected);
            }
            assert!(!matches(ValueType::Int, &source));
            assert!(!matches(ValueType::Size, &source));
            assert!(!matches(ValueType::NullableCapability(super::super::CapabilityKindId::new(expected)), &source));
            assert!(ordinary_own_source_type_supported(&source));
        }
    }

    #[test]
    fn recursive_fn_returning_aggregate_matches_and_rejects_wrong_nested_signature() {
        use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};
        use witchy_syntax::ast::{CallableQualifiers, Type};
        use witchy_canonical_source::aggregates::ScalarAggregateShape;

        let named = |name: &str| Type::Named(name.into(), Vec::new());
        let source_fn = Type::Fn(
            Vec::new(),
            Box::new(named("Cursor")),
            Vec::new(),
            CallableQualifiers::ORDINARY,
        );
        let mut cursor_shape = ScalarAggregateShape {
            name: "Cursor".into(),
            fields: vec![source_fn.clone()],
            variants: Vec::new(),
            tagged: false,
            packed: false,
        }
        .packed_view();
        cursor_shape.packed = false;
        let records = BTreeMap::from([("Cursor".into(), cursor_shape)]);

        let mut layouts = LayoutInterner::new();
        let callable = layouts.intern_scalar(ScalarKind::Callable).unwrap();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let environment = layouts.intern_packed_record(vec![int]).unwrap();
        let environment_slot = layouts
            .intern_runtime_pointer_value_slot(environment, PointerSlot::Owned)
            .unwrap();
        let closure = layouts
            .intern_packed_record(vec![callable, environment_slot])
            .unwrap();
        let closure_slot = layouts
            .intern_runtime_pointer_value_slot(closure, PointerSlot::Owned)
            .unwrap();
        let cursor = layouts.intern_packed_record(vec![closure_slot]).unwrap();
        let signature_id = FunctionSignatureId::new(7);
        let closure_layouts = BTreeMap::from([(closure, signature_id)]);
        let signature = |result| FunctionSignature {
            id: signature_id,
            parameters: vec![ValueType::Ref(closure)],
            results: vec![result],
            contract: CallableContract {
                parameters: vec![super::super::ParameterContract {
                    value_type: ValueType::Ref(closure),
                    ownership: super::super::ParameterOwnership::Shared,
                }],
                results: vec![ResultContract::Value(result)],
                parameter_effects: BTreeMap::new(),
            },
        };

        assert!(layout_matches_source_with_closures(
            cursor,
            &named("Cursor"),
            &layouts,
            &records,
            &closure_layouts,
            &[signature(ValueType::Ref(cursor))],
            &mut CorrespondenceContext::default(),
        ));
        assert!(!layout_matches_source_with_closures(
            cursor,
            &named("Cursor"),
            &layouts,
            &records,
            &closure_layouts,
            &[signature(ValueType::Int)],
            &mut CorrespondenceContext::default(),
        ));
    }

    #[test]
    fn final_authority_requires_the_exact_initial_checked_module() {
        let checked = check_initial_with_test_witness(optimizable_candidate()).unwrap();
        let initial_snapshot = checked.module().clone();
        let final_module = validate_final_for_test(checked).unwrap();
        assert_eq!(final_module.checked_source(), initial_snapshot.checked_source());
    }

    #[test]
    fn witness_from_another_checked_module_is_rejected() {
        let checked = check_initial_with_test_witness(optimizable_candidate()).unwrap();
        let (initial, _) = checked.into_parts_for_test();

        let mut other = candidate();
        other.functions[0].blocks[0].operations[0] =
            Operation::ConstInt { destination: ValueId::new(0), value: 4 };
        let (_, wrong_witness) = check_initial_with_test_witness(other).unwrap().into_parts_for_test();
        assert!(matches!(
            validate_final_parts_for_test(initial, wrong_witness),
            Err(FinalValidationError::InitialModuleDoesNotMatchWitness)
        ));
    }

    #[test]
    fn final_authority_refuses_a_candidate_mutated_after_initial_validation() {
        let checked = check_initial_with_test_witness(candidate()).unwrap();
        let (mut module, witness) = checked.into_parts_for_test();
        module.functions[0].blocks[0].operations[0] =
            Operation::ConstInt { destination: ValueId::new(0), value: 9 };

        assert!(matches!(
            validate_final_parts_for_test(module, witness),
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
    fn tail_manifest_digest_is_bound_into_zero_pass_authority() {
        let candidate = candidate();
        let expected = tail_digest(&candidate).bytes();
        let checked = check_initial_with_expected_digest(candidate, expected).unwrap();
        let final_module = validate_final_for_test(checked).unwrap();
        assert_eq!(final_module.checked_lowering().proper_tail_manifest_digest.bytes(), expected);
    }

    #[test]
    fn optimized_authority_refuses_a_mutated_output_module() {
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

    #[test]
    fn optimized_authority_refuses_an_unrelated_witness() {
        let first = crate::canonical_safewir::optimize::
            eliminate_same_block_redundant_bounds_guards_checked(
                check_initial_with_test_witness(optimizable_candidate()).unwrap(),
            )
            .unwrap();
        let (module, lowering, _) = first.into_parts_for_test();

        let mut unrelated = optimizable_candidate();
        unrelated.functions[0].blocks[0].operations[0] =
            Operation::ConstSize { destination: ValueId::new(0), value: 17 };
        let unrelated = crate::canonical_safewir::optimize::
            eliminate_same_block_redundant_bounds_guards_checked(
                check_initial_with_test_witness(unrelated).unwrap(),
            )
            .unwrap();
        let (_, _, unrelated_witness) = unrelated.into_parts_for_test();

        assert!(matches!(
            validate_optimized_final_parts_for_test(module, lowering, unrelated_witness),
            Err(FinalValidationError::OptimizationDoesNotMatchWitness)
        ));
    }

    #[test]
    fn optimized_authority_refuses_a_non_progressing_revision_link() {
        let optimized = crate::canonical_safewir::optimize::
            eliminate_same_block_redundant_bounds_guards_checked(
                check_initial_with_test_witness(optimizable_candidate()).unwrap(),
            )
            .unwrap();
        let (module, lowering, mut witnesses) = optimized.into_parts_for_test();
        witnesses[0].output_revision = witnesses[0].input_revision;

        assert!(matches!(
            validate_optimized_final_parts_for_test(module, lowering, witnesses),
            Err(FinalValidationError::OptimizationDoesNotMatchWitness)
        ));
    }
}
