use super::*;
use crate::canonical_safewir::check::{AffineAbiProofs, AffinePhysicalPlan, AffineSignatureProof};
use witchy_canonical_source::storage::{AffineStorageShape, affine_storage_shape};
use witchy_syntax::ast::{Convention, Type};

/// Bind the checked source's affine field judgment to the already validated
/// physical ABI. This is called only after candidate ABI correspondence has
/// succeeded; a raw SafeWIR module has no source-derived child proof.
pub(super) fn affine_abi_proofs<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> Result<AffineAbiProofs, CandidateAbiBindingError> {
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&candidate.layouts().canonical_bytes())
        .map_err(|_| CandidateAbiBindingError::MissingCanonicalLayouts)?;
    let mut proofs = AffineAbiProofs::new();
    for binding in candidate.source_abi_bindings().entries() {
        let ordinal = binding.source_declaration_ordinal();
        let function = candidate.functions().iter().find(|function| function.id == binding.function())
            .ok_or(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() })?;
        let signature = candidate.signatures().iter().find(|signature| signature.id == function.signature)
            .ok_or(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() })?;
        let Some(Type::Fn(parameters, result, conventions, _)) = source.closed_callable_signature(ordinal) else {
            return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
        };
        let mut proof = AffineSignatureProof {
            parameters: vec![None; signature.parameters.len()],
            results: vec![None; signature.results.len()],
        };
        for (index, ty) in parameters.iter().enumerate() {
            let Some(shape) = affine_storage_shape(ty, source.type_definitions()) else {
                if source.storage_plan(ty.unqualified()).requires_affine_owner_token() {
                    return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
                }
                continue;
            };
            let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.parameters.get(index) else {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            proof.parameters[index] = Some(bind_plan(ty, *layout, &shape, &layouts, source, &mut proofs.layouts)
                .ok_or(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal })?);
        }
        if let Some(shape) = affine_storage_shape(result, source.type_definitions()) {
            let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.results.first() else {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            proof.results[0] = Some(bind_plan(result, *layout, &shape, &layouts, source, &mut proofs.layouts)
                .ok_or(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal })?);
        } else if source.storage_plan(result.unqualified()).requires_affine_owner_token() {
            return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
        }
        // A `var unique` parameter returns its replacement in an additional
        // ABI lane. Bind that lane by the source parameter's write-back
        // ordinal, then require a unique, layout-equal candidate result.
        let writebacks = parameters.iter().enumerate().filter_map(|(index, _)|
            (conventions.get(index).copied().unwrap_or(Convention::Let) == Convention::Var)
                .then_some(index)).collect::<Vec<_>>();
        let Some(hidden_start) = signature.results.len().checked_sub(writebacks.len()) else {
            return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
        };
        for (parameter_index, parameter_proof) in proof.parameters.iter().enumerate() {
            let Some(parameter_proof) = parameter_proof else { continue };
            let Some(crate::canonical_safewir::ParameterContract {
                value_type: crate::canonical_safewir::ValueType::Ref(parameter_layout),
                ownership: crate::canonical_safewir::ParameterOwnership::UniqueWriteBack,
            }) = signature.contract.parameters.get(parameter_index) else { continue };
            let Some(offset) = writebacks.iter().position(|candidate| *candidate == parameter_index) else {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            let result_index = hidden_start + offset;
            if !matches!(signature.contract.results.get(result_index),
                Some(crate::canonical_safewir::ResultContract::Unique { layout }) if layout == parameter_layout)
            {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
            proof.results[result_index] = Some(parameter_proof.clone());
        }
        if proof.parameters.iter().chain(&proof.results).any(Option::is_some) {
            if proofs.signatures.insert(signature.id, proof.clone()).is_some_and(|prior| prior != proof) {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
        }
    }
    Ok(proofs)
}

fn bind_plan(
    source_type: &Type,
    layout: crate::layout::LayoutId,
    shape: &AffineStorageShape,
    layouts: &crate::layout::LayoutInterner,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    catalog: &mut BTreeMap<crate::layout::LayoutId, AffinePhysicalPlan>,
) -> Option<AffinePhysicalPlan> {
    use crate::layout::{FieldKind, LayoutKind, PointerSlot, RcHeader};
    let physical = layouts.resolve_nominal(layout).ok()?;
    let descriptor = layouts.get(physical)?;
    let plan = match shape {
        AffineStorageShape::Leaf => {
            let source_shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions())?;
            if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. })
                || descriptor.fields().len() != source_shape.fields.len() { return None; }
            AffinePhysicalPlan::Leaf
        }
        AffineStorageShape::NullableFields(children) => {
            let source_shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions())?;
            if !source_shape.tagged || !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. })
                || descriptor.fields().len() != source_shape.fields.len() { return None; }
            let mut bound = Vec::with_capacity(children.len());
            for (index, child) in children {
                let field = descriptor.fields().get(*index as usize)?;
                let pointee = match field.kind() {
                    FieldKind::Pointer { pointee, slot: PointerSlot::OwnedNullable } => pointee,
                    FieldKind::Inline(slot) => match layouts.get(slot)?.kind() {
                        LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::OwnedNullable } => *pointee,
                        _ => return None,
                    },
                    _ => return None,
                };
                let child_source = source_shape.fields.get(*index as usize)?;
                bound.push((*index, pointee, Box::new(bind_plan(child_source, pointee, child, layouts, source, catalog)?)));
            }
            AffinePhysicalPlan::NullableFields(bound)
        }
        AffineStorageShape::Sequence(child) => {
            let witchy_canonical_source::storage::StoragePlan::MoveOnlyList(sequence) = source.storage_plan(source_type.unqualified()) else { return None };
            let Type::Named(_, arguments) = sequence.source() else { return None };
            let [child_source] = arguments.as_slice() else { return None };
            let LayoutKind::PackedList { element, rc: RcHeader::Required } = descriptor.kind() else { return None };
            let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } = layouts.get(*element)?.kind() else { return None };
            AffinePhysicalPlan::Sequence {
                element: *pointee,
                child: Box::new(bind_plan(child_source, *pointee, child, layouts, source, catalog)?),
            }
        }
    };
    if catalog.insert(layout, plan.clone()).is_some_and(|prior| prior != plan) { return None; }
    Some(plan)
}
