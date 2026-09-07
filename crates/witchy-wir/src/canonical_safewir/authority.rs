use std::collections::{BTreeMap, BTreeSet};
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

/// Final canonical SafeWIR authority. Its fields are private; only the final
/// checker can mint it, and only the private backend may inspect its module.
pub struct ValidatedSafeWir {
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
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
        witness_digest(b"witchy-safewir-zero-pass-authority-v1", &[&self.exact_module.0])
    }

    pub(crate) fn validate_pre_admission(
        &self,
    ) -> Result<(), AdmissionWitnessMismatch> {
        let expected_exact_module = ExactModuleWitness::derive(&self.module, &self.checked_lowering);
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
        source: &witchy_types::pipeline::CanonicalCheckedSource,
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
    fn derive(
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
    SourceWitnessDoesNotMatchInitial,
    Safety(InitialCheckError),
    CandidateAbi(CandidateAbiBindingError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateAbiBindingError {
    MissingBinding { source_declaration: u32 },
    UnexpectedBinding { source_declaration: u32 },
    DuplicateSourceDeclaration { source_declaration: u32 },
    DuplicateCandidateFunction { function: super::FunctionId },
    MissingCandidateFunction { function: super::FunctionId },
    MissingCandidateSignature { function: super::FunctionId },
    SourceAbiMismatch { source_declaration: u32 },
    CandidateSignatureMismatch { function: super::FunctionId },
    MissingCanonicalLayouts,
    MissingClosureWitness { closure: u32 },
    CapturingClosureBinding { closure: u32 },
    ClosureSignatureMismatch { closure: u32 },
    MissingClosureEnvironmentParameter { function: super::FunctionId },
    AdapterShapeMismatch { adapter: super::FunctionId },
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
}

impl fmt::Display for InitialWitnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(error) => error.fmt(formatter),
            Self::CandidateAbi(error) => error.fmt(formatter),
            Self::LaunchContractMismatch => formatter.write_str("canonical SafeWIR launch contract does not match checked source"),
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
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<CheckedInitialSafeWir, InitialWitnessError> {
    let source_digest = source.source_witness().source_bundle_digest();
    let expected_source = source_digest;
    validate_candidate_abi_bindings(&candidate, source)
        .map_err(InitialWitnessError::CandidateAbi)?;
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

fn validate_candidate_abi_bindings<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<(), CandidateAbiBindingError> {
    use witchy_types::canonical_abi::CanonicalTargetPolicy;

    let source_entries = source.callable_abi_manifest().entries().iter().filter_map(|entry| {
        let CanonicalTargetPolicy::ClosedLocal(target) = entry.target_policy else { return None };
        Some((target.function_ordinal, entry.key.as_bytes()))
    }).collect::<BTreeMap<_, _>>();
    let records = witchy_types::canonical_aggregates::type_definitions(source.typed_module().module());
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
        if !candidate_signature_matches_source_with_records(signature, source_signature, candidate, &records) {
            return Err(CandidateAbiBindingError::CandidateSignatureMismatch { function: binding.function() });
        }
    }
    // A function synthesized for a lambda is bound to that lambda's closure
    // witness instead of a source declaration: the witness must exist, its
    // captures must be `Int` values copied into the closure record's slots
    // (no more of them than the record holds), and it must carry the
    // function's signature past the environment parameter.
    let closures = source.closure_witness().entries();
    let (_, closure_layout_catalog) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) {
        Ok(value) => value,
        Err(_) => return Err(CandidateAbiBindingError::MissingCanonicalLayouts),
    };
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
        let capture_slots = match signature.parameters.first() {
            Some(super::ValueType::Ref(layout)) => closure_layout_catalog
                .get(*layout)
                .and_then(|descriptor| match descriptor.kind() {
                    crate::layout::LayoutKind::PackedRecord { fields } => Some(fields.len().saturating_sub(1)),
                    _ => None,
                })
                .unwrap_or(0),
            _ => 0,
        };
        let int_capture = |capture: &witchy_types::canonical_closure::ClosureCaptureWitness| {
            matches!(capture.transfer(), witchy_types::canonical_closure::ClosureCaptureTransfer::CopyScalar)
                && matches!(capture.checked_type().unqualified(), witchy_syntax::ast::Type::Named(name, arguments) if name == "Int" && arguments.is_empty())
        };
        if witness.captures().len() > capture_slots || !witness.captures().iter().all(int_capture) {
            return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: binding.closure() });
        }
        let Some(source_signature) = witchy_types::typeck::ty_to_ast(witness.signature()) else {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        };
        // The synthesized function takes the closure object first; only the
        // remaining lanes correspond to the source lambda's own signature.
        let Some(source_lanes) = strip_closure_environment(signature, candidate) else {
            return Err(CandidateAbiBindingError::MissingClosureEnvironmentParameter { function: binding.function() });
        };
        if !candidate_signature_matches_source_with_records(&source_lanes, &source_signature, candidate, &records) {
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
        if !adapter_body_forwards(adapter, target.id, target_signature) {
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
    strip_closure_environment_with(signature, candidate.closure_layouts())
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
            results: signature.contract.results.clone(),
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
            (super::CallArgument::Move(value), Some(super::ValueType::Ref(_))) => value == parameter,
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
            if object == environment => {}
        _ => return false,
    }
    matches!(&block.terminator, super::Terminator::Return { values } if values == destinations)
}

/// The checked module's type definitions, keyed by bare and entry-qualified
/// names. A nominal scalar aggregate (a record, a sum type, a scalar
/// `Option`/`Result`) binds to a candidate reference whose layout carries
/// exactly the slots `witchy_types::canonical_aggregates` derives for it.
type ScalarRecordFields = BTreeMap<String, witchy_syntax::ast::TypeDef>;

fn candidate_signature_matches_source<S: ModuleStage>(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
) -> bool {
    candidate_signature_matches_source_with_records(actual, source, candidate, &BTreeMap::new())
}

fn candidate_signature_matches_source_with_records<S: ModuleStage>(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
    records: &ScalarRecordFields,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    let (_, layouts) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) { Ok(value) => value, Err(_) => return false };
    candidate_signature_matches_source_inner(actual, parameters, result, conventions, &layouts, candidate.signatures(), &mut BTreeSet::new(), records, candidate.closure_layouts())
}

fn candidate_signature_matches_source_inner(
    actual: &super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::FunctionSignature],
    seen: &mut BTreeSet<super::FunctionSignatureId>,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    if actual.parameters.len() != parameters.len() || actual.contract.parameters.len() != parameters.len() {
        return false;
    }
    let convention_at = |index| conventions.get(index).copied().unwrap_or_default();
    if !actual.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        (matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own) && ordinary_own_source_type_supported(source))
            && value_type_matches_source(*actual, source, layouts, signatures, seen, records, closure_layouts)
    }) { return false; }
    if !actual.contract.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        if !(matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own) && ordinary_own_source_type_supported(source))
            || !value_type_matches_source(actual.value_type, source, layouts, signatures, seen, records, closure_layouts) { return false; }
        match (actual.value_type, convention) {
            (super::ValueType::Ref(_), witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            (_, witchy_syntax::ast::Convention::Var) => matches!(actual.ownership, super::ParameterOwnership::WriteBack),
            (_, witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Value),
            (super::ValueType::Ref(_), _) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            _ => matches!(actual.ownership, super::ParameterOwnership::Value),
        }
    }) { return false; }
    let result_is_unit = matches!(result.unqualified(), witchy_syntax::ast::Type::Named(name, arguments) if (name == "Nil" || name == "Unit") && arguments.is_empty());
    let ordinary_results = usize::from(!result_is_unit);
    let write_back_parameters = parameters.iter().enumerate()
        .filter(|(index, _)| matches!(convention_at(*index), witchy_syntax::ast::Convention::Var))
        .collect::<Vec<_>>();
    (actual.results.len() == ordinary_results + write_back_parameters.len()
        && (result_is_unit || value_type_matches_source(actual.results[0], result, layouts, signatures, seen, records, closure_layouts))
        && write_back_parameters.iter().enumerate().all(|(offset, (parameter, source))| {
            value_type_matches_source(actual.results[ordinary_results + offset], source, layouts, signatures, seen, records, closure_layouts)
                && matches!(actual.contract.parameters[*parameter].ownership, super::ParameterOwnership::WriteBack)
        })
        && actual.contract.parameter_effects.iter().all(|(parameter, effects)| {
            matches!(actual.parameters.get(*parameter as usize), Some(super::ValueType::Ref(_)))
                && (matches!(actual.contract.parameters.get(*parameter as usize).map(|parameter| parameter.ownership), Some(super::ParameterOwnership::WriteBack))
                    || effects.iter().all(|effect| matches!(effect, super::MemoryEffect::Read | super::MemoryEffect::Retain | super::MemoryEffect::Release)))
        }))
}

fn ordinary_own_source_type_supported(source: &witchy_syntax::ast::Type) -> bool {
    match source {
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => {
            matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String")
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            ordinary_own_source_type_supported(&arguments[0])
        }
        _ => false,
    }
}

fn value_type_matches_source(
    actual: super::ValueType,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::FunctionSignature],
    seen: &mut BTreeSet<super::FunctionSignatureId>,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    match source.unqualified() {
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Duration" => actual == super::ValueType::Int,
            "Float" => actual == super::ValueType::Float,
            "Bool" => actual == super::ValueType::Bool,
            "Nil" | "Unit" => actual == super::ValueType::Unit,
            // Canonical lowering assigns the authenticated prelude Console
            // capability the fixed ABI kind 0; no candidate-selected kind is
            // accepted at this source boundary.
            "Console" => actual == super::ValueType::Capability(super::CapabilityKindId::new(0)),
            "String" => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source(layout, source, layouts, records)),
            // A scalar aggregate: one reference to a fixed layout whose slots
            // are exactly the shape's fields, in order.
            _ => witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches(layout, &shape.fields, layouts, records))
            }),
        },
        // A dictionary is its canonical tuple of the key list and the value
        // list, so it matches by that shape like any other aggregate.
        witchy_syntax::ast::Type::Named(name, arguments)
            if matches!(name.as_str(), "Option" | "Result") || (name == "Dict" && arguments.len() == 2) =>
        {
            witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches(layout, &shape.fields, layouts, records))
            })
        }
        witchy_syntax::ast::Type::Tuple(_) => {
            witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches(layout, &shape.fields, layouts, records))
            })
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source(layout, source, layouts, records)),
        // A `fn` value is a closure record: a reference to a layout the
        // module registered with a closure signature, which, past its
        // environment parameter, is the source signature.
        witchy_syntax::ast::Type::Fn(parameters, result, _, _) => match actual {
            super::ValueType::Ref(layout) => {
                let Some(&id) = closure_layouts.get(&layout) else { return false };
                if !seen.insert(id) {
                    return false;
                }
                let matched = signatures
                    .iter()
                    .find(|signature| signature.id == id)
                    .and_then(|signature| strip_closure_environment_with(signature, closure_layouts))
                    .is_some_and(|declaration| {
                        let witchy_syntax::ast::Type::Fn(_, _, conventions, _) = source.unqualified() else { return false };
                        candidate_signature_matches_source_inner(&declaration, parameters, result, conventions, layouts, signatures, seen, records, closure_layouts)
                    });
                seen.remove(&id);
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
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    use crate::layout::{LayoutKind, ScalarKind};
    let Some(descriptor) = layouts.get(layout) else { return false };
    let LayoutKind::PackedRecord { fields: actual } = descriptor.kind() else { return false };
    actual.len() == fields.len()
        && actual.iter().zip(fields).all(|(actual, field)| {
            let Some(actual) = layouts.get(*actual) else { return false };
            match field.unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Duration" | "Float" | "Bool") => match name.as_str() {
                    "Int" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                    "Duration" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Duration)),
                    "Float" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                    _ => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
                },
                // A string, list, tuple, or aggregate field lives behind an
                // owned pointer slot whose pointee is that value's own layout.
                _ => matches!(actual.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::Owned }
                    if layout_matches_source(*pointee, field, layouts, records)),
            }
        })
}

fn layout_matches_source(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, ScalarKind};
    let Some(descriptor) = layouts.get(layout) else { return false };
    // An aggregate (record, sum, tuple, scalar Option/Result) is a fixed
    // record of its shape's slots.
    if let Some(shape) = witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records) {
        return record_layout_matches(layout, &shape.fields, layouts, records);
    }
    match source.unqualified() {
        witchy_syntax::ast::Type::Named(name, arguments) if name == "String" && arguments.is_empty() => {
            let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else { return false };
            layouts.get(*element).is_some_and(|element| matches!(element.kind(), LayoutKind::Scalar(ScalarKind::RawByte)))
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            let LayoutKind::PackedList { element, .. } = descriptor.kind() else { return false };
            let Some(element) = layouts.get(*element) else { return false };
            match arguments[0].unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Float" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Bool" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
                inner => matches!(element.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } if layout_matches_source(*pointee, inner, layouts, records)),
            }
        }
        _ => false,
    }
}

/// Consume the immutable initially-checked graph and recheck that exact graph
/// independently before minting the only typestate accepted by the backend.
/// There is intentionally no transform or history input in milestone one.
pub fn validate_final(
    checked: CheckedInitialSafeWir,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<ValidatedSafeWir, FinalValidationError> {
    validate_final_inner(checked.module, checked.witness, Some(source))
}

fn validate_final_inner(
    module: CanonicalModule<InitialValid>,
    checked_lowering: CheckedLoweringWitness,
    source: Option<&witchy_types::pipeline::CanonicalCheckedSource>,
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
    let exact_module = ExactModuleWitness::derive(&module, &checked_lowering);
    let final_safety_witness = FinalSafeWirWitness {
        exact_module,
        final_module_digest: module_digest(&module),
        final_revision: module.revision(),
    };

    let checked_source_witness = checked_lowering.checked_source_witness.clone();

    Ok(ValidatedSafeWir {
        module,
        checked_lowering,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::canonical_safewir::{
        BasicBlock, BlockId, CallableContract, Function, FunctionId, FunctionSignature,
        FunctionSignatureId, LoweredModuleParts, Operation, ResultContract, Terminator, ValueId,
        ValueType,
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
        let signature = |id| module.signatures().iter().find(|signature| signature.id == id).expect("signature");
        let lanes = strip_closure_environment(signature(adapter_function.signature), module);
        lanes.is_some_and(|lanes| signature_lanes_match(&lanes, signature(functions[&target].signature)))
            && adapter_body_forwards(adapter_function, target, signature(functions[&target].signature))
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
    fn adapter_binding_admits_a_trailing_release_of_the_environment() {
        let (mut operations, terminator) = forwarding_body();
        operations.push(Operation::DropRef { object: ValueId::new(0) });
        let (module, adapter, target) = adapter_candidate(operations, terminator);
        assert!(adapter_shape_holds(&module, adapter, target));
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

    fn source() -> witchy_types::pipeline::CheckedModule {
        witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main() -> Int:\n    3\n".to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source")
    }

    fn candidate_bound_to_source(
        checked: &witchy_types::pipeline::CheckedModule,
    ) -> CanonicalModule<LoweredCandidate> {
        let source = checked.canonical_source().expect("canonical source");
        let mut candidate = candidate();
        candidate.checked_source = CheckedSourceIdentity::from_digest(
            source.source_witness().source_bundle_digest(),
        );
        // A candidate bound to real source carries that source's launch
        // payload; the initial checker compares the two, so a hand-built
        // candidate without it is refused before any other judgment runs.
        let mut candidate = candidate
            .with_launch_contract_payload(source.launch_contract_payload().to_vec());
        let source_abi = source.closed_callable_abi("main").expect("main ABI");
        let witchy_types::canonical_abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy else {
            panic!("main must have a closed target");
        };
        let binding = super::super::CandidateAbiBinding::new(
            FunctionId::new(0),
            target.function_ordinal,
        );
        candidate.with_source_abi_bindings(super::super::CandidateAbiBindings::new(vec![binding]))
    }

    #[test]
    fn checked_source_abi_binding_accepts_the_actual_matching_candidate() {
        let checked = source();
        assert!(check_initial_with_witness(candidate_bound_to_source(&checked), checked.canonical_source().unwrap()).is_ok());
    }

    #[test]
    fn source_own_scalar_rejects_a_unique_candidate_convention() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn helper(own value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    helper(1)\n".to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked source");
        let source = checked.canonical_source().unwrap().closed_callable_signature(0).unwrap();
        let candidate = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Int],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![super::super::ParameterContract {
                    value_type: ValueType::Int,
                    ownership: super::super::ParameterOwnership::Unique,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        assert!(!candidate_signature_matches_source(
            &candidate,
            source,
            &CanonicalModule::from_lowering(LoweredModuleParts {
                revision: Revision::new(1),
                checked_source: CheckedSourceIdentity::from_digest([0; 32]),
                layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
                signatures: vec![candidate.clone()],
                imports: Vec::new(),
                functions: Vec::new(),
                proper_tail_sites: Vec::new(),
            }),
        ));
    }

    #[test]
    fn checked_source_abi_binding_rejects_bool_substitution() {
        let checked = source();
        let mut candidate = candidate_bound_to_source(&checked);
        candidate.signatures[0].results = vec![ValueType::Bool];
        candidate.signatures[0].contract.results = vec![ResultContract::Value(ValueType::Bool)];
        // Rebuild the complete identity-only binding after the substitution.
        // No candidate-controlled ABI shape remains in it to bless the forgery.
        let source_abi = checked.canonical_source().unwrap().closed_callable_abi("main").unwrap();
        let witchy_types::canonical_abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy else { panic!("closed target") };
        candidate.abi_bindings = super::super::CandidateAbiBindings::new(vec![
            super::super::CandidateAbiBinding::new(FunctionId::new(0), target.function_ordinal),
        ]);
        assert!(matches!(
            check_initial_with_witness(candidate, checked.canonical_source().unwrap()),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::CandidateSignatureMismatch { .. }))
        ));
    }

    #[test]
    fn checked_source_abi_binding_rejects_an_extra_result_lane() {
        let checked = source();
        let mut candidate = candidate_bound_to_source(&checked);
        candidate.signatures[0].results.push(ValueType::Bool);
        candidate.signatures[0].contract.results.push(ResultContract::Value(ValueType::Bool));
        assert!(matches!(
            check_initial_with_witness(candidate, checked.canonical_source().unwrap()),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::CandidateSignatureMismatch { .. }))
        ));
    }

    #[test]
    fn source_console_requires_the_authenticated_capability_kind() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main(console: Console):\n    console.print(\"ok\")\n".to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap();
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = source.closed_callable_signature(0).unwrap().unqualified() else { panic!("function") };
        assert!(!value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(1)), &parameters[0], &LayoutInterner::new(), &[], &mut BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new()));
        assert!(value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(0)), &parameters[0], &LayoutInterner::new(), &[], &mut BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new()));
    }

    /// A closure record of one code slot and no captures.
    fn closure_record_layout() -> crate::layout::LayoutId {
        let mut interner = LayoutInterner::new();
        let code = interner.intern_scalar(crate::layout::ScalarKind::Callable).unwrap();
        interner.intern_packed_record(vec![code]).unwrap()
    }

    #[test]
    fn source_higher_order_callable_matches_the_actual_nested_signature() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("apply".to_string(), b"fn apply(f: fn(Int) -> Int) -> Int:\n    f(1)\n".to_vec())],
            "apply", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap().closed_callable_signature(0).unwrap();
        // The `fn(Int) -> Int` parameter is a closure record whose registered
        // signature takes the record first.
        let closure = closure_record_layout();
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Ref(closure), ValueType::Int], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }, super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(closure)], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate().with_closure_layouts([(closure, FunctionSignatureId::new(1))].into_iter().collect());
        candidate.signatures = vec![outer, inner];
        assert!(candidate_signature_matches_source(&candidate.signatures[0], source, &candidate));
    }

    #[test]
    fn source_higher_order_unit_callable_requires_zero_result_lanes() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("apply".to_string(), b"fn apply(f: fn(Int) -> Nil):\n    f(1)\n".to_vec())],
            "apply", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap().closed_callable_signature(0).unwrap();
        let closure = closure_record_layout();
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Ref(closure), ValueType::Int], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }, super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(closure)], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }], results: vec![], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate().with_closure_layouts([(closure, FunctionSignatureId::new(1))].into_iter().collect());
        candidate.signatures = vec![outer, inner];
        assert!(candidate_signature_matches_source(&candidate.signatures[0], source, &candidate));
    }

    #[test]
    fn checked_source_abi_binding_rejects_missing_and_duplicate_entries() {
        let checked = source();
        let source = checked.canonical_source().unwrap();
        let mut missing = candidate_bound_to_source(&checked);
        missing.abi_bindings = super::super::CandidateAbiBindings::default();
        assert!(matches!(
            check_initial_with_witness(missing, source),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::MissingBinding { .. }))
        ));

        let candidate = candidate_bound_to_source(&checked);
        let binding = candidate.source_abi_bindings().entries()[0].clone();
        let mut duplicate = candidate;
        duplicate.abi_bindings = super::super::CandidateAbiBindings::new(vec![binding.clone(), binding]);
        assert!(matches!(
            check_initial_with_witness(duplicate, source),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::DuplicateSourceDeclaration { .. }))
        ));
    }

    #[test]
    fn final_authority_requires_the_exact_initial_checked_module() {
        let checked = check_initial_with_test_witness(candidate()).unwrap();
        let initial_snapshot = checked.module().clone();
        let final_module = validate_final_for_test(checked).unwrap();
        assert_eq!(final_module.checked_source(), initial_snapshot.checked_source());
    }

    #[test]
    fn witness_from_another_checked_module_is_rejected() {
        let checked = check_initial_with_test_witness(candidate()).unwrap();
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
    fn final_authority_refuses_a_mismatched_closure_witness() {
        let checked_source = source();
        let source = checked_source.canonical_source().unwrap();
        let mut checked = check_initial_with_witness(candidate_bound_to_source(&checked_source), source)
            .expect("initial authority");
        checked.witness.checked_source_witness.closure_witness_digest = [0xa5; 32];

        assert!(matches!(
            validate_final(checked, source),
            Err(FinalValidationError::SourceWitnessDoesNotMatchInitial)
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
}
