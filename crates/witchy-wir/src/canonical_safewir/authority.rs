use std::collections::{BTreeMap, BTreeSet};
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
    checked_source_witness: CheckedSourceWitness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CandidateId([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedSourceWitness {
    checked_source: CheckedSourceIdentity,
    initial_module_digest: ModuleDigest,
    proper_tail_manifest_digest: ModuleDigest,
    callable_abi_manifest_digest: [u8; 32],
}

/// Evidence returned only by the independent final SafeWIR checker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalSafeWirWitness {
    candidate: CandidateId,
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

    pub(crate) fn validate_pre_admission(
        &self,
    ) -> Result<(), AdmissionWitnessMismatch> {
        let expected_candidate = CandidateId::derive(&self.module, &self.optimization_history);
        if self.candidate != expected_candidate {
            return Err(AdmissionWitnessMismatch::Candidate);
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
        {
            return Err(AdmissionWitnessMismatch::CheckedSource);
        }
        Ok(())
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
}

impl fmt::Display for InitialWitnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Safety(error) => error.fmt(formatter),
            Self::CandidateAbi(error) => error.fmt(formatter),
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

fn validate_candidate_abi_bindings(
    candidate: &CanonicalModule<LoweredCandidate>,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<(), CandidateAbiBindingError> {
    use witchy_types::canonical_abi::CanonicalTargetPolicy;

    let source_entries = source.callable_abi_manifest().entries().iter().filter_map(|entry| {
        let CanonicalTargetPolicy::ClosedLocal(target) = entry.target_policy else { return None };
        Some((target.function_ordinal, entry.key.as_bytes()))
    }).collect::<BTreeMap<_, _>>();
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
        if !candidate_signature_matches_source(signature, source_signature, candidate) {
            return Err(CandidateAbiBindingError::CandidateSignatureMismatch { function: binding.function() });
        }
    }
    if functions.len() != bound_functions.len() {
        let function = functions.keys().find(|function| !bound_functions.contains_key(function))
            .copied().unwrap_or_else(|| *bound_functions.keys().next().expect("nonempty mismatch"));
        return Err(CandidateAbiBindingError::MissingCandidateFunction { function });
    }
    Ok(())
}

fn candidate_signature_matches_source(
    actual: &super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<LoweredCandidate>,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    let (_, layouts) = match crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes()) { Ok(value) => value, Err(_) => return false };
    candidate_signature_matches_source_inner(actual, parameters, result, conventions, &layouts, candidate.signatures(), &mut BTreeSet::new())
}

fn candidate_signature_matches_source_inner(
    actual: &super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::FunctionSignature],
    seen: &mut BTreeSet<super::FunctionSignatureId>,
) -> bool {
    if actual.parameters.len() != parameters.len() || actual.contract.parameters.len() != parameters.len() {
        return false;
    }
    let convention_at = |index| conventions.get(index).copied().unwrap_or_default();
    if !actual.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        (matches!(convention, witchy_syntax::ast::Convention::Let)
            || matches!(convention, witchy_syntax::ast::Convention::Own) && ordinary_own_source_type_supported(source))
            && value_type_matches_source(*actual, source, layouts, signatures, seen)
    }) { return false; }
    if !actual.contract.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        if !(matches!(convention, witchy_syntax::ast::Convention::Let)
            || matches!(convention, witchy_syntax::ast::Convention::Own) && ordinary_own_source_type_supported(source))
            || !value_type_matches_source(actual.value_type, source, layouts, signatures, seen) { return false; }
        match (actual.value_type, convention) {
            (super::ValueType::Ref(_), witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            (_, witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::ParameterOwnership::Value),
            (super::ValueType::Ref(_), _) => matches!(actual.ownership, super::ParameterOwnership::Shared),
            _ => matches!(actual.ownership, super::ParameterOwnership::Value),
        }
    }) { return false; }
    let result_is_unit = matches!(result.unqualified(), witchy_syntax::ast::Type::Named(name, arguments) if (name == "Nil" || name == "Unit") && arguments.is_empty());
    (if result_is_unit { actual.results.is_empty() } else {
        actual.results.len() == 1 && value_type_matches_source(actual.results[0], result, layouts, signatures, seen)
    }) && actual.contract.parameter_effects.iter().all(|(parameter, effects)| {
        matches!(actual.parameters.get(*parameter as usize), Some(super::ValueType::Ref(_)))
            && effects.iter().all(|effect| matches!(effect, super::MemoryEffect::Read | super::MemoryEffect::Retain | super::MemoryEffect::Release))
    })
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
            "String" => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source(layout, source, layouts)),
            _ => false,
        },
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => matches!(actual, super::ValueType::Ref(layout) if layout_matches_source(layout, source, layouts)),
        witchy_syntax::ast::Type::Fn(parameters, result, _, _) => match actual {
            super::ValueType::Callable(id) if seen.insert(id) => {
                let matched = signatures.iter().find(|signature| signature.id == id).is_some_and(|signature| {
                    let witchy_syntax::ast::Type::Fn(_, _, conventions, _) = source.unqualified() else { return false };
                    candidate_signature_matches_source_inner(signature, parameters, result, conventions, layouts, signatures, seen)
                });
                seen.remove(&id);
                matched
            }
            _ => false,
        },
        _ => false,
    }
}

fn layout_matches_source(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, ScalarKind};
    let Some(descriptor) = layouts.get(layout) else { return false };
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
                witchy_syntax::ast::Type::Named(name, arguments) if name == "Bool" && arguments.is_empty() => matches!(element.kind(), LayoutKind::Scalar(ScalarKind::BoolValueSlot)),
                inner => matches!(element.kind(), LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } if layout_matches_source(*pointee, inner, layouts)),
            }
        }
        _ => false,
    }
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

    let checked_source_witness = checked_lowering.checked_source_witness.clone();

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
    let checked_source_witness = CheckedSourceWitness {
        checked_source: module.checked_source(),
        initial_module_digest: module_digest(&module),
        proper_tail_manifest_digest: expected,
        callable_abi_manifest_digest: [0; 32],
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
            vec![("main".to_string(), b"fn main(own value: Int) -> Int:\n    value\n".to_vec())],
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
        assert!(!value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(1)), &parameters[0], &LayoutInterner::new(), &[], &mut BTreeSet::new()));
        assert!(value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(0)), &parameters[0], &LayoutInterner::new(), &[], &mut BTreeSet::new()));
    }

    #[test]
    fn source_higher_order_callable_matches_the_actual_nested_signature() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("apply".to_string(), b"fn apply(f: fn(Int) -> Int) -> Int:\n    f(1)\n".to_vec())],
            "apply", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = checked.canonical_source().unwrap().closed_callable_signature(0).unwrap();
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Int], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Callable(FunctionSignatureId::new(1))], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Callable(FunctionSignatureId::new(1)), ownership: super::super::ParameterOwnership::Value }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate();
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
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Int], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Callable(FunctionSignatureId::new(1))], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Callable(FunctionSignatureId::new(1)), ownership: super::super::ParameterOwnership::Value }], results: vec![], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate();
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
