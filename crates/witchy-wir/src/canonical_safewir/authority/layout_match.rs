use super::*;
use witchy_canonical_source::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage, RetainedRootStorage, RootedViewStorage};
use witchy_canonical_source::storage::StoragePlan;

pub(crate) fn value_type_matches_source(
    actual: super::super::ValueType,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
) -> bool {
    if is_unit_source_type(source) {
        return actual == super::super::ValueType::Unit;
    }
    // Classify the source view once, then independently match its physical
    // representation. Final binding separately requires the nominally bound
    // plan from CanonicalSourceView for checked record-root signatures.
    if let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, payload) = source {
        let record_head = witchy_canonical_source::rooted_record::has_record_root_head(payload, records);
        if record_head.is_none() { return false; }
        let storage = match StoragePlan::classify(source, records, |_| Some(0)) {
            StoragePlan::RootedView(plan) => Some(plan),
            _ => None,
        };
        if record_head == Some(true) {
            let Some(RootedViewStorage::RetainedRoot(RetainedRootStorage::CheckedAggregate(plan))) = storage else { return false };
            let super::super::ValueType::Ref(layout) = actual else { return false };
            if !nominal_layout_matches_source_identity(layout, payload, layouts, records) { return false; }
            return record_layout_matches_with_closures(
                layout, &plan.shape.fields, plan.shape.tagged, layouts, records,
                closure_layouts, signatures, context,
            );
        }
        match storage {
            Some(RootedViewStorage::RetainedRoot(RetainedRootStorage::StructuralRecord(plan))) => {
                let super::super::ValueType::Ref(layout) = actual else { return false };
                if matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
                    Some(crate::layout::LayoutKind::OpaqueNominal { .. })) { return false; }
                return super::super::closure_owner_contract::retained_structural_root_layout_matches(
                    layouts, layout, &plan,
                );
            }
            Some(RootedViewStorage::RetainedRoot(RetainedRootStorage::Sequence(carrier))) => {
                // Every nested element still needs an owned slot and RC list.
                let super::super::ValueType::Ref(layout) = actual else { return false };
                if matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
                    Some(crate::layout::LayoutKind::OpaqueNominal { .. })) { return false; }
                let Ok(physical) = layouts.resolve_nominal(layout) else { return false };
                let Some(crate::layout::LayoutKind::PackedList { element, rc: crate::layout::RcHeader::Required }) =
                    layouts.get(physical).map(|descriptor| descriptor.kind()) else { return false };
                return super::super::closure_owner_contract::carrier_element_layout_matches(layouts, *element, &carrier);
            }
            _ => {}
        }
    }
    // A rooted scalar or slice view aggregate is an opaque owner family, including when it
    // enters as a parameter. Its ordinary record fields may happen to look
    // like the erased referents, but that linear layout cannot authenticate
    // the retained roots. The Gc arm below binds the exact source identity.
    if is_direct_rooted_view_aggregate_carrier(source, records)
        && !matches!(actual, super::super::ValueType::Gc(_)) {
        return false;
    }
    if let super::super::ValueType::Gc(id) = actual {
        let Some(family) = context.owner_families.iter().find(|f| f.owner_type == id || f.list_type == id).cloned() else { return false };
        if let super::super::closure_owner_contract::OwnerFamilySource::SharedReference(expected) = &family.source {
            let expected_id = match StoragePlan::classify(expected, records, |_| Some(0)) {
                StoragePlan::RootedView(plan) => plan.owner_family_id(),
                _ => None,
            };
            if !expected_id.is_some_and(|expected_id| family.owner_type.get() == expected_id
                && family.list_type.get() == expected_id + 1) {
                return false;
            }
            // `List(&Int)` is the `&Int` family's owner list.
            if id == family.list_type {
                return is_direct_rooted_scalar_place(expected)
                    && is_list_of_rooted_scalar_places(source);
            }
            return id == family.owner_type
                && ((is_direct_rooted_scalar_place(expected) && is_direct_rooted_scalar_place(source))
                    || (is_direct_rooted_projection(expected, records)
                        && family.slice_carrier.as_ref().is_some_and(|carrier| {
                            OwnerCarriedViewPlan::classify(source, records).is_some_and(|plan| {
                                matches!(plan.admitted_storage(), Some(OwnerCarriedViewStorage::RetainedProjection(element)) if element == carrier)
                            })
                        })));
        }
        if let super::super::closure_owner_contract::OwnerFamilySource::Aggregate(expected) = &family.source {
            let aggregate = if id == family.list_type {
                if !matches!(StoragePlan::classify(source.unqualified(), records, |_| Some(0)),
                    StoragePlan::RetainedOpaqueAggregateList(_)
                        | StoragePlan::RetainedGraphList(_)) {
                    return false;
                }
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
            CapabilityRepresentation::Direct(CapabilityKind::Console) | CapabilityRepresentation::Nullable(CapabilityKind::Console) => super::super::CapabilityKindId::new(0),
            CapabilityRepresentation::Direct(CapabilityKind::Dir) | CapabilityRepresentation::Nullable(CapabilityKind::Dir) => super::super::DIR_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Env) | CapabilityRepresentation::Nullable(CapabilityKind::Env) => super::super::ENV_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::File) | CapabilityRepresentation::Nullable(CapabilityKind::File) => super::super::FILE_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Net) | CapabilityRepresentation::Nullable(CapabilityKind::Net) => super::super::NET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Listener) | CapabilityRepresentation::Nullable(CapabilityKind::Listener) => super::super::LISTENER_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Socket) | CapabilityRepresentation::Nullable(CapabilityKind::Socket) => super::super::SOCKET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Exec) | CapabilityRepresentation::Nullable(CapabilityKind::Exec) => super::super::EXEC_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Fetch) | CapabilityRepresentation::Nullable(CapabilityKind::Fetch) => super::super::FETCH_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Clock) | CapabilityRepresentation::Nullable(CapabilityKind::Clock) => super::super::CLOCK_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::SecretStore) | CapabilityRepresentation::Nullable(CapabilityKind::SecretStore) => super::super::SECRET_STORE_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::Secret) | CapabilityRepresentation::Nullable(CapabilityKind::Secret) => super::super::SECRET_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildOut) | CapabilityRepresentation::Nullable(CapabilityKind::BuildOut) => super::super::BUILD_OUT_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildRead) | CapabilityRepresentation::Nullable(CapabilityKind::BuildRead) => super::super::BUILD_READ_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildEnv) | CapabilityRepresentation::Nullable(CapabilityKind::BuildEnv) => super::super::BUILD_ENV_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildExec) | CapabilityRepresentation::Nullable(CapabilityKind::BuildExec) => super::super::BUILD_EXEC_CAPABILITY_KIND,
            CapabilityRepresentation::Direct(CapabilityKind::BuildNet) | CapabilityRepresentation::Nullable(CapabilityKind::BuildNet) => super::super::BUILD_NET_CAPABILITY_KIND,
        };
        return matches!(
            (representation, actual),
            (CapabilityRepresentation::Direct(_), super::super::ValueType::Capability(actual_kind))
                | (CapabilityRepresentation::Nullable(_), super::super::ValueType::NullableCapability(actual_kind))
                if actual_kind == kind
        );
    }
    match StoragePlan::classify(source.unqualified(), records, |_| Some(0)) {
        StoragePlan::InlineScalar(kind) | StoragePlan::Owned(witchy_canonical_source::carrier::CarrierElement::Scalar(kind)) => {
            use witchy_canonical_source::abi::CanonicalScalarKind;
            actual == match kind {
                CanonicalScalarKind::Unit => super::super::ValueType::Unit,
                CanonicalScalarKind::Int | CanonicalScalarKind::Duration => super::super::ValueType::Int,
                CanonicalScalarKind::Float => super::super::ValueType::Float,
                CanonicalScalarKind::Bool => super::super::ValueType::Bool,
            }
        }
        StoragePlan::Owned(
            witchy_canonical_source::carrier::CarrierElement::Tuple(_)
            | witchy_canonical_source::carrier::CarrierElement::Dict(_, _)
            | witchy_canonical_source::carrier::CarrierElement::Record { .. }
            | witchy_canonical_source::carrier::CarrierElement::Sum { .. }
        ) | StoragePlan::AggregateView(_) | StoragePlan::AuthenticatedPlace(_) => {
            witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::super::ValueType::Ref(layout)
                    if nominal_layout_matches_source_identity(layout, source, layouts, records)
                        && record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            })
        }
        StoragePlan::MoveOnlyList(_) | StoragePlan::RetainedGraphList(_) | StoragePlan::RetainedCallableList(_) | StoragePlan::Owned(
            witchy_canonical_source::carrier::CarrierElement::String
            | witchy_canonical_source::carrier::CarrierElement::Bytes
            | witchy_canonical_source::carrier::CarrierElement::List(_)
        ) => matches!(actual, super::super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
        StoragePlan::RetainedCallable(_) => match actual {
            super::super::ValueType::Ref(layout) => {
                let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
                let Some(&id) = closure_layouts.get(&layout) else { return false };
                let matched = signatures
                    .iter()
                    .find(|signature| signature.id == id)
                    .and_then(|signature| strip_closure_environment_with(signature, closure_layouts))
                    .is_some_and(|declaration| {
                        candidate_signature_matches_source_inner(&declaration, source, parameters, result, conventions, layouts, signatures, context, records, closure_layouts)
                    });
                matched
            }
            _ => false,
        },
        StoragePlan::InlineAggregate(_) | StoragePlan::RootedView(_)
            | StoragePlan::RetainedOwnerList(_) | StoragePlan::RetainedOpaqueAggregateList(_)
            | StoragePlan::Refused(_) => false,
    }
}

pub(super) fn record_layout_matches_with_closures(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    let mut pending = Vec::new();
    if !enqueue_record_fields_with_closures(layout, fields, nullable_fields, layouts, &mut pending, closure_layouts) {
        return false;
    }
    match_layout_graph(pending, layouts, records, closure_layouts, signatures, context)
}

pub(super) fn enqueue_record_fields(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
) -> bool {
    enqueue_record_fields_with_closures(layout, fields, nullable_fields, layouts, pending, &BTreeMap::new())
}

pub(super) fn enqueue_record_fields_with_closures(
    layout: crate::layout::LayoutId,
    fields: &[witchy_syntax::ast::Type],
    nullable_fields: bool,
    layouts: &crate::layout::LayoutInterner,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    _closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
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

pub(super) fn layout_matches_source(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    layout_matches_source_with_closures(layout, source, layouts, records, &BTreeMap::new(), &[], &mut CorrespondenceContext::default())
}

pub(super) fn layout_matches_source_with_closures(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    match_layout_graph(vec![(layout, source.unqualified().clone())], layouts, records, closure_layouts, signatures, context)
}

fn nominal_layout_matches_source_identity(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
) -> bool {
    let Some(crate::layout::LayoutKind::OpaqueNominal { key }) =
        layouts.get(layout).map(|descriptor| descriptor.kind())
    else { return true };
    let Some(identity) = witchy_canonical_source::aggregates::canonical_owner_aggregate_identity(
        source, records,
    ) else { return false };
    witchy_canonical_source::structural_identity::recursive_nominal_layout_key(&identity)
        .is_some_and(|expected| *key == expected)
}

/// Check every node in the finite layout/source-type product. A revisited
/// exact pair closes a recursive edge only: all other outgoing obligations
/// still have to pass. Nominal resolution never replaces source validation.
pub(super) fn match_layout_graph(
    mut pending: Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    while let Some((layout, source)) = pending.pop() {
        if !nominal_layout_matches_source_identity(layout, &source, layouts, records) { return false; }
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

pub(super) fn enqueue_layout_children(
    layout: crate::layout::LayoutId,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
    records: &ScalarRecordFields,
    pending: &mut Vec<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
) -> bool {
    use crate::layout::{LayoutKind, PointerSlot, RcHeader, ScalarKind};
    let Ok(layout) = layouts.resolve_nominal(layout) else { return false };
    let Some(descriptor) = layouts.get(layout) else { return false };
    let storage = StoragePlan::classify(source.unqualified(), records, |_| Some(0));
    // An aggregate (record, sum, tuple, scalar Option/Result) is a fixed
    // record of its shape's slots.
    if let Some(shape) = witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records) {
        if !matches!(storage,
            StoragePlan::Owned(witchy_canonical_source::carrier::CarrierElement::Tuple(_)
                | witchy_canonical_source::carrier::CarrierElement::Dict(_, _)
                | witchy_canonical_source::carrier::CarrierElement::Record { .. }
                | witchy_canonical_source::carrier::CarrierElement::Sum { .. })
                | StoragePlan::AggregateView(_) | StoragePlan::AuthenticatedPlace(_)) {
            return false;
        }
        return enqueue_record_fields(layout, &shape.fields, shape.tagged, layouts, pending);
    }
    let list_disposition = if matches!(storage, StoragePlan::MoveOnlyList(_)) {
        witchy_canonical_source::storage::ListElementDisposition::MoveOnly
    } else {
        witchy_canonical_source::storage::ListElementDisposition::Cloneable
    };
    match storage {
        StoragePlan::Owned(witchy_canonical_source::carrier::CarrierElement::String
            | witchy_canonical_source::carrier::CarrierElement::Bytes) => {
            let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else { return false };
            layouts.get(*element).is_some_and(|element| matches!(element.kind(), LayoutKind::Scalar(ScalarKind::RawByte)))
        }
        StoragePlan::Owned(witchy_canonical_source::carrier::CarrierElement::List(_))
            | StoragePlan::RetainedGraphList(_) | StoragePlan::RetainedCallableList(_)
            | StoragePlan::MoveOnlyList(_) => {
            let witchy_syntax::ast::Type::Named(_, arguments) = source.unqualified() else { return false };
            let [source_element] = arguments.as_slice() else { return false };
            // Source List values are retained objects. An elided-header list
            // cannot satisfy their layout, including a DictRoot child list.
            let LayoutKind::PackedList { element, rc: RcHeader::Required } = descriptor.kind() else { return false };
            let element_layout = *element;
            let Some(element) = layouts.get(element_layout) else { return false };
            // The source plan owns the recursive carrier judgment, including
            // nested views and qualifiers. No parallel syntax scan can grant
            // a plain packed-list representation here.
            let element_storage = StoragePlan::classify(source_element, records, |_| Some(0));
            if element_storage.list_element_disposition(source_element) != Some(list_disposition)
            { return false; }
            match element_storage {
                StoragePlan::InlineScalar(kind) => {
                    return super::super::closure_owner_contract::carrier_element_layout_matches(
                        layouts, element_layout,
                        &witchy_canonical_source::carrier::CarrierElement::Scalar(kind),
                    );
                }
                StoragePlan::Owned(carrier) => {
                    // The same source plan used by layout projection supplies
                    // the recursive ownership proof. A physical mismatch
                    // cannot fall through to the older layout walk.
                    return super::super::closure_owner_contract::carrier_element_layout_matches(
                        layouts, element_layout, &carrier,
                    );
                }
                StoragePlan::RetainedCallable(plan) => {
                    return matches!(element.kind(), LayoutKind::RuntimePointerValueSlot {
                        pointee, semantics: PointerSlot::Owned,
                    } if { pending.push((*pointee, plan.exact_signature().clone())); true });
                }
                StoragePlan::AuthenticatedPlace(plan) => {
                    return matches!(element.kind(), LayoutKind::RuntimePointerValueSlot {
                        pointee, semantics: PointerSlot::Owned,
                    } if { pending.push((*pointee, plan.source().clone())); true });
                }
                StoragePlan::MoveOnlyList(_) => {
                    return matches!(element.kind(), LayoutKind::RuntimePointerValueSlot {
                        pointee, semantics: PointerSlot::Owned,
                    } if { pending.push((*pointee, source_element.clone())); true });
                }
                _ => return false,
            }
        }
        StoragePlan::RetainedCallable(_) => {
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
