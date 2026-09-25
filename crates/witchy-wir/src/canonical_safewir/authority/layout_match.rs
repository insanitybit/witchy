use super::*;
use witchy_canonical_source::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage, RetainedRootStorage, RootedViewStorage};
use witchy_canonical_source::storage::{ListElementProjection, StoragePlan};

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
            return record_layout_matches_with_closures(
                layout, &plan.shape.fields, plan.shape.tagged, layouts, records,
                closure_layouts, signatures, context,
            );
        }
        match storage {
            Some(RootedViewStorage::RetainedRoot(RetainedRootStorage::StructuralRecord(plan))) => {
                let super::super::ValueType::Ref(layout) = actual else { return false };
                return super::super::closure_owner_contract::retained_structural_root_layout_matches(
                    layouts, layout, &plan,
                );
            }
            Some(RootedViewStorage::RetainedRoot(RetainedRootStorage::Sequence(carrier))) => {
                // Every nested element still needs an owned slot and RC list.
                let super::super::ValueType::Ref(layout) = actual else { return false };
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
    match source.unqualified() {
        // `Dir` carries its rights as type arguments (`Dir[Read]`), and the
        // rights live in the host's authority object behind the token, so
        // every spelling is the one fixed ABI kind. This arm precedes the
        // argument-free one because `Dir` may appear with no arguments too.
        witchy_syntax::ast::Type::Named(name, _) if name == "Dir" => {
            actual == super::super::ValueType::Capability(super::super::DIR_CAPABILITY_KIND)
        }
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => match name.as_str() {
            "Int" | "Duration" => actual == super::super::ValueType::Int,
            "Float" => actual == super::super::ValueType::Float,
            "Bool" => actual == super::super::ValueType::Bool,
            "Nil" | "Unit" => actual == super::super::ValueType::Unit,
            // Canonical lowering assigns the authenticated prelude Console
            // capability the fixed ABI kind 0; no candidate-selected kind is
            // accepted at this source boundary.
            "Console" => actual == super::super::ValueType::Capability(super::super::CapabilityKindId::new(0)),
            // The process environment grant, the one `Env` token.
            "Env" => actual == super::super::ValueType::Capability(super::super::ENV_CAPABILITY_KIND),
            // `Bytes` is the same raw-byte buffer under its own source type.
            "String" | "Bytes" => matches!(actual, super::super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
            // A scalar aggregate: one reference to a fixed layout whose slots
            // are exactly the shape's fields, in order.
            _ => witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            }),
        },
        // A dictionary is its canonical tuple of the key list and the value
        // list, so it matches by that shape like any other aggregate.
        witchy_syntax::ast::Type::Named(name, arguments)
            if matches!(name.as_str(), "Option" | "Result") || (name == "Dict" && arguments.len() == 2) =>
        {
            witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            })
        }
        witchy_syntax::ast::Type::Tuple(_) => {
            witchy_canonical_source::aggregates::scalar_aggregate_shape(source, records).is_some_and(|shape| {
                matches!(actual, super::super::ValueType::Ref(layout) if record_layout_matches_with_closures(layout, &shape.fields, shape.tagged, layouts, records, closure_layouts, signatures, context))
            })
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => matches!(actual, super::super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
        // Concrete user generics use the same independently matched field
        // graph as other aggregates. A type name or its arguments alone never
        // grant a candidate reference its layout authority.
        witchy_syntax::ast::Type::Named(_, _) => matches!(actual, super::super::ValueType::Ref(layout) if layout_matches_source_with_closures(layout, source, layouts, records, closure_layouts, signatures, context)),
        // A `fn` value is a closure record: a reference to a layout the
        // module registered with a closure signature, which, past its
        // environment parameter, is the source signature.
        witchy_syntax::ast::Type::Fn(parameters, result, _, _) => match actual {
            super::super::ValueType::Ref(layout) => {
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
pub(super) fn list_element_needs_owner_carrier(
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
            // Source List values are retained objects. An elided-header list
            // cannot satisfy their layout, including a DictRoot child list.
            let LayoutKind::PackedList { element, rc: RcHeader::Required } = descriptor.kind() else { return false };
            let element_layout = *element;
            let Some(element) = layouts.get(element_layout) else { return false };
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
            match ListElementProjection::classify(&arguments[0], records) {
                ListElementProjection::Owned(carrier) => {
                    // The same source plan used by layout projection supplies
                    // the recursive ownership proof. A physical mismatch
                    // cannot fall through to the older layout walk.
                    return super::super::closure_owner_contract::carrier_element_layout_matches(
                        layouts, element_layout, &carrier,
                    );
                }
                ListElementProjection::RetainedByteBuffer(_) => {
                    return super::super::closure_owner_contract::carrier_element_layout_matches(
                        layouts, element_layout,
                        &witchy_canonical_source::carrier::CarrierElement::Bytes,
                    );
                }
                ListElementProjection::Refused => return false,
                ListElementProjection::RetainedCallable(_) | ListElementProjection::LayoutOnly => {}
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
