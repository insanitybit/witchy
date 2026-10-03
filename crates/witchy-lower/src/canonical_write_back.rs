//! Call-local places: capture source coordinates once, then rebuild the current
//! root with the returned value. No saved parent value can overwrite a sibling
//! result or a disjoint update performed while evaluating a later argument.
use super::*;

pub(super) struct PreparedWriteBack {
    pub(super) root: String,
    projections: Vec<PreparedProjection>,
    /// A checked two-root returned handle. The selector and both source root
    /// names are retained from the exact call/access facts; `root` is only
    /// the first candidate for legacy diagnostics and never authorizes a
    /// dynamic commit by itself.
    disjunctive: Option<DisjunctiveWriteBackRoots>,
    /// A local exclusive handle is a checked view of the storage being
    /// replaced, rather than an independently owned place. Its call
    /// write-back must therefore refresh the handle from that replacement
    /// before the replacement is moved into its owning root.
    /// This is deliberately unavailable to ordinary write-backs.
    reference_alias: Option<String>,
    pub(super) detached_owned_field: bool,
}

#[derive(Debug, Clone)]
struct DisjunctiveWriteBackRoots {
    selector: ValueId,
    first: String,
    second: String,
}

enum PreparedProjection {
    Field { source: Type, index: usize },
    Index { source: Type, index: ValueId, site: abort::Site },
    Dictionary { source: Type, key: ValueId },
}

fn pattern_binds_name(pattern: &Pattern, name: &str) -> bool {
    match pattern {
        Pattern::Var(bound) => bound == name,
        Pattern::Tuple(items) | Pattern::Ctor { args: items, .. } | Pattern::AnonCtor { args: items, .. }
        | Pattern::Or(items) => items.iter().any(|item| pattern_binds_name(item, name)),
        Pattern::List { elems, rest } => {
            elems.iter().any(|item| pattern_binds_name(item, name))
                || rest.as_ref().and_then(|bound| bound.as_deref()) == Some(name)
        }
        Pattern::Wildcard | Pattern::Int(_) | Pattern::Str(_) | Pattern::Bool(_)
        | Pattern::Duration(_) | Pattern::IntRange { .. } => false,
    }
}

enum AssignmentProjection<'a> {
    Field { base: &'a Expr, field: &'a str },
    Index { base: &'a Expr, index: &'a Expr, expression: &'a Expr },
}

/// Match only the read spine corresponding to the parser's outward rebuild.
/// An ordinary set-at value expression must retain ordinary argument semantics.
fn assignment_read_matches(expression: &Expr, root: &str, path: &[AssignmentProjection<'_>]) -> bool {
    let Some((last, prefix)) = path.split_last() else {
        return matches!(expression, Expr::Var(name) if name == root);
    };
    match (last, expression) {
        (AssignmentProjection::Field { field: expected, .. }, Expr::Field { base, field }) => {
            field == expected && assignment_read_matches(base, root, prefix)
        }
        (AssignmentProjection::Index { index: expected, .. }, Expr::Call { name, args }) => {
            matches!(name.as_str(), witchy_syntax::intrinsics::LIST_AT | witchy_syntax::intrinsics::DICT_AT) && args.len() == 2
                && args[1] == **expected && assignment_read_matches(&args[0], root, prefix)
        }
        (AssignmentProjection::Index { index: expected, .. }, Expr::Index { base, index }) => {
            index.as_ref() == *expected && assignment_read_matches(base, root, prefix)
        }
        _ => false,
    }
}

/// The declaration access signature supplies only the root write-back ABI:
/// the selected argument owns the returned `String` under the same exclusive
/// lifetime. It deliberately contains no field projection. The callee's
/// source-derived result contract and the caller's live loan/place event
/// authenticate the fixed field independently, so neither source can erase
/// the other into a root alias.
pub(super) fn exact_projected_result_root_relation(
    access: &witchy_canonical_source::access::AccessSignature,
    parameter: usize,
) -> bool {
    let Some((result_lifetime, result_referent)) = exclusive_reference_parts(access.result().ty()) else {
        return false;
    };
    if !is_projected_exclusive_leaf(result_referent) {
        return false;
    }
    let Some(contract) = access.params().get(parameter) else {
        return false;
    };
    if contract.kind() != witchy_canonical_source::access::AccessKind::ExclusiveWriteback
        || !exclusive_reference_parts(contract.ty()).is_some_and(|(parameter_lifetime, owner)| {
            parameter_lifetime == result_lifetime
                && matches!(owner.unqualified(), Type::Named(_, arguments) if arguments.is_empty())
        })
    {
        return false;
    }
    matches!(access.borrow_relations(), [relation]
        if matches!(relation.owners(), [candidate]
            if exact_projected_result_root_relation_shape(
                result_lifetime,
                result_referent,
                parameter,
                relation.lifetime(),
                relation.kind(),
                relation.storage_type(),
                relation.output_projection(),
                candidate.position(),
                candidate.input_projection(),
            )))
}

pub(super) fn exact_projected_result_root_relation_shape(
    result_lifetime: &str,
    result_referent: &Type,
    parameter: usize,
    relation_lifetime: &str,
    relation_kind: witchy_canonical_source::access::BorrowKind,
    relation_storage: &Type,
    output_projection: &witchy_canonical_source::access::LoanProjection,
    owner_position: usize,
    input_projection: &witchy_canonical_source::access::LoanProjection,
) -> bool {
    relation_lifetime == result_lifetime
        && relation_kind == witchy_canonical_source::access::BorrowKind::Exclusive
        && relation_storage.unqualified() == result_referent.unqualified()
        && owner_position == parameter
        && input_projection.steps.is_empty()
        && output_projection.steps.is_empty()
}

/// The signature relation selects the root write-back lane; this canonical
/// result contract selects the one concrete child. Both identifiers are
/// required before reopening a returned projected handle.
pub(super) fn projected_return_contract_matches_root_relation(
    contract: Option<&ResultContract>,
    relation_parameter: usize,
    field: u32,
) -> bool {
    projected_return_contract_field(contract, relation_parameter) == Some(field)
}

fn projected_return_contract_field(
    contract: Option<&ResultContract>,
    relation_parameter: usize,
) -> Option<u32> {
    match contract {
        Some(ResultContract::AliasesWriteBackParameter {
            parameter,
            projection: Some(contract_field),
            ..
        }) if usize::try_from(*parameter).ok() == Some(relation_parameter) => Some(*contract_field),
        _ => None,
    }
}

pub(super) fn exact_write_back_root_layout(
    contract: Option<&ParameterContract>,
    signature_parameter: Option<&ValueType>,
    root: Option<&ValueType>,
) -> bool {
    matches!(contract,
        Some(ParameterContract {
            ownership: ParameterOwnership::WriteBack,
            value_type,
        }) if Some(value_type) == signature_parameter && signature_parameter == root)
}

pub(super) fn exact_unpacked_string_record_field_name(
    source: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    field: u32,
) -> Option<String> {
    let Type::Named(record, arguments) = source.unqualified() else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let definition = function_record_definition(record, definitions)?;
    if definition.packed || definition.variants.len() != 1 {
        return None;
    }
    let variant = &definition.variants[0];
    let index = usize::try_from(field).ok()?;
    variant
        .field_names
        .get(index)
        .zip(variant.fields.get(index))
        .filter(|(_, ty)| is_direct_exclusive_owned_object(ty))
        .map(|(name, _)| name.clone())
}

fn assignment_plan<'a>(root: &str, expression: &'a Expr, depth: u32) -> Option<(Vec<AssignmentProjection<'a>>, &'a Expr)> {
    let mut path = Vec::new();
    let mut replacement = expression;
    for _ in 0..depth {
        match replacement {
            Expr::Call { name, args }
                if matches!(name.as_str(), witchy_syntax::intrinsics::LIST_SET_AT | witchy_syntax::intrinsics::DICT_INSERT) && args.len() == 3
                    && assignment_read_matches(&args[0], root, &path) =>
            {
                path.push(AssignmentProjection::Index { base: &args[0], index: &args[1], expression: replacement });
                replacement = &args[2];
            }
            Expr::RecordUpdate { name: None, base, fields }
                if fields.len() == 1 && assignment_read_matches(base, root, &path) =>
            {
                path.push(AssignmentProjection::Field { base, field: &fields[0].0 });
                replacement = &fields[0].1;
            }
            _ => return None,
        }
    }
    (!path.is_empty()).then_some((path, replacement))
}

pub(super) fn source_place_root(expression: &Expr) -> Option<&str> {
    match expression {
        Expr::Var(name) => Some(name),
        Expr::Field { base, .. } | Expr::Index { base, .. } => source_place_root(base),
        Expr::Call { name, args }
            if matches!(name.as_str(), witchy_syntax::intrinsics::LIST_AT | witchy_syntax::intrinsics::DICT_AT) && args.len() == 2 =>
        {
            source_place_root(&args[0])
        }
        _ => None,
    }
}

fn statement_expression(statement: &Stmt) -> Option<&Expr> {
    match statement {
        Stmt::Let { value, .. }
        | Stmt::LetPattern { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::Yield(value)
        | Stmt::Expr(value)
        | Stmt::Return(Some(value)) => Some(value),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => None,
    }
}

impl FunctionLowerer<'_> {
    /// Reopen the sole `Ok` payload of a checked `Result(&mut String, E)`
    /// call.  The Result shell is not an alias authority: the declaration's
    /// exact access relation, the checked call opening, and the checked `Ok`
    /// pattern must all name the same exclusive owner lane before the payload
    /// can write that owner back.
    ///
    /// This is intentionally narrower than aggregate transport.  In
    /// particular, neither another tagged type with an `Ok` spelling nor an
    /// `Option`-like physical layout can enter here: the source result must be
    /// the built-in two-argument `Result`, its selected variant must be its
    /// canonical tag-zero `Ok` arm, and the retained relation must be the
    /// direct result payload projection opened by that exact call.
    pub(super) fn prepare_authenticated_result_ok_exclusive_write_back(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let Some((use_index, match_statement, scrutinee, arms)) = self.source_body.stmts.iter()
            .enumerate()
            .find_map(|(index, statement)| {
                let Stmt::Expr(Expr::Match { scrutinee, arms }) = statement else {
                    return None;
                };
                let contains_use = arms.iter().any(|arm| {
                    matches!(&arm.body,
                        Expr::Block(block) if block.stmts.iter().any(|statement| std::ptr::eq(statement, use_statement))
                    )
                });
                contains_use.then_some((index, statement, scrutinee.as_ref(), arms.as_slice()))
            })
        else {
            return Ok(None);
        };
        let Expr::Var(selected) = scrutinee else {
            return Ok(None);
        };
        let selected_type = self.expression_type(scrutinee)?;
        let Type::Named(result_name, result_arguments) = selected_type.unqualified() else {
            return Ok(None);
        };
        if result_name != "Result" || result_arguments.len() != 2 {
            return Ok(None);
        }
        let Some(shape) = scalar_aggregate_shape(&selected_type, self.definitions) else {
            return Ok(None);
        };
        let Some((ok_tag, ok_variant)) = shape.variant("Ok") else {
            return Ok(None);
        };
        // `Result` is the sealed two-tag carrier used by the checker.  Keep
        // the tag and payload slot in the proof rather than treating a
        // matching constructor spelling as authority.
        if !shape.tagged || ok_tag != 0 || ok_variant.first_field != 1 || ok_variant.arity != 1 {
            return Ok(None);
        }
        let ok_arms = arms.iter().filter(|arm| {
            matches!(&arm.pattern,
                Pattern::Ctor { name, args }
                    if name == "Ok" && matches!(args.as_slice(), [Pattern::Var(bound)] if bound == view)
            ) && matches!(&arm.body,
                Expr::Block(block) if block.stmts.iter().any(|statement| std::ptr::eq(statement, use_statement))
            )
        }).collect::<Vec<_>>();
        let [_] = ok_arms.as_slice() else { return Ok(None) };
        let Some((call_index, call_statement, callee, arguments, call)) = self.source_body.stmts[..use_index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, statement)| {
                let Stmt::Let { name, value: call @ Expr::Call { name: callee, args: arguments }, .. } = statement else {
                    return None;
                };
                (name == selected).then_some((index, statement, callee.as_str(), arguments.as_slice(), call))
            })
        else {
            return Ok(None);
        };
        // A same-spelled top-level binding between this exact opening and the
        // enclosing match would make the source link ambiguous.  Pattern-local
        // `view` is selected above; no spelling recovery is used for the
        // Result shell or owner.
        if self.source_body.stmts[call_index + 1..use_index].iter().any(|statement| {
            matches!(statement, Stmt::Let { name, .. } if name == selected)
                || matches!(statement, Stmt::LetPattern { pattern, .. }
                    if pattern_binds_name(pattern, selected))
        }) {
            return Ok(None);
        }
        let Some(call_access) = self.access_facts.call_at(self.checked_module, call) else {
            return Ok(None);
        };
        let Some(declaration) = self.access_facts.declaration(callee) else {
            return Ok(None);
        };
        if call_access.verify_exact(declaration).is_err() {
            return Ok(None);
        }
        let Type::Named(access_result_name, access_result_arguments) = call_access.result().ty().unqualified() else {
            return Ok(None);
        };
        let [payload, _error] = access_result_arguments.as_slice() else { return Ok(None) };
        if access_result_name != "Result" {
            return Ok(None);
        }
        let Some((lifetime, referent)) = exclusive_reference_parts(payload) else {
            return Ok(None);
        };
        if !is_direct_exclusive_owned_object(referent) {
            return Ok(None);
        }
        let relations = call_access.borrow_relations();
        let [relation] = relations else { return Ok(None) };
        let [owner] = relation.owners() else { return Ok(None) };
        let owner_parameter = owner.position();
        if relation.lifetime() != lifetime
            || relation.kind() != witchy_canonical_source::access::BorrowKind::Exclusive
            || relation.storage_type().unqualified() != referent.unqualified()
            || !owner.input_projection().steps.is_empty()
            || !call_access.params().get(owner_parameter).is_some_and(|parameter| {
                parameter.kind() == witchy_canonical_source::access::AccessKind::ExclusiveWriteback
                    && exclusive_reference_parts(parameter.ty()).is_some_and(|(candidate_lifetime, candidate_referent)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                    })
            })
        {
            return Ok(None);
        }
        let Some(Expr::Unary { op: UnOp::BorrowMut, expr: borrowed }) = arguments.get(owner_parameter) else {
            return Ok(None);
        };
        let Some(place) = self.access_facts.place_at(self.checked_module, borrowed.as_ref()) else {
            return Ok(None);
        };
        let Expr::Var(owner_name) = borrowed.as_ref() else { return Ok(None) };
        if place.root() != *owner_name
            || !place.steps().is_empty()
            || self.expression_type(borrowed.as_ref())? != referent.unqualified().clone()
            || !self.mutable_bindings.contains(owner_name)
            || !self.bindings.contains_key(owner_name)
        {
            return Ok(None);
        }
        let openings = loan_facts.opens_after(call_statement).iter().filter(|event| {
            event.view == *selected
                && event.owner == *owner_name
                && event.projection.steps.is_empty()
                && event.borrower_projection == *relation.output_projection()
                && event.origin == callee
                && event.kind == witchy_types::loans::BorrowKind::Exclusive
                && event.owner_type.unqualified() == referent.unqualified()
                && event.owner_root().local == *owner_name
                && event.owner_root().direct_storage_type.as_ref() == Some(&referent.unqualified().clone())
        }).collect::<Vec<_>>();
        let [opening] = openings.as_slice() else { return Ok(None) };
        if !loan_facts.active_at(match_statement).contains(opening) {
            return Ok(None);
        }
        Ok(Some(PreparedWriteBack {
            root: owner_name.clone(),
            projections: Vec::new(),
            disjunctive: None,
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// Reopen one selected exclusive leaf from the bounded returned tuple
    /// carrier.  This is deliberately a provenance chain, rather than a
    /// physical aggregate inspection: the exact source call relation selects
    /// the tuple slot and owner argument, then the checked affine move and
    /// destructuring events carry that same owner to the local handle.
    ///
    /// No list, nested pattern, projection, callable, or inferred alias is
    /// accepted here.  Each omitted fact makes the carrier unavailable.
    pub(super) fn prepare_authenticated_aggregate_tuple_exclusive_write_back(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let Some(use_index) = self.source_body.stmts.iter().position(|statement| {
            std::ptr::eq(statement, use_statement)
        }) else {
            return Ok(None);
        };
        let events = loan_facts.active_at(use_statement).iter().filter(|event| {
            event.view == view && event.kind == witchy_types::loans::BorrowKind::Exclusive
        }).collect::<Vec<_>>();
        let [event] = events.as_slice() else { return Ok(None) };
        if !event.projection.steps.is_empty()
            || !event.borrower_projection.steps.is_empty()
            || !is_direct_exclusive_owned_object(&event.owner_type)
            || event.owner_root().local != event.owner
            || event.owner_root().direct_storage_type.as_ref() != Some(&event.owner_type)
            || !self.mutable_bindings.contains(&event.owner)
            || !self.bindings.contains_key(&event.owner)
        {
            return Ok(None);
        }

        let Some((destructure_index, slot, moved, destructure)) = self.source_body.stmts[..use_index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, statement)| {
                let Stmt::LetPattern { pattern: Pattern::Tuple(items), value: Expr::Var(moved) } = statement else {
                    return None;
                };
                let slots = items.iter().enumerate().filter_map(|(slot, item)| {
                    matches!(item, Pattern::Var(name) if name == view).then_some(slot)
                }).collect::<Vec<_>>();
                let [slot] = slots.as_slice() else { return None };
                Some((index, *slot, moved.as_str(), statement))
            })
        else {
            return Ok(None);
        };
        // A later source binding of the same spelling would make this lexical
        // reconstruction ambiguous.  Never use the current ValueId as a
        // fallback for that ambiguity.
        if self.source_body.stmts[destructure_index + 1..use_index].iter().any(|statement| {
            matches!(statement, Stmt::Let { name, .. } if name == view)
                || matches!(statement, Stmt::LetPattern { pattern, .. }
                    if pattern_binds_name(pattern, view))
        }) {
            return Ok(None);
        }
        let Some((move_index, values, move_statement)) = self.source_body.stmts[..destructure_index]
            .iter()
            .enumerate()
            .rev().find_map(|(index, statement)| {
                let Stmt::Let { name, value: Expr::Var(values), .. } = statement else {
                    return None;
                };
                (name == moved).then_some((index, values.as_str(), statement))
            })
        else {
            return Ok(None);
        };
        let Some((_, callee, arguments, call_statement, call)) = self.source_body.stmts[..move_index]
            .iter()
            .enumerate()
            .rev().find_map(|(index, statement)| {
                let Stmt::Let { name, value: call @ Expr::Call { name: callee, args: arguments }, .. } = statement else {
                    return None;
                };
                (name == values).then_some((index, callee.as_str(), arguments.as_slice(), statement, call))
            })
        else {
            return Ok(None);
        };
        let Some(call_access) = self.access_facts.call_at(self.checked_module, call) else {
            return Ok(None);
        };
        let Some(declaration) = self.access_facts.declaration(callee) else {
            return Ok(None);
        };
        if call_access.verify_exact(declaration).is_err() || event.origin != callee {
            return Ok(None);
        }
        let Type::Tuple(result_items) = call_access.result().ty().unqualified() else {
            return Ok(None);
        };
        let Some(result_leaf) = result_items.get(slot) else { return Ok(None) };
        let Some((lifetime, referent)) = exclusive_reference_parts(result_leaf) else {
            return Ok(None);
        };
        if !is_direct_exclusive_owned_object(referent)
            || call_access.borrow_relations().len() != result_items.len()
        {
            return Ok(None);
        }
        let Some(relation) = call_access.borrow_relations().get(slot) else { return Ok(None) };
        let [owner] = relation.owners() else { return Ok(None) };
        let owner_parameter = owner.position();
        if relation.lifetime() != lifetime
            || relation.kind() != witchy_canonical_source::access::BorrowKind::Exclusive
            || relation.storage_type().unqualified() != referent.unqualified()
            || relation.output_projection().steps
                != [witchy_canonical_source::access::LoanProjectionStep::Tuple(slot)]
            || !owner.input_projection().steps.is_empty()
            || !call_access.params().get(owner_parameter).is_some_and(|parameter| {
                parameter.kind() == witchy_canonical_source::access::AccessKind::ExclusiveWriteback
                    && exclusive_reference_parts(parameter.ty()).is_some_and(|(candidate_lifetime, candidate_referent)| {
                        candidate_lifetime == lifetime
                            && candidate_referent.unqualified() == referent.unqualified()
                    })
            })
        {
            return Ok(None);
        }
        let Some(Expr::Unary { op: UnOp::BorrowMut, expr: borrowed }) = arguments.get(owner_parameter) else {
            return Ok(None);
        };
        let Some(place) = self.access_facts.place_at(self.checked_module, borrowed.as_ref()) else {
            return Ok(None);
        };
        if !matches!(borrowed.as_ref(), Expr::Var(owner) if owner == &event.owner)
            || place.root() != event.owner
            || !place.steps().is_empty()
            || self.expression_type(borrowed.as_ref())? != event.owner_type
        {
            return Ok(None);
        }
        let call_open = loan_facts.opens_after(call_statement).iter().any(|opened| {
            opened.view == values
                && opened.owner == event.owner
                && opened.projection == event.projection
                && opened.borrower_projection.steps
                    == [witchy_canonical_source::access::LoanProjectionStep::Tuple(slot)]
                && opened.kind == event.kind
                && opened.owner_type == event.owner_type
                && opened.origin == event.origin
        });
        let move_open = loan_facts.opens_after(move_statement).iter().any(|opened| {
            opened.view == moved
                && opened.owner == event.owner
                && opened.projection == event.projection
                && opened.borrower_projection.steps
                    == [witchy_canonical_source::access::LoanProjectionStep::Tuple(slot)]
                && opened.kind == event.kind
                && opened.owner_type == event.owner_type
                && opened.origin == event.origin
        });
        if !call_open || !move_open || !loan_facts.opens_after(destructure).contains(event) {
            return Ok(None);
        }
        Ok(Some(PreparedWriteBack {
            root: event.owner.clone(),
            projections: Vec::new(),
            disjunctive: None,
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// Authenticate the exact checked direct-call/argument pair before
    /// reconstructing its write-back place. The direct ABI remains limited to
    /// a direct `&mut String`/`Int` parameter, with a checked fixed caller
    /// projection. A named mutable handle is either an unshadowed
    /// declaration-authenticated entry parameter, a direct-root reborrow, or
    /// the one retained-fact-authenticated local fixed-field handle. Returned,
    /// aggregate, callable, and generic carrier transport remains refused.
    pub(super) fn prepare_authenticated_direct_exclusive_write_back(
        &mut self,
        callee: &str,
        call_site: Option<&Expr>,
        parameter: usize,
        argument: &Expr,
        callable_reference: Option<&CallableReferenceCarrier>,
    ) -> Result<PreparedWriteBack, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let call_site = call_site.ok_or(CanonicalLoweringError::UnsupportedExpression(
            "exclusive write-back requires an authenticated direct call",
        ))?;
        let access = self
            .access_facts
            .call_at(self.checked_module, call_site)
            .ok_or(CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back call has no checked access contract",
            ))?;
        let contract = access.params().get(parameter).ok_or(
            CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back parameter lacks a checked access contract",
            ),
        )?;
        let declaration_matches = self.access_facts.declaration(callee)
            .is_some_and(|declaration| access.verify_exact(declaration).is_ok());
        // An indirect callable has no declaration keyed by the local callee
        // name. Its sole alternative authority is the exact exclusive carrier
        // already authenticated at this call site. In particular, neither a
        // matching closure layout nor a function-shaped access signature can
        // replace the opaque callable identity.
        let callable_matches = matches!(callable_reference,
            Some(expected @ CallableReferenceCarrier::ExclusiveWriteBack(identity))
                if access.identity_key() == *identity
                    && callable_reference_carrier(access).as_ref() == Some(expected));
        if !declaration_matches && !callable_matches {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back call has no matching checked authority",
            ));
        }
        // A direct scalar owner remains the ordinary ABI.  The one additional
        // carrier is a checked call whose *result* is that direct scalar
        // handle and whose sole exact relation selects one fixed field of this
        // argument.  Do not treat a named aggregate parameter as direct just
        // because its declaration says `&mut`: the relation is the authority.
        let projected_result_parameter = exact_projected_result_root_relation(access, parameter);
        if !matches!(contract.kind(),
            witchy_canonical_source::access::AccessKind::ExclusiveWriteback
                | witchy_canonical_source::access::AccessKind::Consuming)
            || !(is_direct_exclusive_reference(contract.ty()) || projected_result_parameter)
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back contract is outside the direct ABI",
            ));
        }
        if !matches!(call_site, Expr::Call { name, .. } if name == callee) {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back call contract does not match its callee",
            ));
        }

        match argument {
            Expr::Unary { op: UnOp::BorrowMut, expr } => {
                let place = self
                    .access_facts
                    .place_at(self.checked_module, expr)
                    .ok_or(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive write-back place has no checked identity",
                    ))?;
                if place.has_dynamic_index() {
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive write-back place has a dynamic projection",
                    ));
                }
                if !self.mutable_bindings.contains(place.root())
                    || !self.bindings.contains_key(place.root())
                {
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive write-back place is not a live mutable owner",
                    ));
                }
                // The place fact is pointer-keyed to this exact expression;
                // preparation is allowed only after that lookup.  It captures
                // each fixed projection while retaining the root as the only
                // owner token, so it cannot turn a field into an owner base.
                self.prepare_write_back(expr)
            }
            Expr::Var(view) => {
                let place = self
                    .access_facts
                    .place_at(self.checked_module, argument)
                    .ok_or(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive reborrow handle has no checked place identity",
                    ))?;
                if place.root() != view || !place.steps().is_empty() {
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive reborrow handle is not a direct checked loan",
                    ));
                }
                let entry_parameter = self.entry_exclusive_reference_params.contains(view)
                    && !self.binding_scopes.iter().any(|scope| scope.contains_key(view));
                if !entry_parameter {
                    let statement = self.source_statement_containing(call_site).ok_or(
                        CanonicalLoweringError::UnsupportedExpression(
                            "exclusive reborrow has no checked source statement",
                        ),
                    )?;
                    let event = loan_facts.active_at(statement).iter().find(|event| {
                        event.view == *view
                            && event.kind == witchy_types::loans::BorrowKind::Exclusive
                    }).ok_or(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive reborrow has no active checked loan",
                    ))?;
                    let owner_root = event.owner_root();
                    // This is the deliberately small local-handle carrier:
                    // `var owner; let/var view = &mut owner; direct(view)`.
                    // Mutability of the handle binding does not supply any
                    // authority: the exact call/place facts authenticate the
                    // caller spelling and the active event supplies the
                    // distinct owning local. A returned/reborrowed handle,
                    // aggregate projection, stale fact, or lexical shadow
                    // cannot satisfy all of these independent checked
                    // identities.
                    let direct_local_open = self.source_body.stmts.iter().any(|opening| {
                        matches!(opening,
                            Stmt::Let { name, mutable: _,
                                value: Expr::Unary { op: UnOp::BorrowMut, expr }, .. }
                            if name == view
                                && matches!(expr.as_ref(), Expr::Var(owner) if owner == &event.owner)
                        ) && loan_facts.opens_after(opening).iter().any(|opened| opened == event)
                    });
                    if direct_local_open
                        && event.owner == owner_root.local
                        && event.projection.steps.is_empty()
                        && event.borrower_projection.steps.is_empty()
                        && is_direct_exclusive_owned_object(&event.owner_type)
                        && owner_root.direct_storage_type.as_ref() == Some(&event.owner_type)
                        && self.mutable_bindings.contains(&event.owner)
                        && self.bindings.contains_key(&event.owner)
                    {
                        return Ok(PreparedWriteBack {
                            root: event.owner.clone(),
                            projections: Vec::new(),
                            disjunctive: None,
                            reference_alias: Some(view.clone()),
                            detached_owned_field: false,
                        });
                    }
                    if let Some(place) = self.prepare_authenticated_returned_disjunctive_exclusive_handle(
                        view,
                        statement,
                    )? {
                        return Ok(place);
                    }
                    if let Some(place) = self.prepare_authenticated_returned_direct_exclusive_handle(
                        view,
                        statement,
                    )? {
                        return Ok(place);
                    }
                    if let Some(place) = self.prepare_authenticated_returned_projected_exclusive_handle(
                        view,
                        statement,
                    )? {
                        return Ok(place);
                    }
                    if let Some(place) = self.prepare_authenticated_projected_local_exclusive_handle(
                        view,
                        statement,
                    )? {
                        return Ok(place);
                    }
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "exclusive local reborrow lacks an exact checked owner relation",
                    ));
                }
                self.prepare_write_back(argument)
            }
            _ => Err(CanonicalLoweringError::UnsupportedExpression(
                "exclusive write-back requires a checked mutable place or reborrow",
            )),
        }
    }

    /// Reopen the one deliberately bounded dynamic root carrier.  Every part
    /// is independently authenticated: the exact returned SSA value records
    /// the selector/candidate contract, the opening call retains the source
    /// expression identities, and the checked access relation names precisely
    /// those two write-back owners.  A Bool with the same physical type or a
    /// root outside this checked pair cannot reach the conditional rebind.
    fn prepare_authenticated_returned_disjunctive_exclusive_handle(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let Some(carrier_value) = self.bindings.get(view).copied() else { return Ok(None) };
        let Some(carrier) = self.disjunctive_exclusive_carriers.get(&carrier_value) else {
            return Ok(None);
        };
        // A disjunctive checked loan has one event per candidate in some
        // frontend products, unlike the single-root carrier.  Retain the
        // nearest exact lexical opening rather than demanding a fabricated
        // singular owner event; the call access relation below still fixes
        // both roots and their order.
        let opening = self.source_body.stmts.iter()
            .take_while(|statement| !std::ptr::eq(*statement, use_statement))
            .filter(|statement| matches!(statement,
                Stmt::Let { name, mutable: false, value: Expr::Call { .. }, .. } if name == view
            ))
            .last();
        let Some(opening) = opening else { return Ok(None) };
        if !loan_facts.active_at(use_statement).iter().any(|event| {
            event.view == view
                && event.kind == witchy_types::loans::BorrowKind::Exclusive
                && event.projection.steps.is_empty()
                && event.borrower_projection.steps.is_empty()
                && is_direct_exclusive_owned_object(&event.owner_type)
        }) {
            return Ok(None);
        }
        let Stmt::Let { value: call @ Expr::Call { name: callee, args: arguments }, .. } = opening else {
            return Ok(None);
        };
        let Some((_, signature_id)) = self.functions.get(callee).copied() else { return Ok(None) };
        let Some(signature) = self.signatures.get(signature_id.get() as usize) else { return Ok(None) };
        let Some(ResultContract::AliasesOneOfWriteBackParameters { parameters, .. }) = signature.contract.results.first() else {
            return Ok(None);
        };
        let Some(ResultContract::DisjunctiveWriteBackSelector {
            carrier_result,
            parameters: selector_parameters,
            selector_parameter: _,
        }) = signature.contract.results.get(1) else { return Ok(None) };
        if *carrier_result != 0
            || parameters.as_slice() != carrier.candidates
            || selector_parameters.as_slice() != carrier.candidates
            || self.values.get(&carrier.selector) != Some(&ValueType::Bool)
        {
            return Ok(None);
        }
        let Some(call_access) = self.access_facts.call_at(self.checked_module, call) else {
            return Ok(None);
        };
        let Some(declaration) = self.access_facts.declaration(callee) else { return Ok(None) };
        if call_access.verify_exact(declaration).is_err()
            || !matches!(call_access.borrow_relations(), [relation]
                if relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                    && relation.output_projection().steps.is_empty()
                    && relation.owners().iter().map(|owner| owner.position()).eq(carrier.candidates.iter().map(|candidate| *candidate as usize))
                    && relation.owners().iter().all(|owner| owner.input_projection().steps.is_empty()))
        {
            return Ok(None);
        }
        let mut roots = Vec::with_capacity(2);
        for candidate in carrier.candidates {
            let index = candidate as usize;
            let Some(Expr::Unary { op: UnOp::BorrowMut, expr: borrowed }) = arguments.get(index) else {
                return Ok(None);
            };
            let Expr::Var(root) = borrowed.as_ref() else { return Ok(None) };
            let Some(place) = self.access_facts.place_at(self.checked_module, borrowed.as_ref()) else {
                return Ok(None);
            };
            if place.root() != root
                || !place.steps().is_empty()
                || !self.mutable_bindings.contains(root)
                || !self.bindings.contains_key(root)
            {
                return Ok(None);
            }
            roots.push(root.clone());
        }
        let [first, second] = roots.as_slice() else { return Ok(None) };
        if first == second {
            return Ok(None);
        }
        Ok(Some(PreparedWriteBack {
            root: first.clone(),
            projections: Vec::new(),
            disjunctive: Some(DisjunctiveWriteBackRoots {
                selector: carrier.selector,
                first: first.clone(),
                second: second.clone(),
            }),
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// Admit one further place-owner carrier: a local handle opened directly
    /// from one fixed field of an exact mutable aggregate owner. The source
    /// loan event supplies the owner/projection, while the opening projected
    /// place fact proves that this particular local came from that relation.
    /// The enclosing direct call already authenticated the handle's exact
    /// access signature. No inferred type or similarly-shaped expression can
    /// stand in for those identities.
    fn prepare_authenticated_projected_local_exclusive_handle(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let event = match loan_facts.active_at(use_statement).iter().find(|event| {
            event.view == view && event.kind == witchy_types::loans::BorrowKind::Exclusive
        }) {
            Some(event) => event.clone(),
            None => return Ok(None),
        };
        let [witchy_canonical_source::access::LoanProjectionStep::Field(field)] = event.projection.steps.as_slice() else {
            return Ok(None);
        };
        if !event.borrower_projection.steps.is_empty()
            || !is_direct_exclusive_owned_object(&event.owner_type)
            || !self.mutable_bindings.contains(&event.owner)
            || !self.bindings.contains_key(&event.owner)
        {
            return Ok(None);
        }

        let Some((opening, borrowed, owner)) = self.source_body.stmts.iter().find_map(|opening| {
            let Stmt::Let {
                name,
                mutable: false,
                value: Expr::Unary { op: UnOp::BorrowMut, expr: borrowed },
                ..
            } = opening else { return None };
            let Expr::Field { base, field: opened_field } = borrowed.as_ref() else { return None };
            let Expr::Var(owner) = base.as_ref() else { return None };
            (name == view && owner == &event.owner && opened_field == field)
                .then_some((opening, borrowed.as_ref(), base.as_ref()))
        }) else {
            return Ok(None);
        };
        if !loan_facts.opens_after(opening).iter().any(|opened| opened == &event) {
            return Ok(None);
        }
        let Some(borrowed_place) = self.access_facts.place_at(self.checked_module, borrowed) else {
            return Ok(None);
        };
        let owner_root = event.owner_root();
        let owner_type = self.expression_type(owner)?;
        let borrowed_type = self.expression_type(borrowed)?;
        if borrowed_place.root() != event.owner
            || borrowed_place.steps()
                != [witchy_canonical_source::access::CheckedPlaceStep::Field(field.to_owned())]
            || owner_root.local != event.owner
            || owner_root.direct_storage_type.as_ref() != Some(&owner_type)
            || event.owner_type != borrowed_type
        {
            return Ok(None);
        }
        let projection = self.prepare_field_projection(owner, field)?;
        Ok(Some(PreparedWriteBack {
            root: event.owner.clone(),
            projections: vec![projection],
            disjunctive: None,
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// The bounded returned-handle carrier: `let next = pass(&mut owner)`.
    /// The frontend has already retired the opening loan and opened `next` on
    /// the same root.  Re-check every pointer-keyed component here before the
    /// canonical write-back ABI is allowed to refresh that returned handle.
    fn prepare_authenticated_returned_direct_exclusive_handle(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let Some((opening, event)) = self.returned_exclusive_handle_opening(view, use_statement) else {
            return Ok(None);
        };
        if !event.projection.steps.is_empty()
            || !event.borrower_projection.steps.is_empty()
            || !is_direct_exclusive_owned_object(&event.owner_type)
            || !self.mutable_bindings.contains(&event.owner)
            || !self.bindings.contains_key(&event.owner)
        {
            return Ok(None);
        }
        let Stmt::Let { value: Expr::Call { name: callee, args: arguments }, .. } = opening else {
            return Ok(None);
        };
        let Some(call_access) = self.access_facts.call_at(self.checked_module, match opening {
            Stmt::Let { value, .. } => value,
            _ => unreachable!("opening is a let"),
        }) else {
            return Ok(None);
        };
        let Some(declaration) = self.access_facts.declaration(callee) else {
            return Ok(None);
        };
        if call_access.verify_exact(declaration).is_err()
            || !is_direct_exclusive_reference(call_access.result().ty())
        {
            return Ok(None);
        }
        let Some((result_lifetime, result_referent)) = exclusive_reference_parts(call_access.result().ty()) else {
            return Ok(None);
        };
        let relations = call_access.borrow_relations().iter().filter(|relation| {
            relation.lifetime() == result_lifetime
                && relation.kind() == witchy_canonical_source::access::BorrowKind::Exclusive
                && relation.output_projection().steps.is_empty()
                && relation.storage_type().unqualified() == event.owner_type.unqualified()
                && matches!(relation.owners(), [candidate]
                    if candidate.input_projection().steps.is_empty()
                        && call_access.params().get(candidate.position()).is_some_and(|parameter| {
                            parameter.kind() == witchy_canonical_source::access::AccessKind::ExclusiveWriteback
                                && exclusive_reference_parts(parameter.ty()).is_some_and(|(lifetime, referent)| {
                                    lifetime == result_lifetime
                                        && referent.unqualified() == result_referent.unqualified()
                                        && is_direct_exclusive_owned_object(referent)
                                })
                        }))
        }).collect::<Vec<_>>();
        let [relation] = relations.as_slice() else { return Ok(None) };
        let owner_parameter = relation.owners()[0].position();
        let Some(Expr::Unary { op: UnOp::BorrowMut, expr: borrowed }) = arguments.get(owner_parameter) else {
            return Ok(None);
        };
        if !matches!(borrowed.as_ref(), Expr::Var(owner) if owner == &event.owner) {
            return Ok(None);
        }
        let Some(place) = self.access_facts.place_at(self.checked_module, borrowed.as_ref()) else {
            return Ok(None);
        };
        let root_type = self.expression_type(borrowed.as_ref())?;
        if place.root() != event.owner
            || !place.steps().is_empty()
            || root_type != event.owner_type
            || event.owner_root().local != event.owner
            || event.owner_root().direct_storage_type.as_ref() != Some(&root_type)
        {
            return Ok(None);
        }
        Ok(Some(PreparedWriteBack {
            root: event.owner.clone(),
            projections: Vec::new(),
            disjunctive: None,
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// Reopen the exact fixed field selected by a returned projected
    /// exclusive handle. A call result never supplies its own authority: the
    /// retained access relation and the live loan event must agree on one
    /// owner, one field, one lifetime, and the current root place.
    fn prepare_authenticated_returned_projected_exclusive_handle(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Result<Option<PreparedWriteBack>, CanonicalLoweringError> {
        let Some((opening, event)) = self.returned_exclusive_handle_opening(view, use_statement) else {
            return Ok(None);
        };
        if !event.projection.steps.is_empty()
            || !event.borrower_projection.steps.is_empty()
            || !is_direct_exclusive_owned_object(&event.owner_type)
            || !self.mutable_bindings.contains(&event.owner)
            || !self.bindings.contains_key(&event.owner)
        {
            return Ok(None);
        }
        let Stmt::Let { value: Expr::Call { name: callee, args: arguments }, .. } = opening else {
            return Ok(None);
        };
        let call = match opening { Stmt::Let { value, .. } => value, _ => unreachable!() };
        let Some(access) = self.access_facts.call_at(self.checked_module, call) else {
            return Ok(None);
        };
        let Some(declaration) = self.access_facts.declaration(callee) else {
            return Ok(None);
        };
        if access.verify_exact(declaration).is_err()
            || !is_direct_exclusive_reference(access.result().ty())
        {
            return Ok(None);
        }
        let [relation] = access.borrow_relations() else { return Ok(None) };
        let [owner_relation] = relation.owners() else { return Ok(None) };
        let parameter = owner_relation.position();
        if !exact_projected_result_root_relation(access, parameter) {
            return Ok(None);
        }
        let Some((_, signature_id)) = self.functions.get(callee) else {
            return Ok(None);
        };
        let Some(callee_signature) = self.signatures.get(signature_id.get() as usize) else {
            return Ok(None);
        };
        let Some(contract_field) = projected_return_contract_field(
            callee_signature.contract.results.first(),
            parameter,
        ) else {
            return Ok(None);
        };
        let Some(Expr::Unary { op: UnOp::BorrowMut, expr: borrowed }) = arguments.get(parameter) else {
            return Ok(None);
        };
        if !matches!(borrowed.as_ref(), Expr::Var(owner) if owner == &event.owner) {
            return Ok(None);
        }
        let Some(place) = self.access_facts.place_at(self.checked_module, borrowed.as_ref()) else {
            return Ok(None);
        };
        let root_type = self.expression_type(borrowed.as_ref())?;
        let root_value = self.bindings.get(&event.owner).and_then(|value| self.values.get(value));
        if place.root() != event.owner
            || !place.steps().is_empty()
            || event.owner_root().local != event.owner
            || event.owner_root().direct_storage_type.as_ref() != Some(&root_type)
            || !exact_write_back_root_layout(
                callee_signature.contract.parameters.get(parameter),
                callee_signature.parameters.get(parameter),
                root_value,
            )
        {
            return Ok(None);
        }
        let Some(field) = exact_unpacked_string_record_field_name(
            &root_type,
            self.definitions,
            contract_field,
        ) else {
            return Ok(None);
        };
        let projection = self.prepare_field_projection(borrowed, &field)?;
        let PreparedProjection::Field { index, .. } = &projection else {
            return Ok(None);
        };
        let field = u32::try_from(*index).map_err(|_| CanonicalLoweringError::IdOverflow)?;
        if !projected_return_contract_matches_root_relation(
            callee_signature.contract.results.first(),
            parameter,
            field,
        ) {
            return Ok(None);
        }
        Ok(Some(PreparedWriteBack {
            root: event.owner.clone(),
            projections: vec![projection],
            disjunctive: None,
            reference_alias: Some(view.to_owned()),
            detached_owned_field: false,
        }))
    }

    /// The sole projected mutable-borrow expression which may materialize a
    /// canonical reference token: an exact local opening whose retained place
    /// and loan facts name the same fixed field of a mutable aggregate root.
    /// This is intentionally separate from write-back preparation because it
    /// runs at the opening expression; later direct calls authenticate their
    /// own argument/access contract before committing the projection.
    pub(super) fn authenticated_direct_projected_exclusive_handle_open(
        &self,
        operand: &Expr,
    ) -> Result<bool, CanonicalLoweringError> {
        let loan_facts = self.legacy_loan_facts()?;
        let Expr::Field { base, field } = operand else {
            return Ok(false);
        };
        let Expr::Var(owner) = base.as_ref() else {
            return Ok(false);
        };
        let Some(statement) = self.source_statement_containing(operand) else {
            return Ok(false);
        };
        let Stmt::Let {
            name: view,
            mutable: false,
            value: Expr::Unary { op: UnOp::BorrowMut, expr },
            ..
        } = statement else {
            return Ok(false);
        };
        if !std::ptr::eq(expr.as_ref(), operand) {
            return Ok(false);
        }
        let Some(place) = self.access_facts.place_at(self.checked_module, operand) else {
            return Ok(false);
        };
        if place.root() != owner
            || place.steps()
                != [witchy_canonical_source::access::CheckedPlaceStep::Field(field.clone())]
            || !self.mutable_bindings.contains(owner)
            || !self.bindings.contains_key(owner)
        {
            return Ok(false);
        }
        let owner_type = self.expression_type(base)?;
        let borrowed_type = self.expression_type(operand)?;
        Ok(loan_facts.opens_after(statement).iter().any(|event| {
            event.view == *view
                && event.kind == witchy_types::loans::BorrowKind::Exclusive
                && event.owner == *owner
                && event.projection.steps
                    == [witchy_canonical_source::access::LoanProjectionStep::Field(field.clone())]
                && event.borrower_projection.steps.is_empty()
                && is_direct_exclusive_owned_object(&event.owner_type)
                && event.owner_root().local == *owner
                && event.owner_root().direct_storage_type.as_ref() == Some(&owner_type)
                && event.owner_type == borrowed_type
        }))
    }

    /// The one returned projected carrier is only the exact direct tail
    /// spelling selected by the source-derived callable contract. This checks
    /// the retained field index against the current checked record layout; the
    /// final authority pass separately requires the matching access/loan
    /// relation, and contract derivation requires the physical load/clone
    /// history. No arbitrary `&mut owner.field` in a write-back function is
    /// admitted here.
    pub(super) fn authenticated_returned_projected_exclusive_handle_open(
        &self,
        operand: &Expr,
    ) -> Result<bool, CanonicalLoweringError> {
        let Expr::Field { base, field } = operand else {
            return Ok(false);
        };
        let Expr::Var(owner) = base.as_ref() else {
            return Ok(false);
        };
        let Some(expected_field) = self.returned_exclusive_projected_aliases.get(owner) else {
            return Ok(false);
        };
        if self.binding_scopes.iter().any(|scope| scope.contains_key(owner))
            || !self.write_back_parameters.iter().any(|parameter| parameter == owner)
            || !self.bindings.contains_key(owner)
        {
            return Ok(false);
        }
        let Some(Stmt::Expr(Expr::Unary { op: UnOp::BorrowMut, expr })) = self.source_body.stmts.last() else {
            return Ok(false);
        };
        if !std::ptr::eq(expr.as_ref(), operand) {
            return Ok(false);
        }
        let index = match self.prepare_field_projection(base, field)? {
            PreparedProjection::Field { index, .. } => u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            PreparedProjection::Index { .. } | PreparedProjection::Dictionary { .. } => return Ok(false),
        };
        let source = self.expression_type(operand)?;
        Ok(index == *expected_field && is_projected_exclusive_leaf(&source))
    }

    pub(super) fn source_statement_containing(&self, target: &Expr) -> Option<&Stmt> {
        fn contains_expression(statement: &Stmt, target: &Expr) -> bool {
            let expression = match statement {
                Stmt::Let { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Yield(value)
                | Stmt::Expr(value)
                | Stmt::Return(Some(value)) => value,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => return false,
            };
            let mut found = false;
            let _ = witchy_syntax::ast::visit::visit_expr(expression, &mut |candidate| {
                if std::ptr::eq(candidate, target) {
                    found = true;
                    return Err(());
                }
                Ok(())
            });
            found
        }

        fn nested_statement<'a>(expression: &'a Expr, target: &Expr) -> Option<&'a Stmt> {
            fn in_block<'a>(block: &'a witchy_syntax::ast::Block, target: &Expr) -> Option<&'a Stmt> {
                for statement in &block.stmts {
                    let expression = match statement {
                        Stmt::Let { value, .. }
                        | Stmt::LetPattern { value, .. }
                        | Stmt::Assign { value, .. }
                        | Stmt::Yield(value)
                        | Stmt::Expr(value)
                        | Stmt::Return(Some(value)) => value,
                        Stmt::Return(None) | Stmt::Break | Stmt::Continue => continue,
                    };
                    if let Some(inner) = nested_statement(expression, target) {
                        return Some(inner);
                    }
                    let mut found = false;
                    let _ = witchy_syntax::ast::visit::visit_expr(expression, &mut |candidate| {
                        if std::ptr::eq(candidate, target) {
                            found = true;
                            return Err(());
                        }
                        Ok(())
                    });
                    if found {
                        return Some(statement);
                    }
                }
                None
            }

            match expression {
                Expr::List(values) | Expr::Tuple(values) | Expr::Ctor { args: values, .. }
                | Expr::AnonCtor { args: values, .. } => {
                    values.iter().find_map(|value| nested_statement(value, target))
                }
                Expr::Call { args, .. } => {
                    args.iter().find_map(|argument| nested_statement(argument, target))
                }
                Expr::LabeledCall { args, .. } => args.iter()
                    .find_map(|(_, argument)| nested_statement(argument, target)),
                Expr::LabeledMethodCall { receiver, args, .. } => nested_statement(receiver, target)
                    .or_else(|| args.iter().find_map(|(_, argument)| nested_statement(argument, target))),
                Expr::MethodCall { receiver, args, .. }
                | Expr::ExistentialCall { receiver, args, .. } => nested_statement(receiver, target)
                    .or_else(|| args.iter().find_map(|argument| nested_statement(argument, target))),
                Expr::Apply { func, args } => nested_statement(func, target)
                    .or_else(|| args.iter().find_map(|argument| nested_statement(argument, target))),
                Expr::Unary { expr, .. }
                | Expr::Try(expr)
                | Expr::As { expr, .. }
                | Expr::ExistentialPack { expr, .. }
                | Expr::ExistentialUpcast { expr, .. }
                | Expr::Field { base: expr, .. } => nested_statement(expr, target),
                Expr::Lambda { body, .. } | Expr::Block(body) => in_block(body, target),
                Expr::RecordUpdate { base, fields, .. } => nested_statement(base, target)
                    .or_else(|| fields.iter().find_map(|(_, value)| nested_statement(value, target))),
                Expr::Record { fields, spread, .. } => fields.iter()
                    .find_map(|(_, value)| nested_statement(value, target))
                    .or_else(|| spread.as_deref().and_then(|value| nested_statement(value, target))),
                Expr::Binary { lhs, rhs, .. } => nested_statement(lhs, target)
                    .or_else(|| nested_statement(rhs, target)),
                Expr::If { cond, then_block, else_block } => nested_statement(cond, target)
                    .or_else(|| in_block(then_block, target))
                    .or_else(|| else_block.as_ref().and_then(|block| in_block(block, target))),
                Expr::Match { scrutinee, arms } => nested_statement(scrutinee, target)
                    .or_else(|| arms.iter().find_map(|arm| {
                        arm.guard.as_ref().and_then(|guard| nested_statement(guard, target))
                            .or_else(|| nested_statement(&arm.body, target))
                    })),
                Expr::While { cond, body } => nested_statement(cond, target)
                    .or_else(|| in_block(body, target)),
                Expr::For { iter, body, .. } => nested_statement(iter, target)
                    .or_else(|| in_block(body, target)),
                Expr::Range { lo, hi, .. } => nested_statement(lo, target)
                    .or_else(|| nested_statement(hi, target)),
                Expr::Index { base, index } => nested_statement(base, target)
                    .or_else(|| nested_statement(index, target)),
                Expr::WhileLet { scrutinee, body, .. } => nested_statement(scrutinee, target)
                    .or_else(|| in_block(body, target)),
                Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
                | Expr::Var(_) | Expr::TaggedLit { .. } => None,
            }
        }

        for statement in &self.source_body.stmts {
            let expression = match statement {
                Stmt::Let { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Assign { value, .. }
                | Stmt::Yield(value)
                | Stmt::Expr(value)
                | Stmt::Return(Some(value)) => value,
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => continue,
            };
            if let Some(inner) = nested_statement(expression, target) {
                return Some(inner);
            }
            if contains_expression(statement, target) {
                return Some(statement);
            }
        }
        None
    }

    /// A returned reference is authenticated by the declaration which opened
    /// this exact lexical binding.  The source spelling is insufficient: an
    /// inner `let handle` may shadow an outer `handle` while both loans remain
    /// structurally visible in the function.  First choose the nearest visible
    /// opening, derive its unique event from that statement's `opens_after`,
    /// then require the exact event at the use statement.
    fn returned_exclusive_handle_opening(
        &self,
        view: &str,
        use_statement: &Stmt,
    ) -> Option<(&Stmt, witchy_types::loans::LoanEvent)> {
        fn returned_opening<'a>(statement: &'a Stmt, view: &str) -> Option<&'a Stmt> {
            matches!(statement,
                Stmt::Let { name, mutable: false, value: Expr::Call { .. }, .. } if name == view
            ).then_some(statement)
        }

        fn in_expression<'a>(
            expression: &'a Expr,
            target: &Stmt,
            view: &str,
            visible: Option<&'a Stmt>,
        ) -> Option<Option<&'a Stmt>> {
            fn in_block<'a>(
                block: &'a witchy_syntax::ast::Block,
                target: &Stmt,
                view: &str,
                inherited: Option<&'a Stmt>,
            ) -> Option<Option<&'a Stmt>> {
                let mut visible = inherited;
                for statement in &block.stmts {
                    if std::ptr::eq(statement, target) {
                        return Some(visible);
                    }
                    if let Some(expression) = statement_expression(statement)
                        && let Some(found) = in_expression(expression, target, view, visible)
                    {
                        return Some(found);
                    }
                    if matches!(statement, Stmt::Let { name, .. } if name == view) {
                        visible = returned_opening(statement, view);
                    }
                }
                None
            }

            match expression {
                Expr::List(values) | Expr::Tuple(values) | Expr::Ctor { args: values, .. }
                | Expr::AnonCtor { args: values, .. } => values.iter()
                    .find_map(|value| in_expression(value, target, view, visible)),
                Expr::Call { args, .. } => args.iter()
                    .find_map(|argument| in_expression(argument, target, view, visible)),
                Expr::LabeledCall { args, .. } => args.iter()
                    .find_map(|(_, argument)| in_expression(argument, target, view, visible)),
                Expr::LabeledMethodCall { receiver, args, .. } => in_expression(receiver, target, view, visible)
                    .or_else(|| args.iter().find_map(|(_, argument)| in_expression(argument, target, view, visible))),
                Expr::MethodCall { receiver, args, .. }
                | Expr::ExistentialCall { receiver, args, .. } => in_expression(receiver, target, view, visible)
                    .or_else(|| args.iter().find_map(|argument| in_expression(argument, target, view, visible))),
                Expr::Apply { func, args } => in_expression(func, target, view, visible)
                    .or_else(|| args.iter().find_map(|argument| in_expression(argument, target, view, visible))),
                Expr::Unary { expr, .. }
                | Expr::Try(expr)
                | Expr::As { expr, .. }
                | Expr::ExistentialPack { expr, .. }
                | Expr::ExistentialUpcast { expr, .. }
                | Expr::Field { base: expr, .. } => in_expression(expr, target, view, visible),
                Expr::Lambda { body, .. } | Expr::Block(body) => in_block(body, target, view, visible),
                Expr::RecordUpdate { base, fields, .. } => in_expression(base, target, view, visible)
                    .or_else(|| fields.iter().find_map(|(_, value)| in_expression(value, target, view, visible))),
                Expr::Record { fields, spread, .. } => fields.iter()
                    .find_map(|(_, value)| in_expression(value, target, view, visible))
                    .or_else(|| spread.as_deref().and_then(|value| in_expression(value, target, view, visible))),
                Expr::Binary { lhs, rhs, .. } => in_expression(lhs, target, view, visible)
                    .or_else(|| in_expression(rhs, target, view, visible)),
                Expr::If { cond, then_block, else_block } => in_expression(cond, target, view, visible)
                    .or_else(|| in_block(then_block, target, view, visible))
                    .or_else(|| else_block.as_ref().and_then(|block| in_block(block, target, view, visible))),
                Expr::Match { scrutinee, arms } => in_expression(scrutinee, target, view, visible)
                    .or_else(|| arms.iter().find_map(|arm| {
                        arm.guard.as_ref().and_then(|guard| in_expression(guard, target, view, visible))
                            .or_else(|| in_expression(&arm.body, target, view, visible))
                    })),
                Expr::While { cond, body } => in_expression(cond, target, view, visible)
                    .or_else(|| in_block(body, target, view, visible)),
                Expr::For { iter, body, .. } => in_expression(iter, target, view, visible)
                    .or_else(|| in_block(body, target, view, visible)),
                Expr::Range { lo, hi, .. } => in_expression(lo, target, view, visible)
                    .or_else(|| in_expression(hi, target, view, visible)),
                Expr::Index { base, index } => in_expression(base, target, view, visible)
                    .or_else(|| in_expression(index, target, view, visible)),
                Expr::WhileLet { scrutinee, body, .. } => in_expression(scrutinee, target, view, visible)
                    .or_else(|| in_block(body, target, view, visible)),
                Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
                | Expr::Var(_) | Expr::TaggedLit { .. } => None,
            }
        }

        let mut visible = None;
        let mut opening = None;
        for statement in &self.source_body.stmts {
            if std::ptr::eq(statement, use_statement) {
                opening = visible;
                break;
            }
            if let Some(expression) = statement_expression(statement)
                && let Some(found) = in_expression(expression, use_statement, view, visible)
            {
                opening = found;
                break;
            }
            if matches!(statement, Stmt::Let { name, .. } if name == view) {
                visible = returned_opening(statement, view);
            }
        }
        let opening = opening?;
        let loan_facts = self.loan_facts?;
        let events = loan_facts.opens_after(opening).iter()
            .filter(|event| event.view == view && event.kind == witchy_types::loans::BorrowKind::Exclusive)
            .collect::<Vec<_>>();
        let [event] = events.as_slice() else { return None };
        loan_facts.active_at(use_statement).contains(event)
            .then(|| (opening, (*event).clone()))
    }

    /// The source assignment captures coordinates, not the old root's token.
    /// Its RHS may replace that root or modify a sibling before the store.
    pub(super) fn try_lower_place_assignment(&mut self, root: &str, expression: &Expr, depth: u32) -> Result<bool, CanonicalLoweringError> {
        if depth == 0 {
            return Ok(false);
        }
        // Parser provenance owns the RHS boundary. Shape checking validates
        // those exact layers; it never absorbs a similar-looking RHS update.
        let Some((path, replacement)) = assignment_plan(root, expression, depth) else {
            return Err(CanonicalLoweringError::UnsupportedExpression("assignment place provenance does not match its typed rebuild"));
        };
        let mut place = PreparedWriteBack {
            root: root.to_owned(),
            projections: Vec::with_capacity(path.len()),
            disjunctive: None,
            reference_alias: None,
            detached_owned_field: false,
        };
        for projection in path {
            place.projections.push(match projection {
                AssignmentProjection::Field { base, field } => self.prepare_field_projection(base, field)?,
                AssignmentProjection::Index { base, index, expression } => self.prepare_index_projection(base, index, expression)?,
            });
        }
        let consuming = if place.projections.len() == 1
            && let PreparedProjection::Field { index, .. } = &place.projections[0]
            && let Expr::Call { args, .. } = replacement
            && let Some(Expr::Field { base, field }) = args.first()
            && matches!(base.as_ref(), Expr::Var(source) if source == root)
            && self.scalarized_records.get(root).is_some_and(|record| record.field_names.get(*index) == Some(field))
        { Some((root.to_owned(), *index)) } else { None };
        let previous = std::mem::replace(&mut self.consuming_projected_field, consuming);
        let lowered = self.lower_expr(replacement);
        self.consuming_projected_field = previous;
        let replacement = lowered?;
        self.commit_prepared_write_back(&place, replacement)?;
        Ok(true)
    }

    pub(super) fn prepare_write_back(&mut self, expression: &Expr) -> Result<PreparedWriteBack, CanonicalLoweringError> {
        match expression {
            Expr::Var(root) => {
                if !self.bindings.contains_key(root) {
                    return Err(CanonicalLoweringError::MissingBinding(root.clone()));
                }
                Ok(PreparedWriteBack {
                    root: root.clone(),
                    projections: Vec::new(),
                    disjunctive: None,
                    reference_alias: None,
                    detached_owned_field: false,
                })
            }
            Expr::Field { base, field } => {
                let mut place = self.prepare_write_back(base)?;
                place.projections.push(self.prepare_field_projection(base, field)?);
                Ok(place)
            }
            Expr::Index { base, index } => self.prepare_index_write_back(base, index, expression),
            Expr::Call { name, args }
                if matches!(name.as_str(), witchy_syntax::intrinsics::LIST_AT | witchy_syntax::intrinsics::DICT_AT) && args.len() == 2 =>
            {
                self.prepare_index_write_back(&args[0], &args[1], expression)
            }
            _ => Err(CanonicalLoweringError::UnsupportedExpression("var write-back requires a typed record/list/dictionary place")),
        }
    }

    fn prepare_index_write_back(&mut self, base: &Expr, index: &Expr, expression: &Expr) -> Result<PreparedWriteBack, CanonicalLoweringError> {
        let mut place = self.prepare_write_back(base)?;
        place.projections.push(self.prepare_index_projection(base, index, expression)?);
        Ok(place)
    }

    fn prepare_field_projection(&self, base: &Expr, field: &str) -> Result<PreparedProjection, CanonicalLoweringError> {
        let source = self.expression_type(base)?;
        let shape = scalar_aggregate_shape(&source, self.definitions)
            .filter(|shape| !shape.tagged)
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
        let index = match source.unqualified() {
            Type::Tuple(fields) => field.parse::<usize>().ok().filter(|index| *index < fields.len()),
            Type::Named(name, _) => self.record_definition(name)
                .and_then(|definition| definition.variants.first())
                .and_then(|variant| variant.field_names.iter().position(|name| name == field)),
            _ => None,
        }.filter(|index| *index < shape.fields.len())
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(field.to_owned()))?;
        Ok(PreparedProjection::Field { source, index })
    }

    fn prepare_index_projection(&mut self, base: &Expr, index: &Expr, expression: &Expr) -> Result<PreparedProjection, CanonicalLoweringError> {
        let source = self.expression_type(base)?;
        if matches!(source.unqualified(), Type::Named(name, arguments) if name == "Dict" && arguments.len() == 2) {
            let Type::Named(_, arguments) = source.unqualified() else { unreachable!() };
            let key_type = arguments[0].clone();
            let key = self.lower_expr(index)?;
            if self.values[&key] != self.field_value_type(&key_type)? {
                return Err(CanonicalLoweringError::UnsupportedExpression("dictionary place key type"));
            }
            // Own the coordinate before the RHS can change its source binding.
            // The dictionary itself is deliberately read only after the RHS.
            let key = if Self::dict_scalar_value(&key_type) { key }
                else { self.owned_for_storage(key, &key_type)? };
            return Ok(PreparedProjection::Dictionary { source, key });
        }
        if !matches!(source.unqualified(), Type::Named(name, arguments) if name == "List" && arguments.len() == 1) {
            return Err(CanonicalLoweringError::UnsupportedType(source));
        }
        // Capture all coordinates before reading the place, as the source
        // interpreter does. Index expressions may themselves perform calls.
        let index = self.lower_expr(index)?;
        if self.values[&index] != ValueType::Int {
            return Err(CanonicalLoweringError::UnsupportedExpression("list place index type"));
        }
        Ok(PreparedProjection::Index { source, index, site: self.diagnostic_site(expression)? })
    }

    pub(super) fn read_prepared_write_back(&mut self, place: &PreparedWriteBack) -> Result<ValueId, CanonicalLoweringError> {
        if let Some(record) = self.scalarized_records.get(&place.root)
            && let Some((PreparedProjection::Field { index, .. }, rest)) = place.projections.split_first()
        {
            let lane = record.lanes[*index].clone();
            let mut value = self.bindings[&lane];
            for projection in rest {
                value = self.read_write_back_projection(value, projection)?;
            }
            self.ensure_string_extent(value)?;
            self.ensure_list_extent(value)?;
            return Ok(value);
        }
        let source = if place.disjunctive.is_some() {
            place.reference_alias.as_ref().ok_or(CanonicalLoweringError::UnsupportedExpression(
                "disjunctive write-back lacks its returned handle",
            ))?
        } else {
            &place.root
        };
        let mut value = *self.bindings.get(source)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
        for projection in &place.projections {
            value = self.read_write_back_projection(value, projection)?;
        }
        self.ensure_string_extent(value)?;
        self.ensure_list_extent(value)?;
        Ok(value)
    }

    /// Transfer the field edge itself for a final `var` call argument when
    /// the ordinary record has one sole owner. The call's hidden result must
    /// restore the exact slot before this parent can be used or destroyed.
    /// The checker independently proves sole ownership and the typed slot.
    pub(super) fn take_prepared_owned_field(
        &mut self,
        place: &mut PreparedWriteBack,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        if place.disjunctive.is_some()
            || place.reference_alias.is_some()
            || self.scalarized_records.contains_key(&place.root)
        {
            return Ok(None);
        }
        let [PreparedProjection::Field { source, index }] = place.projections.as_slice() else {
            return Ok(None);
        };
        let current = *self.bindings.get(&place.root)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
        let ValueType::Ref(layout) = self.values[&current] else { return Ok(None) };
        if !self.proven_unique_record_token(current)
            || self.known_shared_value(current)
            || self.aliased_binding(&place.root, current)
        {
            return Ok(None);
        }
        let shape = scalar_aggregate_shape(source, self.definitions)
            .filter(|shape| !shape.tagged && !shape.packed)
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
        let ty = self.field_value_type(&shape.fields[*index])?;
        if !matches!(ty, ValueType::Ref(_)) {
            return Ok(None);
        }
        let physical = self.layouts.resolve_nominal(layout)
            .map_err(|_| CanonicalLoweringError::MissingLayout(source.clone()))?;
        let Some(field) = self.layouts.get(physical).and_then(|layout| layout.fields().get(*index)) else {
            return Ok(None);
        };
        let owned = match field.kind() {
            witchy_wir::layout::FieldKind::Pointer { slot: PointerSlot::Owned, .. } => true,
            witchy_wir::layout::FieldKind::Inline(slot) => self.layouts.get(slot).is_some_and(|slot| {
                matches!(slot.kind(), LayoutKind::RuntimePointerValueSlot {
                    semantics: PointerSlot::Owned, ..
                })
            }),
            _ => false,
        };
        if !owned {
            return Ok(None);
        }
        let destination = self.fresh(ty)?;
        self.push_operation(Operation::Load {
            destination,
            place: witchy_wir::canonical_safewir::Place::projected(
                current,
                Projection::Field { index: u32::try_from(*index).map_err(|_| CanonicalLoweringError::IdOverflow)? },
                ty,
                AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::TakeOwnedField,
        });
        place.detached_owned_field = true;
        Ok(Some(destination))
    }

    fn proven_unique_record_token(&self, value: ValueId) -> bool {
        self.proven_unique_record_token_via(value, &mut std::collections::BTreeSet::new())
    }

    fn proven_unique_record_token_via(
        &self,
        value: ValueId,
        visited: &mut std::collections::BTreeSet<ValueId>,
    ) -> bool {
        if !visited.insert(value) {
            return false;
        }
        for block in &self.blocks {
            if let Some(parameter) = block.parameters.iter().find(|parameter| parameter.value == value) {
                return parameter.ownership == Some(Ownership::Unique);
            }
            for operation in &block.operations {
                match operation {
                    Operation::Alloc { destination, .. } if *destination == value => return true,
                    Operation::MoveValue { destination, source } if *destination == value => {
                        return self.proven_unique_record_token_via(*source, visited);
                    }
                    _ => {}
                }
            }
        }
        false
    }

    fn read_write_back_field(&mut self, value: ValueId, source: &Type, index: usize, ty: ValueType) -> Result<ValueId, CanonicalLoweringError> {
        if matches!(self.values[&value], ValueType::InlineAggregate(_)) {
            let destination = self.fresh(ty)?;
            self.push_operation(Operation::ExtractInlineField {
                destination, aggregate: value,
                index: u32::try_from(index).map_err(|_| CanonicalLoweringError::IdOverflow)?,
            });
            Ok(destination)
        } else {
            self.load_source_aggregate_field(value, source, 0, index, index, ty)
        }
    }

    fn read_write_back_projection(&mut self, value: ValueId, projection: &PreparedProjection) -> Result<ValueId, CanonicalLoweringError> {
        match projection {
            PreparedProjection::Dictionary { source, key } => {
                if self.is_table_dictionary(source)? {
                    let Type::Named(_, arguments) = source.unqualified() else {
                        return Err(CanonicalLoweringError::UnsupportedExpression("dict table type"));
                    };
                    let [key_type, value_type] = arguments.as_slice() else {
                        return Err(CanonicalLoweringError::UnsupportedExpression("dict table type"));
                    };
                    let (found, bucket, _, _) = self.lower_dict_find_table(value, *key, key_type)?;
                    self.push_trap_unless(found)?;
                    return self.lower_dict_table_value_at(value, bucket, value_type);
                }
                Err(CanonicalLoweringError::UnsupportedType(source.clone()))
            }
            PreparedProjection::Field { source, index } => {
                let shape = scalar_aggregate_shape(source, self.definitions)
                    .filter(|shape| !shape.tagged)
                    .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
                let ty = self.field_value_type(&shape.fields[*index])?;
                self.read_write_back_field(value, source, *index, ty)
            }
            PreparedProjection::Index { source, index, site } => {
                let Type::Named(_, arguments) = source.unqualified() else {
                    return Err(CanonicalLoweringError::UnsupportedType(source.clone()));
                };
                let ty = self.field_value_type(&arguments[0])?;
                self.diagnose_list_index(value, *index, site)?;
                let index_size = self.fresh(ValueType::Size)?;
                self.push_operation(Operation::NarrowSize { destination: index_size, value: *index });
                self.ensure_list_extent(value)?;
                self.load_list_element(value, index_size, ty)
            }
        }
    }

    pub(super) fn commit_prepared_write_back(&mut self, place: &PreparedWriteBack, replacement: ValueId) -> Result<(), CanonicalLoweringError> {
        if let Some(record) = self.scalarized_records.get(&place.root)
            && let Some((PreparedProjection::Field { index, .. }, rest)) = place.projections.split_first()
        {
            let lane = record.lanes[*index].clone();
            let value = if rest.is_empty() { replacement } else {
                self.rebuild_write_back(self.bindings[&lane], rest, replacement)?
            };
            self.bindings.insert(lane, value);
            return Ok(());
        }
        if place.detached_owned_field {
            let [PreparedProjection::Field { source, index }] = place.projections.as_slice() else {
                return Err(CanonicalLoweringError::UnsupportedExpression("detached field lost exact place"));
            };
            let current = *self.bindings.get(&place.root)
                .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
            let shape = scalar_aggregate_shape(source, self.definitions)
                .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
            let expected = self.field_value_type(&shape.fields[*index])?;
            if self.values[&replacement] != expected {
                return Err(CanonicalLoweringError::UnsupportedExpression("detached field write-back type"));
            }
            // The hidden call result may be shared even when the lowering's
            // local graph cannot prove it. Retain a separate field edge and
            // leave the result token for ordinary release placement.
            let value = self.fresh(expected)?;
            self.push_operation(Operation::CloneRef { destination: value, source: replacement });
            self.push_operation(Operation::Store {
                place: witchy_wir::canonical_safewir::Place::projected(
                    current,
                    Projection::Field { index: u32::try_from(*index).map_err(|_| CanonicalLoweringError::IdOverflow)? },
                    expected,
                    AccessMode::Write,
                ),
                value,
                transfer: witchy_wir::canonical_safewir::StoreTransfer::CloneShared,
            });
            return Ok(());
        }
        if let Some(disjunctive) = &place.disjunctive {
            return self.commit_disjunctive_write_back(disjunctive, place.reference_alias.as_deref(), replacement);
        }
        let current = *self.bindings.get(&place.root)
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(place.root.clone()))?;
        // The replacement is moved into the rebuilt root below. Clone its
        // reference token first so a checked local handle continues to name
        // the same (possibly projected) storage rather than the aggregate
        // owner which contains it.
        let alias_value = place.reference_alias.as_ref()
            .map(|_| self.clone_reference_token(replacement))
            .transpose()?;
        let value = self.rebuild_write_back(current, &place.projections, replacement)?;
        self.ensure_string_extent(value)?;
        self.ensure_list_extent(value)?;
        self.bindings.insert(place.root.clone(), value);
        if let (Some(alias), Some(alias_value)) = (&place.reference_alias, alias_value) {
            self.bindings.insert(alias.clone(), alias_value);
        }
        Ok(())
    }

    /// The selector originates only from `DisjunctiveWriteBackSelector`; both
    /// CFG arms rebuild one exact candidate root and carry the other unchanged
    /// through the normal binding join. The replacement appears on mutually
    /// exclusive edges, never as a fabricated common owner.
    fn commit_disjunctive_write_back(
        &mut self,
        roots: &DisjunctiveWriteBackRoots,
        reference_alias: Option<&str>,
        replacement: ValueId,
    ) -> Result<(), CanonicalLoweringError> {
        if self.values.get(&roots.selector) != Some(&ValueType::Bool)
            || roots.first == roots.second
            || !self.mutable_bindings.contains(&roots.first)
            || !self.mutable_bindings.contains(&roots.second)
            || !self.bindings.contains_key(&roots.first)
            || !self.bindings.contains_key(&roots.second)
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "disjunctive write-back lost its checked candidate roots",
            ));
        }
        let alias = reference_alias.map(str::to_owned);
        self.lower_conditional_branches(roots.selector, false, |this| {
            let place = PreparedWriteBack {
                root: roots.first.clone(),
                projections: Vec::new(),
                disjunctive: None,
                reference_alias: alias.clone(),
                detached_owned_field: false,
            };
            this.commit_prepared_write_back(&place, replacement)?;
            Ok(None)
        }, |this| {
            let place = PreparedWriteBack {
                root: roots.second.clone(),
                projections: Vec::new(),
                disjunctive: None,
                reference_alias: alias.clone(),
                detached_owned_field: false,
            };
            this.commit_prepared_write_back(&place, replacement)?;
            Ok(None)
        })?;
        // The conditional selects exactly one of these checked roots.  Its
        // shared phis may have discarded the predecessor generation attached
        // to a count lane, so each candidate gets a fresh, guarded header
        // observation before code following the write-back reads either root.
        // Never walk arbitrary bindings: these are the authenticated carrier's
        // exact two roots plus its checked local alias when present.
        let mut refreshed = vec![roots.first.as_str(), roots.second.as_str()];
        if let Some(alias) = reference_alias {
            refreshed.push(alias);
        }
        let mut refreshed_values = std::collections::BTreeSet::new();
        for name in refreshed {
            let value = *self.bindings.get(name)
                .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
            if !refreshed_values.insert(value) {
                continue;
            }
            self.list_counts.remove(&value);
            self.list_capacities.remove(&value);
            self.known_list_lengths.remove(&value);
            self.refresh_disjunctive_string_extent(value)?;
            self.ensure_list_extent(value)?;
        }
        Ok(())
    }

    fn rebuild_write_back(&mut self, current: ValueId, projections: &[PreparedProjection], replacement: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let Some((projection, rest)) = projections.split_first() else {
            return Ok(replacement);
        };
        if let PreparedProjection::Field { source, index } = projection
            && witchy_canonical_source::storage::affine_storage_shape(source, self.definitions).is_some()
        {
            let shape = scalar_aggregate_shape(source, self.definitions)
                .filter(|shape| !shape.tagged && !shape.packed && shape.variants.len() == 1)
                .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
            let selected = (0..shape.fields.len())
                .map(|slot| !rest.is_empty() || slot != *index).collect::<Vec<_>>();
            let old_fields = self.take_consuming_aggregate_fields(current, source, 0, &shape, &selected)?;
            let child = if rest.is_empty() { None } else {
                Some(old_fields[*index].ok_or(CanonicalLoweringError::UnsupportedExpression(
                    "missing affine write-back child"))?)
            };
            let replacement = match child {
                Some(child) => self.rebuild_write_back(child, rest, replacement)?,
                None => replacement,
            };
            let expected = self.field_value_type(&shape.fields[*index])?;
            if self.values[&replacement] != expected {
                return Err(CanonicalLoweringError::UnsupportedExpression("projected write-back result type"));
            }
            let values = old_fields.into_iter().enumerate().map(|(slot, old)| {
                if slot == *index { Ok(replacement) } else {
                    old.ok_or(CanonicalLoweringError::UnsupportedExpression("missing reconstructed write-back field"))
                }
            }).collect::<Result<Vec<_>, _>>()?;
            return self.lower_aggregate_for_source(source, &shape, 0, values);
        }
        let replacement = if rest.is_empty() {
            replacement
        } else {
            let child = self.read_write_back_projection(current, projection)?;
            self.rebuild_write_back(child, rest, replacement)?
        };
        match projection {
            PreparedProjection::Dictionary { source, key } => {
                if self.is_table_dictionary(source)? {
                    let Type::Named(_, arguments) = source.unqualified() else {
                        return Err(CanonicalLoweringError::UnsupportedExpression("dict table type"));
                    };
                    let [key_type, value_type] = arguments.as_slice() else {
                        return Err(CanonicalLoweringError::UnsupportedExpression("dict table type"));
                    };
                    let (rebuilt, _) = self.lower_dict_insert_table(
                        current, key_type, value_type, *key, replacement, false,
                    )?;
                    return Ok(rebuilt);
                }
                Err(CanonicalLoweringError::UnsupportedType(source.clone()))
            }
            PreparedProjection::Index { source, index, site } => {
                self.replace_list_value(current, *index, replacement, source.clone(), site)
            }
            PreparedProjection::Field { source, index } => {
                let shape = scalar_aggregate_shape(source, self.definitions)
                    .filter(|shape| !shape.tagged)
                    .ok_or_else(|| CanonicalLoweringError::UnsupportedType(source.clone()))?;
                let mut fields = Vec::with_capacity(shape.fields.len());
                for (slot, source_type) in shape.fields.iter().enumerate() {
                    let ty = self.field_value_type(source_type)?;
                    fields.push(if slot == *index {
                        if self.values[&replacement] != ty {
                            return Err(CanonicalLoweringError::UnsupportedExpression("projected write-back result type"));
                        }
                        replacement
                    } else {
                        self.read_write_back_field(current, source, slot, ty)?
                    });
                }
                if let ValueType::InlineAggregate(layout) = self.values[&current] {
                    let destination = self.fresh(ValueType::InlineAggregate(layout))?;
                    self.push_operation(Operation::MakeInlineAggregate { destination, layout, fields });
                    Ok(destination)
                } else {
                    self.lower_heap_record_for_source(source, &shape.name, &shape.fields, fields)
                }
            }
        }
    }
}
