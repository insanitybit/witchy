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
    // witness instead of a source declaration.  The uniform closure record
    // owns a tagged environment record; captures are its checked scalar or
    // managed-reference payloads, never capabilities or an erased value.
    let closures = source.closure_witness().entries();
    let (_bundle, closure_layout_catalog) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) {
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
        let Some(source_signature) = witchy_types::typeck::ty_to_ast(witness.signature()) else {
            return Err(CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() });
        };
        let family = closures.iter().filter(|entry| {
            witchy_types::typeck::ty_to_ast(entry.signature()).as_ref() == Some(&source_signature)
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
        let expected_field_types = expected.iter().enumerate().map(|(index, capture)| {
                let field = *environment_fields.get(index + 1)?;
                let actual = match closure_layout_catalog.get(field).map(|field| field.kind()) {
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration)) => super::ValueType::Int,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float)) => super::ValueType::Float,
                    Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool)) => super::ValueType::Bool,
                    Some(crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::OwnedNullable }) => super::ValueType::Ref(*pointee),
                    _ => return None,
                };
                let transfer_matches = matches!(
                    (actual, capture.transfer()),
                    (super::ValueType::Ref(_), witchy_types::canonical_closure::ClosureCaptureTransfer::ManagedReferenceByValue)
                        | (super::ValueType::Int | super::ValueType::Float | super::ValueType::Bool,
                           witchy_types::canonical_closure::ClosureCaptureTransfer::CopyScalar)
                );
                (transfer_matches && value_type_matches_source(
                    actual, capture.checked_type(), &closure_layout_catalog, candidate.signatures(), &mut BTreeSet::new(),
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
        // A capability token is a scalar-like value: copied on pass, never
        // released, so every convention carries it. `Dir` names its rights in
        // type arguments, which change no ABI fact about the token.
        witchy_syntax::ast::Type::Named(name, _) if matches!(name.as_str(), "Dir" | "File") => true,
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => {
            matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String" | "Bytes" | "Console" | "Env")
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
    if let Some(representation) = witchy_types::canonical_capabilities::representation(source, records) {
        use witchy_types::canonical_capabilities::{CapabilityKind, CapabilityRepresentation};
        let kind = match representation {
            CapabilityRepresentation::Direct(CapabilityKind::Console) | CapabilityRepresentation::Nullable(CapabilityKind::Console) => super::CapabilityKindId::new(0),
            CapabilityRepresentation::Direct(CapabilityKind::Dir) | CapabilityRepresentation::Nullable(CapabilityKind::Dir) => super::DIR_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Env) | CapabilityRepresentation::Nullable(CapabilityKind::Env) => super::ENV_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::File) | CapabilityRepresentation::Nullable(CapabilityKind::File) => super::FILE_CAPABILITY_KIND,
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
            "String" | "Bytes" => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts)),
            // A scalar aggregate: one reference to a fixed layout whose slots
            // are exactly the shape's fields, in order.
            _ => witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts))
            }),
        },
        // A dictionary is its canonical tuple of the key list and the value
        // list, so it matches by that shape like any other aggregate.
        witchy_syntax::ast::Type::Named(name, arguments)
            if matches!(name.as_str(), "Option" | "Result") || (name == "Dict" && arguments.len() == 2) =>
        {
            witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts))
            })
        }
        witchy_syntax::ast::Type::Tuple(_) => {
            witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts))
            })
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts)),
        // Concrete user generics use the same independently matched field
        // graph as other aggregates. A type name or its arguments alone never
        // grant a candidate reference its layout authority.
        witchy_syntax::ast::Type::Named(_, _) => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts)),
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
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    record_layout_matches_with_closures(layout, fields, nullable_fields, layouts, records, &BTreeMap::new())
}

fn record_layout_matches_with_closures(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    let mut pending = Vec::new();
    if !enqueue_record_fields_with_closures(layout, fields, nullable_fields, layouts, &mut pending, closure_layouts) {
        return false;
    }
    match_layout_graph(pending, layouts, records, closure_layouts)
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
            match field.unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() && matches!(name.as_str(), "Int" | "Duration" | "Float" | "Bool") => match name.as_str() {
                    "Int" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                    "Duration" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Duration)),
                    "Float" => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                    _ => matches!(actual.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
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
    layout_matches_source_with_closures(layout, source, layouts, records, &BTreeMap::new())
}

fn layout_matches_source_with_closures(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    match_layout_graph(vec![(layout, source.unqualified().clone())], layouts, records, closure_layouts)
}

/// Check every node in the finite layout/source-type product. A revisited
/// exact pair closes a recursive edge only: all other outgoing obligations
/// still have to pass. Nominal resolution never replaces source validation.
fn match_layout_graph(
    mut pending: Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    while let Some((layout, source)) = pending.pop() {
        if seen.contains(&(layout, source.clone())) {
            continue;
        }
        // A resource ceiling refuses pathological expansion; it never turns
        // unfinished correspondence into successful admission.
        if seen.len() == 65_536 {
            return false;
        }
        seen.insert((layout, source.clone()));
        if !enqueue_layout_children(layout, &source, layouts, records, &mut pending, closure_layouts) {
            return false;
        }
    }
    true
}

fn enqueue_layout_children(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::FunctionSignatureId>,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, ScalarKind};
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else { return false };
    // An aggregate (record, sum, tuple, scalar Option/Result) is a fixed
    // record of its shape's slots.
    if let Some(shape) = witchy_types::canonical_aggregates::scalar_aggregate_shape(source, records) {
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
            match arguments[0].unqualified() {
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Int" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Int)),
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Float" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Float)),
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Bool" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::Bool)),
                inner => matches!(element.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } if { pending.push((*pointee, inner.unqualified().clone())); true }),
            }
        }
        witchy_syntax::ast::Type::Fn(..) => {
            let LayoutKind::PackedRecord { fields } = descriptor.kind() else { return false };
            if fields.len() != 2 || !closure_layouts.contains_key(&layout) {
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
        use witchy_types::canonical_aggregates::ScalarAggregateShape;
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
        use witchy_types::canonical_aggregates::ScalarAggregateShape;
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
                &[], &mut BTreeSet::new(), &records, &BTreeMap::new(),
            ));
            assert!(!value_type_matches_source(
                super::super::ValueType::Ref(wrong), &source, &layouts,
                &[], &mut BTreeSet::new(), &records, &BTreeMap::new(),
            ));
        }
        assert!(!layout_matches_source(int_box, &Type::Named("Box".into(), vec![named("Missing")]), &layouts, &records));
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

    /// `Dir` matches kind 1 whatever rights it names, and `Env` kind 2. The
    /// rights live in the host's authority object, so they change no ABI
    /// fact about the token the entry receives.
    #[test]
    fn source_dir_and_env_match_their_authenticated_capability_kinds() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main(dir: Dir[Read], env: Env):
    let _ = dir.exists(\"a\")
".to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap();
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = source.closed_callable_signature(0).unwrap().unqualified() else { panic!("function") };
        let matches = |actual, source: &witchy_syntax::ast::Type| {
            value_type_matches_source(actual, source, &LayoutInterner::new(), &[], &mut BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new())
        };
        assert!(matches(ValueType::Capability(super::super::DIR_CAPABILITY_KIND), &parameters[0]));
        assert!(!matches(ValueType::Capability(super::super::CapabilityKindId::new(0)), &parameters[0]));
        assert!(matches(ValueType::Capability(super::super::ENV_CAPABILITY_KIND), &parameters[1]));
        assert!(!matches(ValueType::Capability(super::super::DIR_CAPABILITY_KIND), &parameters[1]));
        // Both are admissible as ordinary `own` parameters too.
        assert!(ordinary_own_source_type_supported(&parameters[0]));
        assert!(ordinary_own_source_type_supported(&parameters[1]));
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
