//! Independent checked-source binding for opaque closure constructors.
//! Candidate IDs and field maps are claims, never capability authority.
use super::authority::*;
use super::closure_owner_contract::{CaptureStorage, CaptureTransfer, OwnerOperation, SourceConstructor};
use super::{CanonicalModule, ModuleStage, Operation, ValueId, ValueType};
use std::collections::{BTreeMap, BTreeSet};
use witchy_types::canonical_closure::ClosureBindingPathStep;

pub(super) fn validate<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_types::pipeline::CanonicalCheckedSource,
) -> Result<(), CandidateAbiBindingError> {
    let records = witchy_types::canonical_aggregates::type_definitions(source.typed_module().module());
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
        .map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
    let witnesses = source.closure_witness().entries();
    let mut seen = BTreeSet::new();
    let mut family_keys = std::collections::HashSet::new();
    for family in candidate.owner_families() {
        let mut source_key = None;
        let mut observed = BTreeSet::new();
        for constructor in &family.constructors {
            let Some(closure) = constructor.witness else {
                if constructor.environment_tag != 0 || !constructor.captures.is_empty() {
                    return Err(CandidateAbiBindingError::AdapterShapeMismatch { adapter: constructor.function });
                }
                continue;
            };
            let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure };
            if constructor.id.0 != closure || !seen.insert(closure) { return Err(refuse()); }
            let checked = SourceConstructor::from_checked(source, constructor.id).map_err(|_| refuse())?;
            let witness = checked.witness();
            let signature = witchy_types::typeck::ty_to_ast(witness.signature()).ok_or_else(refuse)?;
            let key = source_closure_family_key(&signature, &records);
            if source_key.as_ref().is_some_and(|previous| previous != &key) { return Err(refuse()); }
            source_key = Some(key.clone());
            observed.insert(closure);
            if !candidate.closure_bindings().entries().iter().any(|b| b.function() == constructor.function && b.closure() == closure) {
                return Err(refuse());
            }
            let declaration = candidate.signatures().iter().find(|s| s.id == family.declaration_signature).ok_or_else(refuse)?;
            if !candidate_signature_matches_source_with_records(declaration, &signature, candidate, &records)
                || constructor.captures.len() != witness.captures().len()
            { return Err(refuse()); }
            let related = witnesses.iter().enumerate().filter(|(_, w)| {
                witchy_types::typeck::ty_to_ast(w.signature()).is_some_and(|s| source_closure_family_key(&s, &records) == key)
            }).collect::<Vec<_>>();
            let position = related.iter().position(|(ordinal, _)| *ordinal == closure as usize).ok_or_else(refuse)?;
            if constructor.environment_tag != i64::try_from(position + 1).map_err(|_| refuse())? { return Err(refuse()); }
            let mut environment_field = 1 + related[..position].iter().map(|(ordinal, _)| {
                family.constructors.iter().find(|constructor| constructor.witness == Some(*ordinal as u32))
                    .map(|constructor| constructor.captures.iter().filter(|capture| {
                        matches!(capture.storage, CaptureStorage::EnvironmentField(_))
                    }).count())
                    .unwrap_or(0)
            }).sum::<usize>();
            let mut owner_field = 2;
            for ((capture, original), transfer) in constructor.captures.iter().zip(witness.captures()).zip(checked.transfers()) {
                let type_matches = value_type_matches_source(capture.value_type, original.checked_type(), &layouts,
                    candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), &records, candidate.closure_layouts());
                let expected_storage = if matches!(transfer, CaptureTransfer::CopyCapability(_))
                    || matches!((transfer, capture.value_type), (CaptureTransfer::CloneManaged, ValueType::Gc(_)))
                {
                    let field = owner_field;
                    owner_field += 1;
                    CaptureStorage::OwnerField(field)
                } else {
                    let field = u32::try_from(environment_field).map_err(|_| refuse())?;
                    environment_field += 1;
                    CaptureStorage::EnvironmentField(field)
                };
                if capture.transfer != *transfer || capture.storage != expected_storage || !type_matches
                { return Err(refuse()); }
            }
        }
        let key = source_key.ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
        if !family_keys.insert(key.clone()) { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        let expected = witnesses.iter().enumerate().filter_map(|(index, witness)| {
            witchy_types::typeck::ty_to_ast(witness.signature())
                .filter(|s| source_closure_family_key(s, &records) == key).map(|_| index as u32)
        }).collect::<BTreeSet<_>>();
        if expected != observed || !expected.iter().any(|index| witnesses[*index as usize].captures().iter().any(|c|
            c.transfer() == witchy_types::canonical_closure::ClosureCaptureTransfer::CopyCapability))
        { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        let physical = layouts.resolve_nominal(family.environment).map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
        let fields = match layouts.get(physical).map(|d| d.kind()) {
            Some(crate::layout::LayoutKind::PackedRecord { fields }) => fields,
            _ => return Err(CandidateAbiBindingError::MissingCanonicalLayouts),
        };
        if !fields.first().is_some_and(|id| matches!(layouts.get(*id).map(|d| d.kind()), Some(crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int)))) {
            return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
        }
        let captures = family.constructors.iter().flat_map(|c| &c.captures).filter_map(|capture| {
            if let CaptureStorage::EnvironmentField(index) = capture.storage { Some((index, capture.value_type)) } else { None }
        }).collect::<BTreeMap<_, _>>();
        if fields.len() != captures.len() + 1 { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        for (index, ty) in captures {
            let descriptor = fields.get(index as usize).and_then(|id| layouts.get(*id)).ok_or(CandidateAbiBindingError::MissingCanonicalLayouts)?;
            let matches = match (ty, descriptor.kind()) {
                (ValueType::Int, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int | crate::layout::ScalarKind::Duration))
                | (ValueType::Float, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Float))
                | (ValueType::Bool, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Bool))
                | (ValueType::Unit, crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Unit)) => true,
                (ValueType::Ref(layout), crate::layout::LayoutKind::RuntimePointerValueSlot { pointee, semantics: crate::layout::PointerSlot::OwnedNullable }) => layout == *pointee,
                _ => false,
            };
            if !matches { return Err(CandidateAbiBindingError::MissingCanonicalLayouts); }
        }
    }
    for function in candidate.functions() {
        for block in &function.blocks {
            for operation in &block.operations {
                match operation {
                    Operation::ClosureOwner(OwnerOperation::Construct { constructor, captures, .. }) => {
                        let checked = SourceConstructor::from_checked(source, *constructor)
                            .map_err(|_| CandidateAbiBindingError::MissingClosureWitness { closure: constructor.0 })?;
                        let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 };
                        if captures.len() != checked.witness().captures().len() { return Err(refuse()); }
                        for (value, capture) in captures.iter().zip(checked.witness().captures()) {
                            // Lexical identity is checked independently, not
                            // inferred from an SSA name or matching type.
                            let binding = capture.binding();
                            let [ClosureBindingPathStep::Parameter(index)] = binding.path() else { return Err(refuse()); };
                            let (expected_function, offset) = if binding.owner().lambda_path.is_empty() && binding.owner().method.is_none() {
                                (candidate.source_abi_bindings().entries().iter().find(|b| b.source_declaration_ordinal() == binding.owner().item)
                                    .map(|b| b.function()).ok_or_else(refuse)?, 0)
                            } else {
                                let ordinal = witnesses.iter().position(|w| w.id() == binding.owner()).ok_or_else(refuse)?;
                                (candidate.closure_bindings().entries().iter().find(|b| b.closure() == ordinal as u32)
                                    .map(|b| b.function()).ok_or_else(refuse)?, 1)
                            };
                            if function.id != expected_function { return Err(refuse()); }
                            let parameter = function.blocks.iter().find(|b| b.id == function.entry)
                                .and_then(|b| b.parameters.get(*index as usize + offset)).ok_or_else(refuse)?;
                            if !same_parameter(function, *value, parameter.value, &mut BTreeSet::new()) { return Err(refuse()); }
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadCapability { owner, constructor, .. }
                        | OwnerOperation::CloneEnvironment { owner, constructor, .. }) => {
                        let declared = candidate.owner_families().iter().flat_map(|f| &f.constructors).find(|c| c.id == *constructor)
                            .ok_or(CandidateAbiBindingError::MissingClosureWitness { closure: constructor.0 })?;
                        let parameter = function.blocks.iter().find(|b| b.id == function.entry).and_then(|b| b.parameters.first());
                        if function.id != declared.function || parameter.is_none_or(|p| p.value != *owner) {
                            return Err(CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 });
                        }
                    }
                    Operation::ClosureOwner(OwnerOperation::ReadOwner { owner, constructor, capture, .. }) => {
                        let refuse = || CandidateAbiBindingError::CapturingClosureBinding { closure: constructor.0 };
                        let declared = candidate.owner_families().iter().flat_map(|f| &f.constructors)
                            .find(|c| c.id == *constructor).ok_or_else(refuse)?;
                        let parameter = function.blocks.iter().find(|b| b.id == function.entry)
                            .and_then(|b| b.parameters.first());
                        let source_constructor = SourceConstructor::from_checked(source, *constructor).map_err(|_| refuse())?;
                        let Some(source_capture) = source_constructor.witness().captures().get(capture.0 as usize) else {
                            return Err(refuse());
                        };
                        let Some(declared_capture) = declared.captures.get(capture.0 as usize) else {
                            return Err(refuse());
                        };
                        if function.id != declared.function
                            || parameter.is_none_or(|p| p.value != *owner)
                            || source_capture.transfer() != witchy_types::canonical_closure::ClosureCaptureTransfer::ManagedReferenceByValue
                            || !matches!((declared_capture.value_type, declared_capture.storage, declared_capture.transfer),
                                (ValueType::Gc(_), CaptureStorage::OwnerField(_), CaptureTransfer::CloneManaged))
                            || !value_type_matches_source(declared_capture.value_type, source_capture.checked_type(), &layouts,
                                candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), &records,
                                candidate.closure_layouts())
                        {
                            return Err(refuse());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn same_parameter(function: &super::Function, value: ValueId, parameter: ValueId, active: &mut BTreeSet<ValueId>) -> bool {
    if value == parameter { return true; }
    if !active.insert(value) { return false; }
    let matches = function.blocks.iter().flat_map(|b| &b.operations).find_map(|operation| match operation {
        Operation::CloneRef { destination, source } | Operation::MoveValue { destination, source } if *destination == value => {
            Some(same_parameter(function, *source, parameter, active))
        }
        _ => None,
    }).unwrap_or(false);
    active.remove(&value);
    matches
}
