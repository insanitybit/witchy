use super::*;

/// A family selects environment variants, not callable authority. Resolve
/// checked nominal aliases and spell out default conventions before grouping;
/// full signature, capture, transfer, and tag checks remain independent above.
pub(crate) fn source_closure_family_key(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::{CallableQualifiers, Convention, Type};
    fn normalize(source: &Type, records: &ScalarRecordFields) -> Type {
        if let Some(view) = witchy_canonical_source::aggregates::owner_carried_view_identity(source, records) {
            return view;
        }
        match source.unqualified() {
            Type::Fn(parameters, result, conventions, _) => Type::Fn(
                parameters.iter().map(|parameter| normalize(parameter, records)).collect(),
                Box::new(normalize(result, records)),
                if conventions.is_empty() { vec![Convention::Let; parameters.len()] }
                else { conventions.clone() },
                CallableQualifiers::default(),
            ),
            Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(|argument| normalize(argument, records)).collect()),
            Type::Tuple(fields) => Type::Tuple(fields.iter().map(|field| normalize(field, records)).collect()),
            other => other.clone(),
        }
    }
    normalize(&witchy_canonical_source::aggregates::concrete_aggregate_identity(source, records)
        .unwrap_or_else(|| source.unqualified().clone()), records)
}

pub(super) const MAX_SOURCE_CORRESPONDENCE_PAIRS: usize = 65_536;

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
                    Some(true) if view.record_root_plan(payload).is_none() => return false,
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
pub(crate) struct CorrespondenceContext {
    pub(super) owner_families: Vec<super::super::closure_owner_contract::OwnerFamily>,
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

impl CorrespondenceContext {
    pub(crate) fn with_owners<S: ModuleStage>(candidate: &CanonicalModule<S>) -> Self {
        Self { owner_families: candidate.owner_families().to_vec(), ..Self::default() }
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

/// `List(&Int)`: the `&Int` family's owner list. Each element is one retained,
/// authenticated place carrier; the list itself is an ordinary owner list.
pub(super) fn is_list_of_direct_int_views(ty: &witchy_syntax::ast::Type) -> bool {
    matches!(ty.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "List" && arguments.len() == 1 && is_direct_rooted_view_int(&arguments[0]))
}

/// Check the original source graph before any ABI comparison erases wrappers.
/// A root Unique on an own/var input or result is represented by the callable
/// ownership contract. The exact Result-Ok collection carrier and a
/// `List(&Int)` parameter or result are the authenticated nested exceptions;
/// all other nested qualifiers remain unsupported.
pub(super) fn source_abi_qualifiers_supported(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    use witchy_syntax::ast::{Convention, Type, TypeQual};
    let Type::Fn(parameters, result, conventions, _) = source.unqualified() else { return false };
    // The direct shared-reference slice carries an already-addressable
    // reference-counted shell (String/Bytes/List(Int)) or the separately
    // authenticated scalar-place owner. Direct exclusive references are a
    // second shallow exception: only a plain declaration parameter receives
    // the authenticated hidden write-back lane. It is deliberately not a
    // recursive qualifier exception.
    let mut pending = (!(is_direct_shared_reference(result)
        || is_direct_rooted_view_int(result) || is_direct_rooted_view_slice(result, records)
        || is_list_of_direct_int_views(result)
        || is_direct_rooted_view_whole_root(result, records)
        || is_direct_exclusive_reference(result)
        || is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records)
        || witchy_syntax::ast::is_direct_unique_result_ok_collection(result)))
        .then_some((result.as_ref(), true))
        .into_iter()
        .collect::<Vec<_>>();
    pending.extend(parameters.iter().enumerate().filter_map(|(index, parameter)| {
        (!(
            is_direct_shared_reference(parameter)
            || is_direct_rooted_view_int(parameter) || is_direct_rooted_view_slice(parameter, records)
            || is_list_of_direct_int_views(parameter)
            || is_direct_rooted_view_whole_root(parameter, records)
            || is_direct_rooted_view_aggregate_carrier(parameter, records)
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
                    && (is_direct_rooted_view_int(ty) || is_direct_rooted_view_slice(ty, records)
                        || is_direct_rooted_view_whole_root(ty, records)) => {}
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
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
}

/// The initial RFC-0167 view carrier is deliberately narrower than the
/// legacy reference ABI: only a lifetime-free `&Int` maps to the registered
/// opaque root-plus-projection owner family.  This is an exact source-type
/// match, not a representation or shape fallback.
pub(super) fn is_direct_rooted_view_int(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
}

/// A `&String` view is its retained String token (`StringRoot`), so inside
/// an aggregate (`Option(&String)`, `(&String, Int)`, a record field) it has
/// exactly the String layout.  The callable's own direct `&String` lanes keep
/// their dedicated whole-root correspondence and are left as written.
pub(super) fn erase_nested_string_views(signature: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    let lane = |ty: &Type| -> Type {
        match ty {
            Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(erase_string_view).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(erase_string_view).collect()),
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
            if witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner) =>
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

/// Read every `&String` in `ty` as `String`: the view's carrier is the token.
pub(super) fn erase_string_view(ty: &witchy_syntax::ast::Type) -> witchy_syntax::ast::Type {
    use witchy_syntax::ast::Type;
    match ty {
        Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), Type::Named(name, arguments) if name == "String" && arguments.is_empty()) =>
        {
            Type::Named("String".into(), Vec::new())
        }
        Type::Qualified(qualifier, inner) => Type::Qualified(qualifier.clone(), Box::new(erase_string_view(inner))),
        Type::Named(name, arguments) => Type::Named(name.clone(), arguments.iter().map(erase_string_view).collect()),
        Type::Tuple(items) => Type::Tuple(items.iter().map(erase_string_view).collect()),
        other => other.clone(),
    }
}

/// The same reading for record and sum fields, used only for signature
/// correspondence: a `&String` field is laid out as a String field.
pub(super) fn erase_record_string_views(records: &ScalarRecordFields) -> ScalarRecordFields {
    records.iter().map(|(name, definition)| {
        let mut definition = definition.clone();
        for variant in &mut definition.variants {
            variant.fields = variant.fields.iter().map(erase_string_view).collect();
        }
        (name.clone(), definition)
    }).collect()
}

/// A whole-root view carried as the retained object token: `&String`
/// (`StringRoot`) or a view of a declared, non-packed, argument-less user
/// record or sum (`RecordRoot`), exact `Dict(String, S)` with an admitted
/// unqualified inline scalar or String `S` (`DictRoot`), or
/// exact `(S, String)` for admitted inline scalar or String `S` (`TupleRoot`).
/// None has a projection.
pub(crate) fn is_direct_rooted_view_whole_root(source: &witchy_syntax::ast::Type, records: &ScalarRecordFields) -> bool {
    is_direct_rooted_view_string(source)
        || matches!(source,
            witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
                if witchy_canonical_source::aggregates::is_record_root_payload(inner, records)
                    || witchy_canonical_source::carrier::CarrierElement::from_list_root_payload_type_with_definitions(inner, records).is_some()
                    || witchy_canonical_source::carrier::CarrierElement::retained_structural_root(inner, records).is_some()
                    || witchy_canonical_source::aggregates::is_scalar_value_view_payload(inner))
}

/// A whole immutable `&String` view: carried as the retained String token
/// itself (`StringRoot`), so it corresponds to the plain String layout.
pub(super) fn is_direct_rooted_view_string(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::View, inner)
            if matches!(inner.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "String" && arguments.is_empty()))
}

/// A direct `&[T]` rooted slice view of an admitted element (`ListSlice`).
pub(super) fn is_direct_rooted_view_slice(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    witchy_canonical_source::carrier::CarrierElement::from_view_type_with_definitions(source, records).is_some()
}

/// The only non-scalar reference carriers the current canonical ABI can move
/// without manufacturing a place token.  Their source relation is still
/// authenticated by the retained access signature and an alias-result
/// contract; this predicate is intentionally shallow so a borrowed field,
/// aggregate, callable, or mutable handle cannot inherit the exception.
pub(super) fn is_direct_shared_owned_reference(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(_), inner)
            if matches!(inner.unqualified(),
                witchy_syntax::ast::Type::Named(name, arguments)
                    if (matches!(name.as_str(), "String" | "Bytes") && arguments.is_empty())
                        || (name == "List" && arguments.len() == 1
                            && matches!(arguments[0].unqualified(),
                                witchy_syntax::ast::Type::Named(element, element_arguments)
                                    if element == "Int" && element_arguments.is_empty()))))
}

pub(super) fn is_direct_shared_reference(source: &witchy_syntax::ast::Type) -> bool {
    is_direct_shared_int(source) || is_direct_shared_owned_reference(source)
}

/// The physical aggregate ABI can store only these bounded reference leaves.
/// This deliberately proves no alias authority by itself: a declaration must
/// also pass `aggregate_reference_result_relation_is_checked` against its
/// pointer-bound access facts before this structural exception is usable.
pub(super) fn is_aggregate_reference_carrier(source: &witchy_syntax::ast::Type) -> bool {
    match source.unqualified() {
        witchy_syntax::ast::Type::Tuple(items) => !items.is_empty()
            && items.iter().all(|item| {
                is_direct_shared_owned_reference(item) || is_direct_exclusive_reference(item)
            }),
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "List" && arguments.len() == 1 =>
        {
            is_direct_shared_owned_reference(&arguments[0])
        }
        // `Result` carries a tagged owned shell.  The only aggregate Result
        // admitted by this ABI is an `Ok` tuple of direct shared roots with a
        // String `Err` lane.  Its tag and every payload projection are then
        // checked below against the retained source relation product.
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "Result" && arguments.len() == 2 =>
        {
            is_shared_reference_tuple_carrier(&arguments[0])
                && is_string_source_type(&arguments[1])
        }
        // `Option(&Int)` has one rooted-view payload in `Some` and no payload
        // in `None`.  Its opaque aggregate owner is checked against the exact
        // source identity below; do not erase it into the linear Option(Int)
        // physical layout.
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "Option" && arguments.len() == 1 =>
        {
            witchy_canonical_source::aggregates::is_rooted_int_option_payload_carrier(&arguments[0], &BTreeMap::new())
        }
        _ => false,
    }
}

/// A non-packed aggregate of `&Int` views, or a named record with one direct
/// exact `&[Int]` field (see `witchy_canonical_source::aggregates::is_rooted_int_view_aggregate`). The opaque
/// aggregate family and constructor are revalidated independently; this
/// predicate only prevents source correspondence from erasing the exact
/// checked aggregate identity.
pub(super) fn is_direct_rooted_view_aggregate_carrier(
    source: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    witchy_canonical_source::aggregates::is_rooted_int_view_aggregate(source, records)
}

pub(super) fn is_shared_reference_tuple_carrier(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Tuple(items)
        if !items.is_empty() && items.iter().all(is_direct_shared_owned_reference))
}

pub(super) fn is_string_source_type(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "String" && arguments.is_empty())
}

pub(super) fn is_direct_exclusive_referent(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(),
        witchy_syntax::ast::Type::Named(name, arguments)
            if arguments.is_empty() && matches!(name.as_str(), "String" | "Int"))
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
                || is_direct_rooted_view_int(ty) || is_direct_rooted_view_slice(ty, records)
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
            || is_direct_rooted_view_int(declared) || is_direct_rooted_view_slice(declared, records)
            || is_direct_rooted_view_whole_root(declared, records)
            || is_direct_exclusive_reference(declared)
            || is_aggregate_reference_carrier(declared)
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
