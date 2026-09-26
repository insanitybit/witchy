use super::*;
use crate::canonical_safewir::check::{AffineAbiProofs, AffineSignatureProof};
use witchy_canonical_source::storage::affine_nullable_fields;
use witchy_canonical_source::storage::StoragePlan;
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
            parameters: vec![Vec::new(); signature.parameters.len()],
            results: vec![Vec::new(); signature.results.len()],
            move_only_list_parameters: vec![false; signature.parameters.len()],
            move_only_list_results: vec![false; signature.results.len()],
        };
        for (index, ty) in parameters.iter().enumerate() {
            if matches!(source.storage_plan(ty.unqualified()), StoragePlan::MoveOnlyList(_)) {
                let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.parameters.get(index) else {
                    return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
                };
                if !move_only_list_layout(*layout, &layouts) {
                    return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
                }
                proof.move_only_list_parameters[index] = true;
            }
            let Some(fields) = affine_nullable_fields(ty, source.type_definitions()) else { continue };
            if fields.is_empty() { continue; }
            let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.parameters.get(index) else {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            if !fields_match_storage(*layout, ty, &fields, &layouts, source) {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
            proof.parameters[index] = fields;
        }
        if matches!(source.storage_plan(result.unqualified()), StoragePlan::MoveOnlyList(_)) {
            let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.results.first() else {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            if !move_only_list_layout(*layout, &layouts) {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
            proof.move_only_list_results[0] = true;
        }
        if let Some(fields) = affine_nullable_fields(result, source.type_definitions()) {
            if !fields.is_empty() {
                let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.results.first() else {
                    return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
                };
                if !fields_match_storage(*layout, result, &fields, &layouts, source) {
                    return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
                }
                proof.results[0] = fields;
            }
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
        for (parameter_index, parameter_type) in parameters.iter().enumerate() {
            let list = proof.move_only_list_parameters.get(parameter_index) == Some(&true);
            let fields = proof.parameters.get(parameter_index).cloned().unwrap_or_default();
            if !list && fields.is_empty() { continue; }
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
            if !fields.is_empty() && !fields_match_storage(*parameter_layout, parameter_type, &fields, &layouts, source) {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
            proof.results[result_index] = fields;
            proof.move_only_list_results[result_index] = list;
        }
        if proof.parameters.iter().chain(&proof.results).any(|fields| !fields.is_empty())
            || proof.move_only_list_parameters.iter().chain(&proof.move_only_list_results).any(|marked| *marked) {
            if proofs.insert(signature.id, proof.clone()).is_some_and(|prior| prior != proof) {
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            }
        }
    }
    Ok(proofs)
}

fn move_only_list_layout(layout: crate::layout::LayoutId, layouts: &crate::layout::LayoutInterner) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, RcHeader};
    let Ok(physical) = layouts.resolve_nominal(layout) else { return false };
    let Some(LayoutKind::PackedList { element, rc: RcHeader::Required }) = layouts.get(physical).map(|descriptor| descriptor.kind()) else {
        return false;
    };
    matches!(layouts.get(*element).map(|descriptor| descriptor.kind()),
        Some(LayoutKind::RuntimePointerValueSlot { semantics: PointerSlot::Owned, .. }))
}

fn fields_match_storage(
    layout: crate::layout::LayoutId,
    source_type: &Type,
    fields: &[u32],
    layouts: &crate::layout::LayoutInterner,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> bool {
    use crate::layout::{FieldKind, LayoutKind, PointerSlot};
    let Some(shape) = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions()) else {
        return false;
    };
    let Ok(physical) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(physical) else { return false };
    if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. }) || descriptor.fields().len() != shape.fields.len() {
        return false;
    }
    let expected = if shape.tagged { PointerSlot::OwnedNullable } else { PointerSlot::Owned };
    fields.iter().all(|index| {
        let Some(field) = descriptor.fields().get(*index as usize) else { return false };
        match field.kind() {
            FieldKind::Pointer { slot, .. } => slot == expected,
            FieldKind::Inline(layout) => matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
                Some(LayoutKind::RuntimePointerValueSlot { semantics, .. }) if *semantics == expected),
            _ => false,
        }
    })
}
