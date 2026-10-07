use super::*;

/// A family selects environment variants, not callable authority. Resolve
/// checked nominal aliases and spell out default conventions before grouping;
/// full signature, capture, transfer, and tag checks remain independent above.
pub(crate) fn source_closure_family_key(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> Option<witchy_syntax::ast::Type> {
    witchy_canonical_source::callable_storage::bounded_closure_family_type(source, records)
}

pub(super) const MAX_SOURCE_CORRESPONDENCE_PAIRS: usize = 65_536;

/// The closed checked signature loses nested legacy borrow spelling. Inspect
/// the original declaration, including nominal fields, before comparing a
/// candidate to that signature. Expansion is bounded independently of the
/// ordinary storage proof.
pub(super) fn original_type_has_unchecked_loan(
    original: &witchy_syntax::ast::Type,
    definitions: &ScalarRecordFields,
) -> bool {
    use witchy_syntax::ast::{Type, TypeQual};
    let mut pending = vec![(original, true, 0usize)];
    let mut expanded = HashSet::new();
    let mut visited = 0usize;
    while let Some((ty, root, depth)) = pending.pop() {
        visited += 1;
        if visited > 4_096 || depth > 128 { return true; }
        match ty {
            Type::Qualified(
                TypeQual::Borrow(_) | TypeQual::BorrowMut(_) | TypeQual::LegacyBorrow(_),
                inner,
            ) => {
                if !root || !(is_direct_shared_reference(ty) || is_direct_exclusive_reference(ty)) {
                    return true;
                }
                pending.push((inner, false, depth + 1));
            }
            Type::Qualified(_, inner) | Type::Slice(inner) => {
                pending.push((inner, false, depth + 1));
            }
            Type::Named(name, arguments) => {
                pending.extend(arguments.iter().map(|argument| (argument, false, depth + 1)));
                if let Some(definition) = definitions.get(name)
                    && expanded.insert(name.as_str())
                {
                    pending.extend(definition.variants.iter().flat_map(|variant|
                        variant.fields.iter().map(|field| (field, false, depth + 1))));
                }
            }
            Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                pending.extend(arguments.iter().map(|argument| (argument, false, depth + 1)));
            }
            Type::Fn(parameters, result, _, _) => {
                pending.extend(parameters.iter().map(|parameter| (parameter, false, depth + 1)));
                pending.push((result, false, depth + 1));
            }
            Type::RecordCompose { base, fields } => {
                pending.push((base, false, depth + 1));
                pending.extend(fields.iter().map(|(_, field)| (field, false, depth + 1)));
            }
        }
    }
    false
}

/// A source signature can grant a record-root ABI only when the checked view
/// can derive its typed, nominally bound plan. This walk reads source facts;
/// candidate syntax never supplies the plan or the nominal table.
pub(super) fn record_root_plans_in_signature(
    view: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    signature: &witchy_syntax::ast::Type,
) -> bool {
    use witchy_syntax::ast::{Type, TypeQual};
    let mut pending = vec![signature];
    let mut visited = 0usize;
    while let Some(ty) = pending.pop() {
        visited += 1;
        if visited > 4_096 { return false; }
        match ty {
            Type::Qualified(TypeQual::View, payload) => {
                match witchy_canonical_source::rooted_record::has_record_root_head(
                    payload, view.type_definitions(),
                ) {
                    Some(true) if !matches!(
                        view.storage_plan(ty),
                        witchy_canonical_source::storage::StoragePlan::RootedView(
                            witchy_canonical_source::rooted_view::RootedViewStorage::RetainedRoot(
                            witchy_canonical_source::rooted_view::RetainedRootStorage::CheckedAggregate(_)
                            | witchy_canonical_source::rooted_view::RetainedRootStorage::CheckedGraph(_)))
                    ) => return false,
                    None => return false,
                    _ => {}
                }
                pending.push(payload);
            }
            Type::Qualified(_, inner) | Type::Slice(inner) => pending.push(inner),
            Type::Named(_, arguments) | Type::Dyn(_, arguments) | Type::Tuple(arguments) => {
                pending.extend(arguments);
            }
            Type::Fn(parameters, result, _, _) => {
                pending.extend(parameters);
                pending.push(result);
            }
            Type::RecordCompose { base, fields } => {
                pending.push(base);
                pending.extend(fields.iter().map(|(_, field)| field));
            }
        }
    }
    true
}

#[derive(Default)]
pub(crate) struct CorrespondenceContext<'a> {
    pub(super) owner_families: &'a [super::super::closure_owner_contract::OwnerFamily],
    active_callable: HashSet<(super::super::FunctionSignatureId, witchy_syntax::ast::Type)>,
    completed_callable: HashSet<(super::super::FunctionSignatureId, witchy_syntax::ast::Type)>,
    active_layout: HashSet<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    completed_layout: HashSet<(crate::layout::LayoutId, witchy_syntax::ast::Type)>,
    pairs: usize,
}

pub(super) enum CorrespondenceVisit {
    New,
    Active,
    Completed,
    Limit,
}

impl<'a> CorrespondenceContext<'a> {
    pub(crate) fn with_owners<S: ModuleStage>(candidate: &'a CanonicalModule<S>) -> Self {
        Self { owner_families: candidate.owner_families(), ..Self::default() }
    }
    pub(super) fn enter_callable(
        &mut self,
        key: (super::super::FunctionSignatureId, witchy_syntax::ast::Type),
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

    pub(super) fn finish_callable(
        &mut self,
        key: (super::super::FunctionSignatureId, witchy_syntax::ast::Type),
        matched: bool,
    ) {
        self.active_callable.remove(&key);
        if matched {
            self.completed_callable.insert(key);
        }
    }

    pub(super) fn enter_layout(
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

    pub(super) fn finish_layout(
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

pub(super) fn candidate_signature_matches_source<S: ModuleStage>(
    actual: &super::super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
) -> bool {
    candidate_signature_matches_source_with_records(actual, source, candidate, &BTreeMap::new())
}

pub(crate) fn candidate_signature_matches_source_with_records<S: ModuleStage>(
    actual: &super::super::FunctionSignature,
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
pub(crate) fn candidate_signature_matches_source_with_layouts<S: ModuleStage>(
    actual: &super::super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    candidate: &CanonicalModule<S>,
    records: &ScalarRecordFields,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    candidate_signature_matches_source_inner(actual, source, parameters, result, conventions, layouts, candidate.signatures(), &mut CorrespondenceContext::with_owners(candidate), records, candidate.closure_layouts())
}

/// An owner list whose elements are authenticated rooted scalar places.
pub(super) fn is_list_of_rooted_scalar_places(ty: &witchy_syntax::ast::Type) -> bool {
    matches!(
        witchy_canonical_source::storage::StoragePlan::classify(ty.unqualified(), &BTreeMap::new(), |_| None),
        witchy_canonical_source::storage::StoragePlan::RetainedOwnerList(_)
    )
}

/// Check the original source graph before any ABI comparison erases wrappers.
/// A root Unique on an own/var input or result is represented by the callable
/// ownership contract. Nested affine aggregates use the shared recursive
/// storage proof; other nested qualifiers remain unsupported.
pub(super) fn source_abi_qualifiers_supported(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_syntax::ast::{Convention, Type, TypeQual};
    let Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    // The direct shared-reference slice carries an already-addressable
    // reference-counted shell (String/Bytes) or the separately
    // authenticated scalar-place owner. Direct exclusive references are a
    // second shallow exception: only a plain declaration parameter receives
    // the authenticated hidden write-back lane. It is deliberately not a
    // recursive qualifier exception.
    let mut pending = (!(is_direct_shared_reference(result)
        || is_storage_backed_direct_view_carrier(result, records)
        || is_direct_exclusive_reference(result)
        || witchy_canonical_source::storage::affine_aggregate_erased_type(result, records).is_some()))
        .then_some((result.as_ref(), true))
        .into_iter()
        .collect::<Vec<_>>();
    pending.extend(parameters.iter().enumerate().filter_map(|(index, parameter)| {
        (!(
            is_direct_shared_reference(parameter)
            || is_storage_backed_direct_view_carrier(parameter, records)
            || (matches!(conventions.get(index), Some(Convention::Own | Convention::Var))
                && witchy_canonical_source::storage::affine_aggregate_erased_type(parameter, records).is_some())
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
                    && is_storage_backed_rooted_view(ty, records) => {}
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

/// A direct view keeps its checked representation class. Aggregate and list
/// owners are admitted only at the callable boundary; a nested view must
/// prove its own root or projection instead of inheriting that exception.
fn is_storage_backed_direct_view_carrier(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_canonical_source::storage::StoragePlan;
    is_storage_backed_rooted_view(source, records)
        || matches!(StoragePlan::classify(source.unqualified(), records, |_| Some(0)),
            StoragePlan::RetainedOwnerList(_)
                | StoragePlan::AggregateView(witchy_canonical_source::aggregates::ViewCarrierPlan::Aggregate { .. }, _))
}

fn is_storage_backed_rooted_view(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_canonical_source::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage, RootedViewStorage};
    use witchy_canonical_source::storage::StoragePlan;
    match StoragePlan::classify(source, records, |_| Some(0)) {
        StoragePlan::RootedView(RootedViewStorage::InlineScalar(_) | RootedViewStorage::RetainedRoot(_)) => true,
        StoragePlan::RootedView(RootedViewStorage::ScalarCell(_) | RootedViewStorage::RetainedProjection(_)) => {
            OwnerCarriedViewPlan::classify(source, records).is_some_and(|plan| matches!(
                plan.admitted_storage(),
                Some(OwnerCarriedViewStorage::AuthenticatedPlace(_) | OwnerCarriedViewStorage::RetainedProjection(_))
            ))
        }
        _ => false,
    }
}

/// The only nested callable ABI whose hidden write-back lane is presently
/// authenticated by canonical lowering and the adapter binding checker.
/// This is intentionally structural *only after* the outer source declaration
/// has been bound to its checked access contract; it never admits shared,
/// returned, projected, generic, or multi-parameter reference callables.
pub(super) fn is_direct_callable_writeback_carrier(ty: &witchy_syntax::ast::Type) -> bool {
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

pub(super) fn is_direct_shared_int(source: &witchy_syntax::ast::Type) -> bool {
    use witchy_canonical_source::abi::CanonicalScalarKind;
    use witchy_canonical_source::storage::StoragePlan;
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(StoragePlan::classify(inner.unqualified(), &BTreeMap::new(), |_| None),
                StoragePlan::InlineScalar(CanonicalScalarKind::Int)))
}

/// A checked scalar-place owner leaf; the shared classifier currently proves
/// this representation only for an exact Int payload.
pub(super) fn is_direct_rooted_scalar_place(source: &witchy_syntax::ast::Type) -> bool {
    use witchy_canonical_source::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage};
    OwnerCarriedViewPlan::classify(source, &BTreeMap::new()).is_some_and(|plan| {
        matches!(plan.admitted_storage(), Some(OwnerCarriedViewStorage::AuthenticatedPlace(_)))
    })
}

/// A `&String` view is its retained String token (`StringRoot`), so inside
/// an aggregate (`Option(&String)`, `(&String, Int)`, a record field) it has
/// exactly the String layout.  The callable's own direct `&String` lanes keep
/// their dedicated whole-root correspondence and are left as written.
pub(super) fn erase_nested_byte_buffer_views(signature: &witchy_syntax::ast::Type, records: &ScalarRecordFields) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    let lane = |ty: &Type| -> Type {
        if matches!(witchy_canonical_source::storage::StoragePlan::classify(ty, records, |_| Some(0)),
            witchy_canonical_source::storage::StoragePlan::AggregateView(_, _)) {
            return ty.clone();
        }
        match ty {
            Type::Named(_, _) | Type::Tuple(_) => erase_byte_buffer_view(ty, records),
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
pub(super) fn erase_scalar_value_views(ty: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(witchy_canonical_source::storage::StoragePlan::classify(
                ty, &BTreeMap::new(), |_| None,
            ), witchy_canonical_source::storage::StoragePlan::RootedView(
                witchy_canonical_source::rooted_view::RootedViewStorage::InlineScalar(_))) =>
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

/// Read a byte-buffer view as its retained buffer token in nested storage.
pub(super) fn erase_byte_buffer_view(
    ty: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    // A managed carrier containing a retained-root view preserves the
    // checked qualifier relation through signature correspondence.
    if matches!(witchy_canonical_source::storage::StoragePlan::classify(ty, records, |_| Some(0)),
        witchy_canonical_source::storage::StoragePlan::Owned(carrier)
            if carrier.contains_retained_root_view())
    {
        return ty.clone();
    }
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(witchy_canonical_source::storage::StoragePlan::classify(
                ty, &BTreeMap::new(), |_| None,
            ), witchy_canonical_source::storage::StoragePlan::RootedView(
                witchy_canonical_source::rooted_view::RootedViewStorage::RetainedRoot(
                    witchy_canonical_source::rooted_view::RetainedRootStorage::ByteBuffer(_)))) =>
        {
            inner.unqualified().clone()
        }
        Type::Qualified(qualifier, inner) => Type::Qualified(qualifier.clone(), Box::new(erase_byte_buffer_view(inner, records))),
        Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter()
            .map(|argument| erase_byte_buffer_view(argument, records)).collect()),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|item| erase_byte_buffer_view(item, records)).collect()),
        other => other.clone(),
    }
}

/// The same reading for record and sum fields, used only for signature
/// correspondence: a retained byte-buffer view uses its buffer field.
pub(super) fn erase_record_byte_buffer_views(records: &ScalarRecordFields) -> ScalarRecordFields {
    records.iter().map(|(name, definition)| {
        let mut definition = definition.clone();
        for variant in &mut definition.variants {
            variant.fields = variant.fields.iter().map(|field| erase_byte_buffer_view(field, records)).collect();
        }
        (name.clone(), definition)
    }).collect()
}

/// A direct view whose checked storage plan is an inline scalar or retained
/// whole root. The structural payload proof chooses the exact String, List,
/// tuple, dictionary, record, or sum identity; a slice projection and scalar
/// place remain separate physical classes.
pub(crate) fn is_direct_rooted_view_whole_root(source: &witchy_syntax::ast::Type, records: &ScalarRecordFields) -> bool {
    use witchy_canonical_source::rooted_view::RootedViewStorage;
    use witchy_canonical_source::storage::StoragePlan;
    matches!(StoragePlan::classify(source, records, |_| Some(0)),
        StoragePlan::RootedView(RootedViewStorage::InlineScalar(_) | RootedViewStorage::RetainedRoot(_)))
}

/// A retained projection whose recursive element has checked ownership.
pub(super) fn is_direct_rooted_projection(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_canonical_source::rooted_view::{OwnerCarriedViewPlan, OwnerCarriedViewStorage};
    OwnerCarriedViewPlan::classify(source, records).is_some_and(|plan| {
        matches!(plan.admitted_storage(), Some(OwnerCarriedViewStorage::RetainedProjection(_)))
    })
}

/// The only non-scalar reference carriers the current canonical ABI can move
/// without manufacturing a place token.  Their source relation is still
/// authenticated by the retained access signature and an alias-result
/// contract; this predicate is intentionally shallow so a borrowed field,
/// aggregate, callable, or mutable handle cannot inherit the exception.
pub(super) fn is_direct_shared_owned_reference(source: &witchy_syntax::ast::Type) -> bool {
    use witchy_canonical_source::carrier::CarrierElement;
    use witchy_canonical_source::storage::StoragePlan;
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(StoragePlan::classify(inner.unqualified(), &BTreeMap::new(), |_| None),
                StoragePlan::Owned(CarrierElement::String | CarrierElement::Bytes)))
}

pub(super) fn is_direct_shared_reference(source: &witchy_syntax::ast::Type) -> bool {
    is_direct_shared_int(source) || is_direct_shared_owned_reference(source)
}

/// A non-packed aggregate with a checked view-carrier field. The opaque
/// aggregate family and constructor are revalidated independently; this
/// predicate preserves the exact checked aggregate identity in source
/// correspondence.
pub(super) fn is_direct_rooted_view_aggregate_carrier(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    matches!(
        witchy_canonical_source::storage::StoragePlan::classify(
            source.unqualified(), records, |_| Some(0),
        ),
        witchy_canonical_source::storage::StoragePlan::AggregateView(
            witchy_canonical_source::aggregates::ViewCarrierPlan::Aggregate { .. }, _
        )
    )
}

pub(super) fn is_direct_exclusive_referent(source: &witchy_syntax::ast::Type) -> bool {
    use witchy_canonical_source::abi::CanonicalScalarKind;
    use witchy_canonical_source::carrier::CarrierElement;
    use witchy_canonical_source::storage::StoragePlan;
    matches!(StoragePlan::classify(source.unqualified(), &BTreeMap::new(), |_| None),
        StoragePlan::InlineScalar(CanonicalScalarKind::Int) | StoragePlan::Owned(CarrierElement::String))
}

// This is a leaf prefilter only.  Projected return authority is minted below
// from the declaration's final field expression and its pointer-bound loan,
// never from this physical type predicate.
pub(super) fn is_projected_exclusive_leaf(source: &witchy_syntax::ast::Type) -> bool {
    is_direct_exclusive_referent(source)
}

pub(super) fn is_direct_exclusive_reference(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(_), inner)
            if is_direct_exclusive_referent(inner))
}

pub(super) fn is_root_unique(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _))
}

pub(super) fn direct_shared_int_matches(
    actual: super::super::ValueType,
    source: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let super::super::ValueType::Ref(layout) = actual else { return false };
    is_direct_shared_int(source)
        && !matches!(layouts.get(layout).map(|descriptor| descriptor.kind()),
            Some(crate::layout::LayoutKind::OpaqueNominal { .. }))
        && layouts.resolve_nominal(layout).ok()
            .and_then(|physical| layouts.get(physical))
            .is_some_and(|descriptor| descriptor.kind() == &crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::Int))
}

pub(super) fn restore_source_abi_qualifiers(
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
                || is_direct_rooted_scalar_place(ty) || is_direct_rooted_projection(ty, records)
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
            || is_direct_rooted_scalar_place(declared) || is_direct_rooted_projection(declared, records)
            || is_direct_rooted_view_whole_root(declared, records)
            || is_direct_exclusive_reference(declared)
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
pub(super) fn authenticated_projected_scalar_relay_relation_is_checked(
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
pub(super) fn restore_authenticated_projected_scalar_relay_abi(
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
