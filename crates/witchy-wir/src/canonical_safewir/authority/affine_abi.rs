use super::*;
use crate::canonical_safewir::check::{AffineAbiProofs, AffinePhysicalPlan, AffineSignatureProof};
use witchy_canonical_source::storage::{AffineStorageShape, ManagedAggregateFieldTransfer,
    affine_storage_shape};
use crate::canonical_safewir::closure_owner_contract::{CaptureStorage, CaptureTransfer, OwnerFamilySource};
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
                if matches!(signature.parameters.get(index), Some(crate::canonical_safewir::ValueType::Gc(_))) {
                    continue;
                }
                return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal });
            };
            proof.parameters[index] = Some(bind_plan(candidate, ty, *layout, &shape, &layouts, source, &mut proofs.layouts)
                .ok_or(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal })?);
        }
        if let Some(shape) = affine_storage_shape(result, source.type_definitions()) {
            match signature.results.first() {
                Some(crate::canonical_safewir::ValueType::Ref(layout)) => {
                    proof.results[0] = Some(bind_plan(candidate, result, *layout, &shape, &layouts, source, &mut proofs.layouts)
                        .ok_or(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal })?);
                }
                Some(crate::canonical_safewir::ValueType::Gc(_)) => {}
                _ => return Err(CandidateAbiBindingError::SourceAbiMismatch { source_declaration: ordinal }),
            }
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
    // Generated continuations are callable implementations too. Their first
    // physical parameter is the closure environment; the remaining ABI is
    // authenticated by the exact checked lambda witness in candidate_abi.
    // Bind recursive affine plans to those lanes before the memory checker
    // sees a must-bearing Step/aggregate crossing a continuation call.
    for binding in candidate.closure_bindings().entries() {
        let refuse = || CandidateAbiBindingError::ClosureSignatureMismatch { closure: binding.closure() };
        let witness = source.closure_witness().entries().get(binding.closure() as usize)
            .ok_or_else(refuse)?;
        let function = candidate.functions().iter().find(|function| function.id == binding.function())
            .ok_or(CandidateAbiBindingError::MissingCandidateFunction { function: binding.function() })?;
        let signature = candidate.signatures().iter().find(|signature| signature.id == function.signature)
            .ok_or(CandidateAbiBindingError::MissingCandidateSignature { function: binding.function() })?;
        let Type::Fn(parameters, result, conventions, _) = witness.declared_signature() else {
            return Err(refuse());
        };
        if signature.parameters.len() != parameters.len() + 1 {
            return Err(refuse());
        }
        let mut proof = AffineSignatureProof {
            parameters: vec![None; signature.parameters.len()],
            results: vec![None; signature.results.len()],
        };
        for (source_index, ty) in parameters.iter().enumerate() {
            let physical_index = source_index + 1;
            let Some(shape) = affine_storage_shape(ty, source.type_definitions()) else {
                if source.storage_plan(ty.unqualified()).requires_affine_owner_token() {
                    return Err(refuse());
                }
                continue;
            };
            let Some(crate::canonical_safewir::ValueType::Ref(layout)) = signature.parameters.get(physical_index) else {
                if matches!(signature.parameters.get(physical_index), Some(crate::canonical_safewir::ValueType::Gc(_))) {
                    continue;
                }
                return Err(refuse());
            };
            proof.parameters[physical_index] = Some(bind_plan(candidate, ty, *layout, &shape, &layouts, source, &mut proofs.layouts)
                .ok_or_else(refuse)?);
        }
        if let Some(shape) = affine_storage_shape(result, source.type_definitions()) {
            match signature.results.first() {
                Some(crate::canonical_safewir::ValueType::Ref(layout)) => {
                    proof.results[0] = Some(bind_plan(candidate, result, *layout, &shape, &layouts, source, &mut proofs.layouts)
                        .ok_or_else(refuse)?);
                }
                Some(crate::canonical_safewir::ValueType::Gc(_)) => {}
                _ => return Err(refuse()),
            }
        } else if source.storage_plan(result.unqualified()).requires_affine_owner_token() {
            return Err(refuse());
        }
        let writebacks = parameters.iter().enumerate().filter_map(|(index, _)|
            (conventions.get(index).copied().unwrap_or(Convention::Let) == Convention::Var)
                .then_some(index + 1)).collect::<Vec<_>>();
        let hidden_start = signature.results.len().checked_sub(writebacks.len()).ok_or_else(refuse)?;
        for (physical_index, parameter_proof) in proof.parameters.iter().enumerate() {
            let Some(parameter_proof) = parameter_proof else { continue };
            let Some(crate::canonical_safewir::ParameterContract {
                value_type: crate::canonical_safewir::ValueType::Ref(parameter_layout),
                ownership: crate::canonical_safewir::ParameterOwnership::UniqueWriteBack,
            }) = signature.contract.parameters.get(physical_index) else { continue };
            let offset = writebacks.iter().position(|index| *index == physical_index).ok_or_else(refuse)?;
            let result_index = hidden_start + offset;
            if !matches!(signature.contract.results.get(result_index),
                Some(crate::canonical_safewir::ResultContract::Unique { layout }) if layout == parameter_layout)
            {
                return Err(refuse());
            }
            proof.results[result_index] = Some(parameter_proof.clone());
        }
        if proof.parameters.iter().chain(&proof.results).any(Option::is_some)
            && proofs.signatures.insert(signature.id, proof.clone()).is_some_and(|prior| prior != proof)
        {
            return Err(refuse());
        }
    }
    bind_owner_environments(candidate, source, &layouts, &mut proofs)?;
    bind_local_allocations(candidate, source, &layouts, &mut proofs)?;
    Ok(proofs)
}

/// A local allocation has no ABI lane. Bind its recursive move plan only
/// from a concrete checked expression type and exact physical correspondence.
/// The memory checker carries that plan through extraction, calls, returns,
/// and control-flow joins; a candidate layout alone never grants it.
fn bind_local_allocations<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    layouts: &crate::layout::LayoutInterner,
    proofs: &mut AffineAbiProofs,
) -> Result<(), CandidateAbiBindingError> {
    const MAX_LOCAL_PLAN_PAIRS: usize = 65_536;
    let records = source.type_definitions();
    let allocated_layouts = candidate.functions().iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter_map(|operation| match operation {
            crate::canonical_safewir::Operation::Alloc { layout, .. } => Some(*layout),
            _ => None,
        }).collect::<BTreeSet<_>>();
    let affine_types = source.concrete_types().iter().filter_map(|ty|
        affine_storage_shape(ty, records).map(|shape| (ty, shape))).collect::<Vec<_>>();
    let Some(pairs) = affine_types.len().checked_mul(allocated_layouts.len()) else {
        return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
    };
    if pairs > MAX_LOCAL_PLAN_PAIRS {
        return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
    }
    for (source_type, shape) in affine_types {
        for layout in allocated_layouts.iter().copied() {
            if matches!(shape, AffineStorageShape::Sequence(_) | AffineStorageShape::Mapping { .. })
                && !sequence_nominal_matches_source(source_type, layout, layouts, source)
            {
                continue;
            }
            if !layout_matches_source_with_closures(
                layout, source_type, layouts, records, candidate.closure_layouts(),
                candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate),
            ) { continue; }
            // A failed recursive bind must not leave partial child authority.
            let mut catalog = proofs.layouts.clone();
            if bind_plan(candidate, source_type, layout, &shape, layouts, source, &mut catalog).is_none() {
                return Err(CandidateAbiBindingError::MissingCanonicalLayouts);
            }
            proofs.layouts = catalog;
        }
    }
    Ok(())
}

fn sequence_nominal_matches_source(
    source_type: &Type,
    layout: crate::layout::LayoutId,
    layouts: &crate::layout::LayoutInterner,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
) -> bool {
    let Some(identity) = witchy_canonical_source::storage::aggregate_layout_identity(
        source_type, source.type_definitions(),
    ) else { return false };
    let Some(expected) = witchy_canonical_source::structural_identity::recursive_nominal_layout_key(&identity)
    else { return false };
    matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
        Some(crate::layout::LayoutKind::OpaqueNominal { key }) if *key == expected)
}

fn bind_owner_environments<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    layouts: &crate::layout::LayoutInterner,
    proofs: &mut AffineAbiProofs,
) -> Result<(), CandidateAbiBindingError> {
    use crate::layout::{LayoutKind, PointerSlot};
    let refuse = || CandidateAbiBindingError::MissingCanonicalLayouts;
    for family in candidate.owner_families() {
        let OwnerFamilySource::Aggregate(source_type) = &family.source else { continue };
        let move_bearing = family.constructors.iter().flat_map(|constructor| &constructor.captures)
            .any(|capture| capture.transfer == CaptureTransfer::MoveManaged);
        let affine = source.storage_plan(source_type).requires_affine_owner_token();
        if !affine {
            if move_bearing { return Err(refuse()); }
            continue;
        }
        let shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions())
            .filter(|shape| !shape.packed).ok_or_else(refuse)?;
        let physical = layouts.resolve_nominal(family.environment).map_err(|_| refuse())?;
        let Some(LayoutKind::PackedRecord { fields }) = layouts.get(physical).map(|descriptor| descriptor.kind())
        else { return Err(refuse()) };
        if family.constructors.len() != shape.variants.len() { return Err(refuse()); }
        for (constructor, variant) in family.constructors.iter().zip(shape.variants.iter()) {
            if constructor.captures.len() != variant.arity { return Err(refuse()); }
            if proofs.owner_captures.insert(constructor.id, constructor.captures.iter()
                .map(|capture| (capture.transfer, capture.value_type)).collect()).is_some() { return Err(refuse()); }
            let mut children = Vec::new();
            for (offset, capture) in constructor.captures.iter().enumerate() {
                let source_field = shape.fields.get(variant.first_field + offset).ok_or_else(refuse)?;
                if capture.transfer != CaptureTransfer::MoveManaged { continue; }
                if source.aggregate_managed_field_transfer(source_field)
                    != Some(ManagedAggregateFieldTransfer::Move)
                    || affine_storage_shape(source_field, source.type_definitions()).is_none()
                { return Err(refuse()); }
                if let (CaptureStorage::EnvironmentField(index), crate::canonical_safewir::ValueType::Ref(layout)) =
                    (capture.storage, capture.value_type) {
                    let Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::OwnedNullable }) =
                        fields.get(index as usize).and_then(|field| layouts.get(*field)).map(|slot| slot.kind())
                    else { return Err(refuse()) };
                    if *pointee != layout { return Err(refuse()); }
                    let affine = affine_storage_shape(source_field, source.type_definitions()).ok_or_else(refuse)?;
                    let child = bind_plan(candidate, source_field, layout, &affine, layouts, source, &mut proofs.layouts)
                        .ok_or_else(refuse)?;
                    children.push((index, layout, Box::new(child)));
                } else if !matches!((capture.storage, capture.value_type),
                    (CaptureStorage::OwnerField(_), crate::canonical_safewir::ValueType::Gc(_))) {
                    return Err(refuse());
                }
            }
            if proofs.owner_environments.insert(constructor.id, AffinePhysicalPlan::NullableFields {
                fields: children, allow_absent: false,
            }).is_some() {
                return Err(refuse());
            }
        }
    }
    Ok(())
}

fn bind_plan<S: ModuleStage>(
    candidate: &CanonicalModule<S>,
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
            let mut storage = source_type;
            for _ in 0..128 {
                let Type::Qualified(witchy_syntax::ast::TypeQual::Frozen, inner) = storage else { break };
                storage = inner;
            }
            if let Type::Qualified(witchy_syntax::ast::TypeQual::Unique, inner) = storage {
                // A unique root may hold callables (`unique List(fn(...))`):
                // match it with the same closure correspondence as its caller.
                if !layout_matches_source_with_closures(
                    layout, inner, layouts, source.type_definitions(), candidate.closure_layouts(),
                    candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate),
                ) { return None; }
            } else {
                let source_shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions())?;
                if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. })
                    || descriptor.fields().len() != source_shape.fields.len() { return None; }
            }
            AffinePhysicalPlan::Leaf
        }
        AffineStorageShape::NullableFields(children) => {
            let source_shape = witchy_canonical_source::aggregates::scalar_aggregate_shape(source_type, source.type_definitions())?;
            if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. })
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
                bound.push((*index, pointee, Box::new(bind_plan(candidate, child_source, pointee, child, layouts, source, catalog)?)));
            }
            AffinePhysicalPlan::NullableFields { fields: bound, allow_absent: source_shape.tagged }
        }
        AffineStorageShape::Sequence(child) => {
            // The packed-list body is shared with cloneable lists. Only the
            // source-derived nominal root may receive a move-only plan.
            if !sequence_nominal_matches_source(source_type, layout, layouts, source) {
                return None;
            }
            let witchy_canonical_source::storage::StoragePlan::MoveOnlyList(sequence) = source.storage_plan(source_type.unqualified()) else { return None };
            let Type::Named(_, arguments) = sequence.source() else { return None };
            let [child_source] = arguments.as_slice() else { return None };
            let LayoutKind::PackedList { element, rc: RcHeader::Required } = descriptor.kind() else { return None };
            let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } = layouts.get(*element)?.kind() else { return None };
            AffinePhysicalPlan::Sequence {
                element: *pointee,
                child: Box::new(bind_plan(candidate, child_source, *pointee, child, layouts, source, catalog)?),
            }
        }
        AffineStorageShape::Mapping { key, value } => {
            if !sequence_nominal_matches_source(source_type, layout, layouts, source) {
                return None;
            }
            let witchy_canonical_source::storage::StoragePlan::MoveOnlyDict(mapping) =
                source.storage_plan(source_type.unqualified()) else { return None };
            let Type::Named(_, children) = mapping.source() else { return None };
            let [key_source, value_source] = children.as_slice() else { return None };
            let LayoutKind::CanonicalDictionary { key: key_slot, value: value_slot, .. } = descriptor.kind()
                else { return None };
            let bind_child = |shape: &Option<Box<AffineStorageShape>>, child_source: &Type,
                              slot: crate::layout::LayoutId,
                              catalog: &mut BTreeMap<crate::layout::LayoutId, AffinePhysicalPlan>| {
                let shape = shape.as_ref()?;
                let LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } =
                    layouts.get(slot)?.kind() else { return None };
                Some((*pointee, Box::new(bind_plan(candidate, child_source, *pointee, shape, layouts, source, catalog)?)))
            };
            AffinePhysicalPlan::Mapping {
                key: if key.is_some() { Some(bind_child(key, key_source, *key_slot, catalog)?) } else { None },
                value: if value.is_some() { Some(bind_child(value, value_source, *value_slot, catalog)?) } else { None },
            }
        }
    };
    if catalog.insert(layout, plan.clone()).is_some_and(|prior| prior != plan) { return None; }
    Some(plan)
}
