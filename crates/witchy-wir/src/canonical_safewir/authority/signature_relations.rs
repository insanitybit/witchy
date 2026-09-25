use super::*;

pub(super) fn candidate_signature_matches_source_inner(
    actual: &super::super::FunctionSignature,
    source: &witchy_syntax::ast::Type,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
    signatures: &[super::super::FunctionSignature],
    context: &mut CorrespondenceContext,
    records: &ScalarRecordFields,
    closure_layouts: &BTreeMap<crate::layout::LayoutId, super::super::FunctionSignatureId>,
) -> bool {
    if !source_abi_qualifiers_supported(source, records) { return false; }
    let key = (actual.id, source.unqualified().clone());
    match context.enter_callable(key.clone()) {
        CorrespondenceVisit::Active | CorrespondenceVisit::Completed => return true,
        CorrespondenceVisit::Limit => return false,
        CorrespondenceVisit::New => {}
    }
    if actual.parameters.len() != parameters.len() || actual.contract.parameters.len() != parameters.len() {
        context.finish_callable(key, false);
        return false;
    }
    let convention_at = |index| conventions.get(index).copied().unwrap_or_default();
    if !actual.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        (matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own)
                && (ordinary_own_source_type_supported(source)
                    || matches!(actual, super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_))))
        && (direct_shared_int_matches(*actual, source, layouts)
            || value_type_matches_source(*actual, source, layouts, signatures, context, records, closure_layouts))
    }) {
        context.finish_callable(key, false);
        return false;
    }
    if !actual.contract.parameters.iter().zip(parameters).enumerate().all(|(index, (actual, source))| {
        let convention = convention_at(index);
        if !(matches!(convention, witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Var)
            || matches!(convention, witchy_syntax::ast::Convention::Own)
                && (ordinary_own_source_type_supported(source)
                    || matches!(actual.value_type, super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_))))
            || !(direct_shared_int_matches(actual.value_type, source, layouts)
                || value_type_matches_source(actual.value_type, source, layouts, signatures, context, records, closure_layouts)) { return false; }
        if direct_shared_int_matches(actual.value_type, source, layouts) {
            return actual.ownership == super::super::ParameterOwnership::Shared;
        }
        if is_direct_exclusive_reference(source) {
            // The hidden lane is authenticated below against this exact plain
            // declaration parameter. An `own` direct handle consumes the
            // handle but still writes its referent replacement back through
            // that lane. No `var` alias, projection, or callable ABI can
            // borrow this exception.
            return matches!(convention,
                witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
                && actual.ownership == super::super::ParameterOwnership::WriteBack;
        }
        if is_direct_shared_owned_reference(source) {
            // The checked reference is already a linear-memory RC token.  It
            // is transferred as a value token, while an aliasing result below
            // records the retained source relation instead of relabeling this
            // ordinary owner as an untracked shared allocation.
            // An authenticated aggregate result differs narrowly: lowering
            // must clone every source root into a distinct aggregate field,
            // so the callee receives an explicit shared token to consume or
            // release.  `aggregate_reference_result_relation_is_checked`
            // separately proves the source relation product; a matching Ref
            // layout or parameter spelling is never sufficient.
            return actual.ownership == if is_aggregate_reference_carrier(result) {
                super::super::ParameterOwnership::Shared
            } else {
                super::super::ParameterOwnership::Value
            };
        }
        if matches!(source, witchy_syntax::ast::Type::Qualified(
            witchy_syntax::ast::TypeQual::Unique, _
        )) && convention == witchy_syntax::ast::Convention::Own {
            // Root `unique` is a source-level affine fact. Managed roots
            // carry it in SafeWIR's ownership token, while an inline scalar
            // has no addressable storage and therefore retains its ordinary
            // value lane. Keep the distinction in the candidate witness:
            // accepting `Value` is confined to the scalar representations
            // which cannot carry or erase a managed ownership token.
            return root_unique_parameter_ownership_matches(actual.value_type, actual.ownership);
        }
        match (actual.value_type, convention) {
            // `Borrow` is the explicitly written `let` convention; `Let` is the
            // default one. Both are non-owning reads of the same GC-carried
            // value, so they take the same Shared ownership. Without `Borrow`
            // here a `let value: &Int` parameter fell through to the `Value`
            // arm and its signature was refused as a mismatch.
            (super::super::ValueType::Gc(_), witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Borrow | witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::super::ParameterOwnership::Shared),
            // `var view: &T` replaces the caller's immutable GC carrier by
            // write-back; the viewed value is never mutated through it.
            // The replacement lane is authenticated below as the exact same
            // source type, so the lane cannot change the carrier's family.
            (super::super::ValueType::Gc(_), witchy_syntax::ast::Convention::Var) => actual.ownership == super::super::ParameterOwnership::WriteBack,
            (super::super::ValueType::Gc(_), _) => false,
            (super::super::ValueType::Ref(_), witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::super::ParameterOwnership::Shared),
            (super::super::ValueType::Ref(_), witchy_syntax::ast::Convention::Var)
                if matches!(source, witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _)) =>
                    actual.ownership == super::super::ParameterOwnership::UniqueWriteBack,
            (_, witchy_syntax::ast::Convention::Var) => actual.ownership == super::super::ParameterOwnership::WriteBack,
            (_, witchy_syntax::ast::Convention::Own) => matches!(actual.ownership, super::super::ParameterOwnership::Value),
            (super::super::ValueType::Ref(_), _) => matches!(actual.ownership, super::super::ParameterOwnership::Shared),
            _ => matches!(actual.ownership, super::super::ParameterOwnership::Value),
        }
    }) {
        context.finish_callable(key, false);
        return false;
    }
    let result_is_unit = is_unit_source_type(result);
    // The Result wrapper has the ordinary physical layout. Its authenticated
    // Unique contract above proves the exact declared Ok carrier, so only this
    // bounded case compares the physical projection rather than recursively
    // erasing nested qualifiers during correspondence.
    let physical_result = witchy_syntax::ast::direct_unique_result_ok_collection_physical_type(result);
    let ordinary_results = usize::from(!result_is_unit);
    let write_back_parameters = parameters.iter().enumerate()
        .filter(|(index, _)| matches!(convention_at(*index), witchy_syntax::ast::Convention::Var))
        .collect::<Vec<_>>();
    let direct_exclusive_parameters = parameters.iter().enumerate()
        .filter(|(index, parameter)| {
            matches!(convention_at(*index),
                witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
                && is_direct_exclusive_reference(parameter)
        })
        .collect::<Vec<_>>();
    // The only extra ordinary ABI lane is the authenticated Bool selector for
    // the bounded two-root direct result. It precedes every hidden write-back
    // lane, so no generic arity arithmetic can mistake it for a replacement.
    let disjunctive_selector_lanes = usize::from(
        direct_exclusive_parameters.len() == 2
            && direct_exclusive_reference_parts(result).is_some(),
    );
    let abi_ordinary_results = ordinary_results + disjunctive_selector_lanes;
    let source_unique_result = matches!(result,
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, _))
        || witchy_syntax::ast::is_direct_unique_result_ok_collection(result);
    let unique_results_match = actual.contract.results.iter().enumerate().all(|(index, contract)| {
        let unique_parameter = index.checked_sub(abi_ordinary_results)
            .and_then(|offset| write_back_parameters.get(offset))
            .is_some_and(|(parameter, _)| actual.contract.parameters[*parameter].ownership == super::super::ParameterOwnership::UniqueWriteBack);
        let expected_unique = (index == 0 && !result_is_unit && source_unique_result
            && actual.results.get(index).is_some_and(root_unique_result_requires_token))
            || unique_parameter;
        expected_unique == matches!(contract, super::super::ResultContract::Unique { .. })
    });
    let direct_alias_matches = direct_shared_alias_result_matches(
        actual,
        parameters,
        result,
        layouts,
    );
    let direct_exclusive_alias_matches = direct_exclusive_alias_result_matches(
        actual,
        parameters,
        result,
        conventions,
        layouts,
    );
    // A checked `&mut Record.fixed_int` has no physical Int address to pass
    // around.  The candidate carries the authenticated record root instead;
    // its only readable/writable child remains the exact result-contract
    // projection, independently bound by the declaration's loan relation.
    let projected_scalar_result_matches = projected_exclusive_signature_parameter(
        parameters,
        result,
        conventions,
    ).is_some_and(|parameter| {
        direct_exclusive_reference_parts(result)
            .is_some_and(|(_, leaf)| matches!(leaf.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
                if name == "Int" && arguments.is_empty()))
            && matches!(
                (actual.results.first(), actual.contract.results.first(), actual.parameters.get(parameter)),
                (
                    Some(super::super::ValueType::Ref(root)),
                    Some(super::super::ResultContract::AliasesWriteBackParameter {
                        parameter: result_parameter,
                        layout,
                        projection: Some(_),
                    }),
                    Some(super::super::ValueType::Ref(parameter_root)),
                ) if *root == *layout && *root == *parameter_root
                    && usize::try_from(*result_parameter).ok() == Some(parameter)
            )
    });
    let ordinary_result_matches = result_is_unit
        || projected_scalar_result_matches
        || value_type_matches_source(
            actual.results[0],
            physical_result.as_ref().unwrap_or(result),
            layouts,
            signatures,
            context,
            records,
            closure_layouts,
        );
    let matched = unique_results_match && direct_alias_matches && direct_exclusive_alias_matches
        && aggregate_reference_result_contract_matches(actual, result, records)
        && actual.results.len() == abi_ordinary_results + write_back_parameters.len() + direct_exclusive_parameters.len()
        && ordinary_result_matches
        && write_back_parameters.iter().enumerate().all(|(offset, (parameter, source))| {
            value_type_matches_source(actual.results[abi_ordinary_results + offset], source, layouts, signatures, context, records, closure_layouts)
                && matches!(actual.contract.parameters[*parameter].ownership, super::super::ParameterOwnership::WriteBack | super::super::ParameterOwnership::UniqueWriteBack)
        })
        && direct_exclusive_parameters.iter().enumerate().all(|(offset, (parameter, source))| {
            let result_index = abi_ordinary_results + write_back_parameters.len() + offset;
            value_type_matches_source(actual.results[result_index], source, layouts, signatures, context, records, closure_layouts)
                && actual.contract.parameters[*parameter].ownership == super::super::ParameterOwnership::WriteBack
                && direct_exclusive_result_contract_matches(
                    actual.contract.results.get(result_index), actual.results[result_index],
                )
        })
        && actual.contract.parameter_effects.iter().all(|(parameter, effects)| {
            let read_only = effects.iter().all(|effect| matches!(effect, super::super::MemoryEffect::Read | super::super::MemoryEffect::Retain | super::super::MemoryEffect::Release));
            // A source `var` GC carrier is replaced whole through its hidden
            // lane; it is immutable, so nothing may be written through it.
            let gc_var = matches!(actual.parameters.get(*parameter as usize), Some(super::super::ValueType::Gc(_)))
                && convention_at(*parameter as usize) == witchy_syntax::ast::Convention::Var;
            matches!(actual.parameters.get(*parameter as usize), Some(super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_)))
                && (!gc_var && matches!(actual.contract.parameters.get(*parameter as usize).map(|parameter| parameter.ownership), Some(super::super::ParameterOwnership::WriteBack | super::super::ParameterOwnership::UniqueWriteBack))
                    || read_only)
        });
    context.finish_callable(key, matched);
    matched
}

pub(super) fn direct_exclusive_result_contract_matches(
    contract: Option<&super::super::ResultContract>,
    value_type: super::super::ValueType,
) -> bool {
    match (contract, value_type) {
        (Some(super::super::ResultContract::Shared { layout }), super::super::ValueType::Ref(actual)) => *layout == actual,
        (Some(super::super::ResultContract::Value(actual)), value_type) => *actual == value_type,
        _ => false,
    }
}

/// The aggregate shell is an independently owned, shared result token.  The
/// leaf-to-owner proof lives in the retained source `BorrowRelation` product;
/// this SafeWIR-side check merely refuses to let that proven carrier degrade
/// into a fresh/value result with the same physical layout.
pub(super) fn aggregate_reference_result_contract_matches(
    actual: &super::super::FunctionSignature,
    result: &witchy_syntax::ast::Type,
    records: &ScalarRecordFields,
) -> bool {
    if !(is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records)) {
        return true;
    }
    if is_direct_rooted_view_aggregate_carrier(result, records) {
        return matches!(
            (actual.results.first(), actual.contract.results.first()),
            (Some(super::super::ValueType::Gc(owner)), Some(super::super::ResultContract::Value(super::super::ValueType::Gc(contract_owner))))
                if owner == contract_owner
        );
    }
    matches!(
        (actual.results.first(), actual.contract.results.first()),
        (Some(super::super::ValueType::Ref(value_layout)), Some(super::super::ResultContract::Shared { layout }))
            if value_layout == layout
    )
}

/// An inline scalar has no addressable storage and no SafeWIR ownership token.
/// This is deliberately the same narrow physical family canonical lowering
/// permits for a root `unique` result. All managed values must still carry the
/// explicit `Unique` parameter/result contract.
pub(super) const fn inline_scalar_value_type(value: super::super::ValueType) -> bool {
    matches!(
        value,
        super::super::ValueType::Int
            | super::super::ValueType::Float
            | super::super::ValueType::Bool
            | super::super::ValueType::Unit
            | super::super::ValueType::Size
            | super::super::ValueType::V128
    )
}

/// The source witness cannot let an inline scalar impersonate a managed owner,
/// nor a managed (`Ref` or `Gc`) owner erase its token.
pub(super) fn root_unique_parameter_ownership_matches(
    value: super::super::ValueType,
    ownership: super::super::ParameterOwnership,
) -> bool {
    match value {
        super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_) => {
            ownership == super::super::ParameterOwnership::Unique
        }
        value if inline_scalar_value_type(value) => ownership == super::super::ParameterOwnership::Value,
        _ => false,
    }
}

pub(super) fn root_unique_result_requires_token(value: &super::super::ValueType) -> bool {
    matches!(value, super::super::ValueType::Ref(_) | super::super::ValueType::Gc(_))
}

/// A direct shared result must retain the exact one checked input relation.
/// This is intentionally narrower than general loan lowering: it accepts only
/// the direct, whole-value carriers admitted above.  A projected/nested or
/// ambiguous result stays unsupported instead of being made to look like an
/// ordinary owned result.
pub(super) fn direct_shared_alias_result_matches(
    actual: &super::super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Some((lifetime, referent)) = direct_shared_reference_parts(result) else {
        return !is_direct_shared_reference(result);
    };
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        direct_shared_reference_parts(parameter).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    let [parameter] = candidates.as_slice() else { return false };
    let Some(super::super::ResultContract::AliasesParameter { parameter: actual_parameter, layout }) =
        actual.contract.results.first()
    else {
        return false;
    };
    usize::try_from(*actual_parameter).ok() == Some(*parameter)
        && matches!(actual.results.first(), Some(super::super::ValueType::Ref(actual_layout)) if actual_layout == layout)
        && layouts.resolve_nominal(*layout).is_ok()
}

pub(super) fn direct_shared_reference_parts(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type)> {
    let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Borrow(lifetime), referent) = source else {
        return None;
    };
    is_direct_shared_reference(source).then_some((lifetime, referent))
}

pub(super) fn direct_exclusive_reference_parts(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type)> {
    let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(lifetime), referent) = source else {
        return None;
    };
    is_direct_exclusive_referent(referent).then_some((lifetime, referent))
}

/// The direct result is the same checked loan relation as one exact direct
/// exclusive input. The distinct hidden write-back lane is checked separately
/// below; this ordinary lane may never be forged from matching physical refs.
pub(super) fn direct_exclusive_alias_result_matches(
    actual: &super::super::FunctionSignature,
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
    layouts: &crate::layout::LayoutInterner,
) -> bool {
    let Some((lifetime, referent)) = direct_exclusive_reference_parts(result) else {
        return true;
    };
    if !is_projected_exclusive_leaf(referent) {
        return false;
    }
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        direct_exclusive_reference_parts(parameter).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime
                && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    if let [parameter] = candidates.as_slice() {
        let Some(super::super::ResultContract::AliasesWriteBackParameter {
            parameter: actual_parameter,
            layout,
            projection: None,
        }) = actual.contract.results.first()
        else {
            return false;
        };
        return usize::try_from(*actual_parameter).ok() == Some(*parameter)
            && actual.contract.parameters.get(*parameter).is_some_and(|contract| {
                contract.ownership == super::super::ParameterOwnership::WriteBack
                    && contract.value_type == super::super::ValueType::Ref(*layout)
            })
            && matches!(actual.results.first(), Some(super::super::ValueType::Ref(actual_layout)) if actual_layout == layout)
            && layouts.resolve_nominal(*layout).is_ok();
    }
    if candidates.len() >= 2 {
        let expected = candidates
            .iter()
            .map(|parameter| u32::try_from(*parameter).ok())
            .collect::<Option<Vec<_>>>();
        let Some(expected) = expected else { return false };
        let Some(super::super::ResultContract::AliasesOneOfWriteBackParameters {
            parameters: actual_parameters,
            layout,
        }) = actual.contract.results.first()
        else {
            return false;
        };
        return expected.len() == 2
            && actual_parameters == &expected
            && matches!(actual.contract.results.get(1), Some(super::super::ResultContract::DisjunctiveWriteBackSelector {
                carrier_result: 0,
                parameters,
                selector_parameter,
            }) if parameters == &expected
                && matches!(actual.results.get(1), Some(super::super::ValueType::Bool))
                && matches!(actual.contract.parameters.get(*selector_parameter as usize), Some(super::super::ParameterContract {
                    value_type: super::super::ValueType::Bool,
                    ownership: super::super::ParameterOwnership::Value,
                })))
            && actual.contract.parameters.iter().enumerate().all(|(index, contract)| {
                !expected.contains(&(index as u32))
                    || (contract.ownership == super::super::ParameterOwnership::WriteBack
                        && contract.value_type == super::super::ValueType::Ref(*layout))
            })
            && matches!(actual.results.first(), Some(super::super::ValueType::Ref(actual_layout)) if actual_layout == layout)
            && layouts.resolve_nominal(*layout).is_ok();
    }
    let Some(parameter) = projected_exclusive_signature_parameter(parameters, result, conventions) else {
        return false;
    };
    let Some(super::super::ResultContract::AliasesWriteBackParameter {
        parameter: actual_parameter,
        layout,
        projection: Some(_),
    }) =
        actual.contract.results.first()
    else {
        return false;
    };
    usize::try_from(*actual_parameter).ok() == Some(parameter)
        && actual.contract.parameters.get(parameter).is_some_and(|contract| {
            contract.ownership == super::super::ParameterOwnership::WriteBack
                && matches!(contract.value_type, super::super::ValueType::Ref(_))
        })
        && matches!(actual.results.first(), Some(super::super::ValueType::Ref(actual_layout)) if actual_layout == layout)
        && layouts.resolve_nominal(*layout).is_ok()
}

/// Type-level prefilter only. `direct_exclusive_result_relation_is_checked`
/// binds this candidate to the declaration's exact field relation; a function
/// type cannot itself name a record field.
pub(super) fn projected_exclusive_signature_parameter(
    parameters: &[witchy_syntax::ast::Type],
    result: &witchy_syntax::ast::Type,
    conventions: &[witchy_syntax::ast::Convention],
) -> Option<usize> {
    let (lifetime, result_inner) = direct_exclusive_reference_parts(result)?;
    if !is_projected_exclusive_leaf(result_inner) {
        return None;
    }
    let candidates = parameters.iter().enumerate().filter_map(|(index, parameter)| {
        let witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::BorrowMut(candidate_lifetime), owner) = parameter else {
            return None;
        };
        (conventions.get(index).copied().unwrap_or_default() == witchy_syntax::ast::Convention::Var
            && candidate_lifetime == lifetime
            && matches!(owner.unqualified(), witchy_syntax::ast::Type::Named(_, arguments) if arguments.is_empty())
            && !is_direct_exclusive_referent(owner))
            .then_some(index)
    }).collect::<Vec<_>>();
    let [parameter] = candidates.as_slice() else { return None };
    Some(*parameter)
}

/// A source signature that returns a direct shared reference has no safe
/// syntax-only fallback.  Admission must be grounded in both the closed
/// declaration and the pointer-bound access product for that declaration.
pub(super) fn direct_reference_binding_is_checked(
    source_signature: &witchy_syntax::ast::Type,
    declaration: Option<&witchy_syntax::ast::Function>,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    loan_facts_available: bool,
    records: &ScalarRecordFields,
) -> bool {
    let witchy_syntax::ast::Type::Fn(_, checked_result, _, _) = source_signature.unqualified() else {
        return false;
    };
    // Closed callable signatures may erase the reference leaves of an owned
    // aggregate shell.  The declaration is the source contract whose retained
    // access product must authorize those leaves; never let the erased closed
    // signature turn a missing Result/tuple/list relation into a value-only
    // fallback.
    let result = declaration
        .and_then(|declaration| declaration.ret.as_ref())
        .unwrap_or(checked_result);
    let shared_ok = !is_direct_shared_reference(result)
        || declaration.is_some_and(|declaration| {
            direct_shared_reference_relation_is_checked(declaration, access)
        });
    let exclusive_result_ok = !is_direct_exclusive_reference(result)
        || declaration.is_some_and(|declaration| {
            direct_exclusive_result_relation_is_checked(declaration, access, records)
        });
    let aggregate_result_ok = !(is_aggregate_reference_carrier(result)
        || is_direct_rooted_view_aggregate_carrier(result, records))
        || declaration.is_some_and(|declaration| {
            aggregate_reference_result_relation_is_checked(declaration, access, records)
        });
    shared_ok && exclusive_result_ok && aggregate_result_ok && declaration.map_or_else(
        || !source_signature_contains_direct_exclusive_reference(source_signature),
        |declaration| direct_exclusive_reference_relation_is_checked(declaration, access, loan_facts_available),
    )
}

/// Authenticate the bounded aggregate result carrier from the checker-owned
/// relation product.  A tuple has one exact output slot per reference leaf;
/// a homogeneous shared list has one `AnyIndex` slot whose complete owner set
/// is explicit.  No aggregate layout, field name, or matching physical Ref is
/// ever treated as an owner relation.
pub(super) fn aggregate_reference_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    records: &ScalarRecordFields,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return false };
    let Some(access) = access else { return false };
    if is_option_rooted_int_view_carrier(result) {
        return option_rooted_int_view_result_relation_is_checked(declaration, access);
    }
    if is_direct_rooted_view_aggregate_carrier(result, records) {
        // Structural and builtin aggregates retain the complete immediate
        // ownership tree in the checker product. A nominal type carries no
        // expanded field tree here; its exact source identity and each field
        // storage class are instead bound by the opaque-owner family.
        // A builtin or generic shell whose only views sit inside such a
        // nominal (`Option(Inner)`) has no structural view, so the checker
        // tracks no ownership state for it (its output is then necessarily
        // absent; the second conjunct only restates that). The same family
        // binding covers it. A structurally visible view always requires the
        // exact tree below.
        return matches!(result.unqualified(), witchy_syntax::ast::Type::Named(_, arguments)
            if arguments.is_empty())
            || (!witchy_canonical_source::access::type_requires_ownership_state(result)
                && access.result().ownership_output().is_none())
            || witchy_canonical_source::access::ownership_state_class(result).ok().flatten().as_ref()
                == access.result().ownership_output();
    }
    if let witchy_syntax::ast::Type::Named(name, arguments) = result.unqualified()
        && name == "Result"
        && arguments.len() == 2
    {
        let (ok, error) = (&arguments[0], &arguments[1]);
        // The first `Result` alternative is the exact `Ok` tag.  The access
        // catalog represents builtin positional constructor payloads by their
        // constructor field index, so both the tag and inner tuple slot must
        // be present in order.  A missing, swapped, or stale relation cannot
        // authorize this carrier.
        return is_string_source_type(error)
            && aggregate_tuple_reference_result_relation_is_checked(
                declaration,
                access,
                ok,
                &[witchy_canonical_source::access::LoanProjectionStep::Tuple(0)],
            );
    }
    match result.unqualified() {
        witchy_syntax::ast::Type::Tuple(_) => {
            aggregate_tuple_reference_result_relation_is_checked(declaration, access, result, &[])
        }
        witchy_syntax::ast::Type::Named(name, arguments)
            if name == "List" && arguments.len() == 1 =>
        {
            let Some((lifetime, referent, kind)) = aggregate_reference_leaf(&arguments[0]) else {
                return false;
            };
            if kind != witchy_canonical_source::access::BorrowKind::Shared {
                return false;
            }
            matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == kind
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps
                        == [witchy_canonical_source::access::LoanProjectionStep::AnyIndex]
                    && aggregate_relation_owners_match(declaration, relation.owners(), lifetime, referent, kind))
        }
        _ => false,
    }
}

/// `&Int` is an opaque rooted carrier rather than a lifetime-derived alias.
/// Consequently an `Option(&Int)` result has no borrow relation to inspect;
/// its checked access identity must instead retain the exact rooted `Some`
/// child. The owner-family and constructor checks independently authenticate
/// where that child came from, so parameter count never grants authority.
pub(super) fn option_rooted_int_view_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: &witchy_canonical_source::access::AccessSignature,
) -> bool {
    declaration.ret.as_ref().is_some_and(|result| {
        is_option_rooted_int_view_carrier(result)
            && access.result().ownership_output().is_some_and(|class| {
                option_rooted_view_ownership_matches(result, class)
            })
    })
}

/// The checker's ownership tree for an `Option` view carrier mirrors its
/// source nesting exactly: each `Option` shell is one layout-dependent node
/// with a single payload child, and the innermost leaf is the authenticated
/// `&Int` owner. Any other child shape rejects.
pub(super) fn option_rooted_view_ownership_matches(
    source: &witchy_syntax::ast::Type,
    class: &witchy_canonical_source::access::OwnershipStateClass,
) -> bool {
    use witchy_canonical_source::access::OwnershipStateClass;

    if is_direct_rooted_view_int(source) {
        return matches!(class, OwnershipStateClass::RootedViewCarrier);
    }
    let witchy_syntax::ast::Type::Named(name, arguments) = source.unqualified() else { return false };
    let (true, [payload]) = (name == "Option", arguments.as_slice()) else { return false };
    matches!(class, OwnershipStateClass::LayoutDependent { children }
        if matches!(children.as_slice(), [Some(child)] if option_rooted_view_ownership_matches(payload, child)))
}

/// `Option(&Int)`, or an `Option` of such a carrier nested to any depth.
pub(super) fn is_option_rooted_int_view_carrier(source: &witchy_syntax::ast::Type) -> bool {
    matches!(source.unqualified(), witchy_syntax::ast::Type::Named(name, arguments)
        if name == "Option" && matches!(arguments.as_slice(), [payload]
            if is_direct_rooted_view_int(payload) || is_option_rooted_int_view_carrier(payload)))
}

/// Match the complete, ordered leaf-to-owner relation product for one tuple
/// carrier.  `prefix` is empty for a bare tuple and `[Tuple(0)]` for the
/// builtin `Result` `Ok` payload.  It is supplied by the caller rather than
/// inferred from physical layout so a tagged Result cannot be mistaken for a
/// bare aggregate with the same reference fields.
pub(super) fn aggregate_tuple_reference_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: &witchy_canonical_source::access::AccessSignature,
    payload: &witchy_syntax::ast::Type,
    prefix: &[witchy_canonical_source::access::LoanProjectionStep],
) -> bool {
    let witchy_syntax::ast::Type::Tuple(items) = payload.unqualified() else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| is_direct_shared_owned_reference(item) || is_direct_exclusive_reference(item))
        // The access product is itself the complete result authority for this
        // carrier.  Compare its full ordered multiset against the source
        // slots: accepting an `any` match here would let a duplicate relation
        // satisfy two slots, or let an unrelated relation survive.
        && access.borrow_relations().len() == items.len()
        && access.borrow_relations().iter().zip(items).enumerate().all(|(slot, (relation, item))| {
            let Some((lifetime, referent, kind)) = aggregate_reference_leaf(item) else {
                return false;
            };
            let [owner] = relation.owners() else { return false };
            let Some(parameter) = declaration.params.get(owner.position()) else {
                return false;
            };
            let mut expected_projection = prefix.to_vec();
            expected_projection.push(witchy_canonical_source::access::LoanProjectionStep::Tuple(slot));
            relation.lifetime() == lifetime
                && relation.kind() == kind
                && relation.storage_type().unqualified() == referent.unqualified()
                && relation.output_projection().steps == expected_projection
                && owner.input_projection().steps.is_empty()
                && parameter.ty.as_ref().and_then(aggregate_reference_leaf)
                    .is_some_and(|(candidate_lifetime, candidate_referent, candidate_kind)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                            && candidate_kind == kind
                    })
        })
}

/// Relation owner positions are source declarations, not type-shaped aliases.
/// The checker publishes them in declaration order; require a strictly
/// increasing distinct sequence so a duplicated or reordered owner cannot
/// manufacture a list carrier authority.
pub(super) fn aggregate_relation_owners_match(
    declaration: &witchy_syntax::ast::Function,
    owners: &[witchy_canonical_source::access::BorrowOwnerRelation],
    lifetime: &str,
    referent: &witchy_syntax::ast::Type,
    kind: witchy_canonical_source::access::BorrowKind,
) -> bool {
    !owners.is_empty()
        && owners.windows(2).all(|pair| pair[0].position() < pair[1].position())
        && owners.iter().all(|owner| {
            owner.input_projection().steps.is_empty()
                && declaration.params.get(owner.position())
                    .and_then(|parameter| parameter.ty.as_ref())
                    .and_then(aggregate_reference_leaf)
                    .is_some_and(|(candidate_lifetime, candidate_referent, candidate_kind)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                            && candidate_kind == kind
                    })
        })
}

pub(super) fn aggregate_reference_leaf(
    source: &witchy_syntax::ast::Type,
) -> Option<(&str, &witchy_syntax::ast::Type, witchy_canonical_source::access::BorrowKind)> {
    if let Some((lifetime, referent)) = direct_shared_reference_parts(source) {
        return Some((lifetime, referent, witchy_canonical_source::access::BorrowKind::Shared));
    }
    direct_exclusive_reference_parts(source)
        .map(|(lifetime, referent)| (lifetime, referent, witchy_canonical_source::access::BorrowKind::Exclusive))
}

/// A generic shared return may use the ordinary `AliasesParameter` contract
/// only when the source witness retains this exact monomorphized forwarding
/// identity.  Do not reconstruct it from the mangled function name or from a
/// matching physical `Ref` layout: either would let a different generic body
/// borrow the direct carrier's authority.
pub(super) fn exact_generic_shared_relay_binding(
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    source_signature: &witchy_syntax::ast::Type,
    declaration: &witchy_syntax::ast::Function,
    candidate_signature: &super::super::FunctionSignature,
) -> bool {
    use witchy_syntax::ast::{Convention, Expr, Stmt, Type, TypeQual};

    let Some(identity) = source.generic_specialization(&declaration.name) else {
        return false;
    };
    let Some(template) = identity.template() else {
        return false;
    };
    let ([type_parameter], [Some(template_parameter)], Some(template_result), [concrete]) = (
        template.type_parameters(),
        template.parameters(),
        template.result(),
        identity.concrete_types(),
    ) else {
        return false;
    };
    let Some((parameter_lifetime, parameter_inner)) = direct_shared_reference_parts(template_parameter) else {
        return false;
    };
    let Some((result_lifetime, result_inner)) = direct_shared_reference_parts(template_result) else {
        return false;
    };
    if parameter_lifetime != result_lifetime
        || !matches!(parameter_inner.unqualified(), Type::Named(name, arguments)
            if name == type_parameter && arguments.is_empty())
        || !matches!(result_inner.unqualified(), Type::Named(name, arguments)
            if name == type_parameter && arguments.is_empty())
    {
        return false;
    }
    let concrete_reference = Type::Qualified(
        TypeQual::Borrow(parameter_lifetime.to_owned()),
        Box::new(concrete.clone()),
    );
    if !is_direct_shared_owned_reference(&concrete_reference) {
        return false;
    }
    let Type::Fn(parameters, result, conventions, _) = source_signature.unqualified() else {
        return false;
    };
    let convention = conventions.first().copied().unwrap_or(Convention::Let);
    let Some(parameter) = declaration.params.first() else {
        return false;
    };
    let ([super::super::ValueType::Ref(parameter_layout)], [super::super::ValueType::Ref(result_layout)]) = (
        candidate_signature.parameters.as_slice(),
        candidate_signature.results.as_slice(),
    ) else {
        return false;
    };
    let ([parameter_contract], [result_contract]) = (
        candidate_signature.contract.parameters.as_slice(),
        candidate_signature.contract.results.as_slice(),
    ) else {
        return false;
    };
    parameters.as_slice() == [concrete.clone()]
        && result.as_ref() == concrete
        && conventions.len() <= 1
        && convention == Convention::Let
        && declaration.params.len() == 1
        && parameter.convention == Convention::Let
        && parameter.ty.as_ref() == Some(&concrete_reference)
        && declaration.ret.as_ref() == Some(&concrete_reference)
        && matches!(declaration.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(value))] if value == &parameter.name)
        && *parameter_layout == *result_layout
        && parameter_contract.value_type == super::super::ValueType::Ref(*parameter_layout)
        && parameter_contract.ownership == super::super::ParameterOwnership::Value
        && matches!(
            result_contract,
            super::super::ResultContract::AliasesParameter { parameter: 0, layout }
                if *layout == *parameter_layout
        )
        && candidate_signature.contract.parameter_effects
            == BTreeMap::from([(
                0,
                vec![
                    super::super::MemoryEffect::Read,
                    super::super::MemoryEffect::Retain,
                    super::super::MemoryEffect::Release,
                ],
            )])
}

/// Identify only the specialization class whose retained template proves the
/// source `&'l a -> &'l a` relation. The closed source ABI below is physical
/// `[Concrete] -> Concrete`, so it cannot serve as this predicate itself.
pub(super) fn generic_shared_relay_template_candidate(
    source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    name: &str,
) -> bool {
    use witchy_syntax::ast::Type;

    let Some(identity) = source.generic_specialization(name) else {
        return false;
    };
    let Some(template) = identity.template() else {
        return false;
    };
    let ([type_parameter], [Some(template_parameter)], Some(template_result), [concrete]) = (
        template.type_parameters(),
        template.parameters(),
        template.result(),
        identity.concrete_types(),
    ) else {
        return false;
    };
    let Some((parameter_lifetime, parameter_inner)) = direct_shared_reference_parts(template_parameter) else {
        return false;
    };
    let Some((result_lifetime, result_inner)) = direct_shared_reference_parts(template_result) else {
        return false;
    };
    parameter_lifetime == result_lifetime
        && matches!(parameter_inner.unqualified(), Type::Named(parameter, arguments)
            if parameter == type_parameter && arguments.is_empty())
        && matches!(result_inner.unqualified(), Type::Named(parameter, arguments)
            if parameter == type_parameter && arguments.is_empty())
        && is_direct_shared_owned_reference(&Type::Qualified(
            witchy_syntax::ast::TypeQual::Borrow(parameter_lifetime.to_owned()),
            Box::new(concrete.clone()),
        ))
}

pub(super) fn source_signature_contains_direct_exclusive_reference(
    source_signature: &witchy_syntax::ast::Type,
) -> bool {
    let witchy_syntax::ast::Type::Fn(parameters, result, conventions, _) = source_signature.unqualified() else {
        return false;
    };
    is_direct_exclusive_reference(result) || parameters.iter().enumerate().any(|(index, parameter)| {
        matches!(conventions.get(index).copied().unwrap_or_default(),
            witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
            && is_direct_exclusive_reference(parameter)
    })
}

/// Exact checked declaration/access correspondence for the sole exclusive
/// carrier admitted in this phase.  The loan product must also be retained by
/// this exact source; its availability is intentionally not inferred from a
/// lowerer's reconstructed AST traversal.
pub(super) fn direct_exclusive_reference_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    loan_facts_available: bool,
) -> bool {
    let direct = declaration.params.iter().enumerate().filter(|(_, parameter)| {
        matches!(parameter.convention,
            witchy_syntax::ast::Convention::Let | witchy_syntax::ast::Convention::Own)
            && parameter.ty.as_ref().is_some_and(is_direct_exclusive_reference)
    }).collect::<Vec<_>>();
    if direct.is_empty() {
        return true;
    }
    loan_facts_available && access.is_some_and(|access| {
        direct.iter().all(|(index, parameter)| {
            let expected_kind = match parameter.convention {
                witchy_syntax::ast::Convention::Let => witchy_canonical_source::access::AccessKind::ExclusiveWriteback,
                witchy_syntax::ast::Convention::Own => witchy_canonical_source::access::AccessKind::Consuming,
                _ => return false,
            };
            access.params().get(*index).is_some_and(|actual| {
                actual.ty() == parameter.ty.as_ref().expect("filtered above")
                    && actual.kind() == expected_kind
                    && actual.qualifiers().iter().any(|qualifier|
                        matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(_)))
            })
        })
    })
}

/// A direct exclusive result has an exact checked owner set in the
/// pointer-bound access product. This is the authority mint for the direct
/// carrier; matching Ref layouts or a merely present loan graph do not
/// suffice.  A multi-owner relation stays finite and explicit rather than
/// degrading to a shape-derived shared result.
pub(super) fn direct_exclusive_result_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
    records: &ScalarRecordFields,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return true };
    let Some((lifetime, referent)) = direct_exclusive_reference_parts(result) else {
        return true;
    };
    if !is_projected_exclusive_leaf(referent) {
        return false;
    }
    let candidates = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        parameter.ty.as_ref().and_then(direct_exclusive_reference_parts).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime
                && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    if let [owner] = candidates.as_slice() {
        return access.is_some_and(|access| {
        access.result().ty() == result
            && access.result().qualifiers().iter().any(|qualifier| {
                matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
            })
            && matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps.is_empty()
                    && matches!(relation.owners(), [candidate]
                        if candidate.position() == *owner && candidate.input_projection().steps.is_empty()))
        });
    }

    if candidates.len() >= 2 {
        return access.is_some_and(|access| {
            access.result().ty() == result
                && access.result().qualifiers().iter().any(|qualifier| {
                    matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
                })
                && matches!(access.borrow_relations(), [relation]
                    if relation.lifetime() == lifetime
                        && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                        && relation.storage_type().unqualified() == referent.unqualified()
                        && relation.output_projection().steps.is_empty()
                        && relation.owners().iter().map(|owner| owner.position()).eq(candidates.iter().copied())
                        && relation.owners().iter().all(|owner| owner.input_projection().steps.is_empty()))
        });
    }

    let Some(projected) = projected_exclusive_return_source(declaration, records) else { return false };
    access.is_some_and(|access| {
        access.result().ty() == result
            && access.result().qualifiers().iter().any(|qualifier| {
                matches!(qualifier, witchy_canonical_source::access::AccessQualifier::BorrowMut(candidate) if candidate == lifetime)
            })
            && matches!(access.borrow_relations(), [relation]
                if relation.lifetime() == lifetime
                    && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                    && relation.storage_type().unqualified() == referent.unqualified()
                    && relation.output_projection().steps.is_empty()
                    && matches!(relation.owners(), [candidate]
                        if candidate.position() == projected.parameter
                            && candidate.input_projection().steps.is_empty()))
    })
}

/// Consult the type-checker-owned access product before accepting the one
/// source-level alias contract the direct carrier can express.  This is not a
/// lowering-time reconstruction: the product is pointer-bound to the exact
/// typed module and was already used to reject stale or missing facts.
pub(super) fn direct_shared_reference_relation_is_checked(
    declaration: &witchy_syntax::ast::Function,
    access: Option<&witchy_canonical_source::access::AccessSignature>,
) -> bool {
    let Some(result) = declaration.ret.as_ref() else { return true };
    let Some((lifetime, referent)) = direct_shared_reference_parts(result) else {
        // A shared-reference argument which is not returned does not create an
        // output alias. Its ABI contract is checked by the ordinary parameter
        // correspondence below; no checked borrow relation is required here.
        return true;
    };
    let candidates = declaration.params.iter().enumerate().filter_map(|(index, parameter)| {
        parameter.ty.as_ref().and_then(direct_shared_reference_parts).and_then(|(candidate_lifetime, candidate_referent)| {
            (candidate_lifetime == lifetime && candidate_referent.unqualified() == referent.unqualified())
                .then_some(index)
        })
    }).collect::<Vec<_>>();
    let [owner] = candidates.as_slice() else { return false };
    access.is_some_and(|access| access.borrow_relations().iter().any(|relation| {
        relation.lifetime() == lifetime
            && relation.kind() == witchy_canonical_source::access::BorrowKind::Shared
            && relation.storage_type().unqualified() == referent.unqualified()
            && relation.output_projection().steps.is_empty()
            && matches!(relation.owners(), [candidate]
                if candidate.position() == *owner && candidate.input_projection().steps.is_empty())
    }))
}

pub(super) fn ordinary_own_source_type_supported(source: &witchy_syntax::ast::Type) -> bool {
    let source = match source {
        // The outer qualifier is a checked source ownership fact.  Its inner
        // physical lane is still the ordinary own ABI.  Do not recurse
        // through nested qualifiers: an element-level ownership token is not
        // part of this scalar exception.
        witchy_syntax::ast::Type::Qualified(witchy_syntax::ast::TypeQual::Unique, inner) => inner.as_ref(),
        _ => source,
    };
    ordinary_unqualified_own_source_type_supported(source)
}

pub(super) fn ordinary_unqualified_own_source_type_supported(source: &witchy_syntax::ast::Type) -> bool {
    if is_unit_source_type(source) {
        return true;
    }
    match source {
        // This admits the convention, not a signature or pointer claim:
        // value_type_matches_source independently checks the closure record
        // and its complete registered callable signature.
        witchy_syntax::ast::Type::Fn(..) => true,
        // A capability token is a scalar-like value: copied on pass, never
        // released, so every convention carries it. `Dir` names its rights in
        // type arguments, which change no ABI fact about the token.
        witchy_syntax::ast::Type::Named(name, _) if matches!(name.as_str(), "Dir" | "File" | "Net") => true,
        witchy_syntax::ast::Type::Named(name, arguments) if arguments.is_empty() => {
            matches!(name.as_str(), "Int" | "Float" | "Duration" | "Bool" | "Nil" | "Unit" | "String" | "Bytes" | "Console" | "Env" | "Listener" | "Socket")
        }
        witchy_syntax::ast::Type::Named(name, arguments) if name == "List" && arguments.len() == 1 => {
            ordinary_unqualified_own_source_type_supported(&arguments[0])
        }
        _ => false,
    }
}

/// The canonical `()` spelling is structurally an empty tuple.  Keep legacy
/// names accepted at this compatibility boundary, but never require them for
/// a zero-lane result or parameter correspondence.
pub(super) fn is_unit_source_type(source: &witchy_syntax::ast::Type) -> bool {
    match source.unqualified() {
        witchy_syntax::ast::Type::Named(name, arguments) => {
            (name == "Nil" || name == "Unit") && arguments.is_empty()
        }
        witchy_syntax::ast::Type::Tuple(items) => items.is_empty(),
        _ => false,
    }
}
