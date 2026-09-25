//! Declaration-level validation: builtin/synthetic type names, fresh-storage
//! and uniqueness shapes, nominal lifetimes, borrowed-nominal containers,
//! authority taint, and frozen/unique/reference qualifiers.

use super::*;

/// Non-capability type names known without a declaration. Built-in capability
/// names and their zero-arity/right-bearing shapes come from
/// `witchy-cap-model`.
pub(super) const BUILTIN_TYPE_NAMES: &[&str] = &[
    "Int", "Float", "Duration", "String", "str", "Bytes", "__Msg", "Bool", "Nil", "List", "Option",
    "Result", "Dict",
];

pub(super) const AMBIENT_STD_TYPE_NAMES: &[&str] = &["Ordering", "Set", "Iter"];
pub(super) const AMBIENT_TRAIT_NAMES: &[&str] = &["PartialEq", "Eq", "PartialOrd", "Ord"];

pub(super) fn builtin_type_arity(name: &str) -> Option<usize> {
    if let Some(kind) = CapabilityKind::from_name(name) {
        return kind.builtin_arity();
    }
    match name {
        "List" | "Option" | "Set" | "Iter" => Some(1),
        "Result" | "Dict" => Some(2),
        "Int" | "Float" | "Duration" | "String" | "str" | "Bytes" | "__Msg" | "Bool" | "Nil"
        | "Ordering" => Some(0),
        _ => None,
    }
}

pub(super) fn is_builtin_type_name(name: &str) -> bool {
    BUILTIN_TYPE_NAMES.contains(&name) || CapabilityKind::from_name(name).is_some()
}

/// Linker mode markers identify the source module that owns a callable.  They
/// are retained metadata (`@opt:module`), not user spellings, so a normal entry
/// can call an imported opt export without becoming opt itself.  In particular,
/// an inherent method may lower to an unqualified `List__map`; its linker-stamped
/// source owner, rather than that mangled implementation spelling, decides the
/// contract.
pub(super) fn source_callable_is_opt(modes: &[String], function: &Function) -> bool {
    if let Some(owner) = witchy_syntax::linker::linked_source_module(function) {
        return modes.iter().any(|mode| mode == &format!("@opt:{owner}"));
    }

    // Standalone type-checking has no linked source owner. Preserve its
    // source-level `mode opt` behavior; linked modules always carry the marker
    // above, so an unqualified lowered std method cannot inherit entry mode.
    if !function.name.contains('.') {
        return modes.iter().any(|mode| mode == "opt");
    }
    modes.iter().any(|mode| {
        mode.strip_prefix("@opt:")
            .and_then(|owner| function.name.strip_prefix(owner))
            .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

/// Linked async signatures may retain the module-qualified spelling for the
/// compiler-provided Task type while source calls use its bare builtin name.
pub(super) fn same_builtin_nominal_head(left: &str, right: &str) -> bool {
    left == right
        || matches!((left, right), ("Task", "task.Task") | ("task.Task", "Task"))
}

pub(super) fn is_synthetic_type_name(name: &str) -> bool {
    name.strip_prefix("__anon").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    }) || anon_union_synthetic_arity(name).is_some()
        || name.strip_prefix("Tuple").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    })
}

pub(super) fn tuple_synthetic_arity(name: &str) -> Option<usize> {
    name.strip_prefix("Tuple")
        .and_then(|n| (!n.is_empty()).then_some(n))
        .and_then(|n| n.parse::<usize>().ok())
}

pub(super) fn anon_union_synthetic_arity(name: &str) -> Option<usize> {
    anon_union_synthetic_variants(name).map(|variants| {
        variants
            .into_iter()
            .map(|(_, arity)| arity)
            .sum()
    })
}

/// Validate that every named type in `t` is known — a builtin, a declared type,
/// or a lowercase generic parameter — so a typo like `fn f(x: Flarb)` is a clear
/// "unknown type" error rather than an opaque type that mis-unifies later.
/// Whether the value handle itself carries a `frozen` qualifier. Qualifiers
/// beneath an explicit reference describe its referent instead: `&'a frozen T`
/// leaves the reference handle movable and replaceable.
pub(super) fn is_frozen_value_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::Frozen, _) => true,
        ast::Type::Qualified(ast::TypeQual::Unique, inner) => is_frozen_value_type(inner),
        _ => false,
    }
}

/// Whether this declaration qualifies the value handle itself as `unique`.
/// A qualifier below an explicit reference belongs to the referent, so the
/// (copyable) reference handle does not become affine.
/// (RFC-0167) Whether `expression` always evaluates to freshly allocated
/// storage: a literal or constructed aggregate, a fresh-allocator intrinsic
/// (`dict.new`, `dict.with_capacity`, `list.with_capacity`), a call to a
/// function already inferred fresh, or a branch whose every arm is fresh.
pub(super) fn expr_yields_fresh_storage(expression: &Expr, fresh: &HashSet<String>) -> bool {
    match expression {
        // Literals are static data (a string literal carries no RC token),
        // never freshly allocated owner storage; only allocating shapes count.
        Expr::List(_) | Expr::Tuple(_) | Expr::Ctor { .. } | Expr::AnonCtor { .. } => true,
        Expr::Record { spread: None, .. } => true,
        Expr::Call { name, .. } => {
            matches!(
                name.as_str(),
                intrinsics::DICT_NEW | intrinsics::DICT_WITH_CAPACITY | intrinsics::LIST_WITH_CAPACITY
            ) || fresh.contains(name)
        }
        Expr::Block(block) => block_yields_fresh_storage(block, fresh),
        Expr::If { then_block, else_block: Some(else_block), .. } => {
            block_yields_fresh_storage(then_block, fresh) && block_yields_fresh_storage(else_block, fresh)
        }
        Expr::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|arm| expr_yields_fresh_storage(&arm.body, fresh))
        }
        _ => false,
    }
}

/// A block yields fresh storage when its tail expression does and every
/// early `return` inside it does too.
pub(super) fn block_yields_fresh_storage(block: &Block, fresh: &HashSet<String>) -> bool {
    let Some(Stmt::Expr(tail)) = block.stmts.last() else { return false };
    expr_yields_fresh_storage(tail, fresh) && returns_yield_fresh_storage(block, fresh)
}

pub(super) fn returns_yield_fresh_storage(block: &Block, fresh: &HashSet<String>) -> bool {
    fn in_expr(expression: &Expr, fresh: &HashSet<String>) -> bool {
        match expression {
            Expr::Block(block) => returns_yield_fresh_storage(block, fresh),
            Expr::If { then_block, else_block, .. } => {
                returns_yield_fresh_storage(then_block, fresh)
                    && else_block.as_ref().is_none_or(|block| returns_yield_fresh_storage(block, fresh))
            }
            Expr::Match { arms, .. } => arms.iter().all(|arm| in_expr(&arm.body, fresh)),
            Expr::While { body, .. } | Expr::For { body, .. } | Expr::WhileLet { body, .. } => {
                returns_yield_fresh_storage(body, fresh)
            }
            _ => true,
        }
    }
    block.stmts.iter().all(|stmt| match stmt {
        Stmt::Return(value) => value.as_ref().is_some_and(|value| expr_yields_fresh_storage(value, fresh)),
        Stmt::Expr(value) | Stmt::Yield(value) => in_expr(value, fresh),
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::LetPattern { value, .. } => {
            in_expr(value, fresh)
        }
        Stmt::Break | Stmt::Continue => true,
    })
}

pub(super) fn is_unique_value_type(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(ast::TypeQual::Unique, _) => true,
        ast::Type::Qualified(ast::TypeQual::Frozen, inner) => is_unique_value_type(inner),
        _ => false,
    }
}

pub(super) fn type_stores_value_parameter(t: &ast::Type, parameter: &str) -> bool {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => false,
        ast::Type::Qualified(_, inner) | ast::Type::Slice(inner) => {
            type_stores_value_parameter(inner, parameter)
        }
        ast::Type::Named(name, arguments) => {
            name == parameter || arguments.iter().any(|arg| type_stores_value_parameter(arg, parameter))
        }
        ast::Type::Tuple(items) => items.iter().any(|item| type_stores_value_parameter(item, parameter)),
        ast::Type::Dyn(..) | ast::Type::Fn(..) => false,
        ast::Type::RecordCompose { base, fields } => {
            type_stores_value_parameter(base, parameter)
                || fields
                    .iter()
                    .any(|(_, field)| type_stores_value_parameter(field, parameter))
        }
    }
}

pub(crate) fn type_param_names<'a>(fields: impl Iterator<Item = &'a ast::Type>) -> Vec<String> {
    ast::effective_type_params(&[], fields)
}

/// Effective type parameters for an ADT, preserving explicit order and then
/// appending implicit lowercase parameters in first field-occurrence order.
/// Every stage that substitutes ADT fields must use this authority.
pub fn type_def_params(t: &ast::TypeDef) -> Vec<String> {
    ast::effective_type_def_params(t)
}

pub(super) fn type_def_arity(t: &ast::TypeDef) -> usize {
    ast::effective_nominal_type_def_params(t).len()
}

pub(super) fn lifetime_argument_name(t: &ast::Type) -> Option<&str> {
    let ast::Type::Named(name, arguments) = t else { return None };
    arguments.is_empty().then(|| name.strip_prefix('\'')).flatten()
}

pub(super) fn collect_parameter_lifetime_binders(t: &ast::Type, lifetimes: &mut HashSet<String>) {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(lifetime)
            | ast::TypeQual::LegacyBorrow(lifetime)
            | ast::TypeQual::BorrowMut(lifetime),
            inner,
        ) => {
            lifetimes.insert(lifetime.clone());
            collect_parameter_lifetime_binders(inner, lifetimes);
        }
        ast::Type::Qualified(_, inner) => collect_parameter_lifetime_binders(inner, lifetimes),
        ast::Type::Slice(inner) => collect_parameter_lifetime_binders(inner, lifetimes),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                collect_parameter_lifetime_binders(argument, lifetimes);
            }
        }
        ast::Type::RecordCompose { base, fields } => {
            collect_parameter_lifetime_binders(base, lifetimes);
            for (_, field) in fields {
                collect_parameter_lifetime_binders(field, lifetimes);
            }
        }
        // A nested function type binds and validates its own relations; those
        // names do not enter the enclosing callable's lifetime scope.
        ast::Type::Fn(..) => {}
    }
}

pub(super) fn validate_nominal_lifetime_uses(
    t: &ast::Type,
    lifetimes: &HashSet<String>,
    context: &str,
    borrowed_qualifiers_must_be_bound: bool,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(lifetime)
            | ast::TypeQual::LegacyBorrow(lifetime)
            | ast::TypeQual::BorrowMut(lifetime),
            inner,
        ) => {
            if borrowed_qualifiers_must_be_bound && !lifetimes.contains(lifetime) {
                return terr(format!(
                    "{context} uses lifetime `'{lifetime}` but does not declare it; add \
                     `'{lifetime}` to the nominal type parameters"
                ));
            }
            validate_nominal_lifetime_uses(
                inner,
                lifetimes,
                context,
                borrowed_qualifiers_must_be_bound,
            )
        }
        ast::Type::Qualified(_, inner) => validate_nominal_lifetime_uses(
            inner,
            lifetimes,
            context,
            borrowed_qualifiers_must_be_bound,
        ),
        ast::Type::Slice(inner) => validate_nominal_lifetime_uses(
            inner,
            lifetimes,
            context,
            borrowed_qualifiers_must_be_bound,
        ),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                if let Some(lifetime) = lifetime_argument_name(argument) {
                    if !lifetimes.contains(lifetime) {
                        return terr(format!(
                            "{context} uses lifetime argument `'{lifetime}`, but no parameter \
                             binds that lifetime"
                        ));
                    }
                } else {
                    validate_nominal_lifetime_uses(
                        argument,
                        lifetimes,
                        context,
                        borrowed_qualifiers_must_be_bound,
                    )?;
                }
            }
            Ok(())
        }
        ast::Type::RecordCompose { base, fields } => {
            validate_nominal_lifetime_uses(
                base,
                lifetimes,
                context,
                borrowed_qualifiers_must_be_bound,
            )?;
            for (_, field) in fields {
                validate_nominal_lifetime_uses(
                    field,
                    lifetimes,
                    context,
                    borrowed_qualifiers_must_be_bound,
                )?;
            }
            Ok(())
        }
        ast::Type::Fn(parameters, result, _, _) => {
            let mut nested = HashSet::new();
            for parameter in parameters {
                collect_parameter_lifetime_binders(parameter, &mut nested);
            }
            for parameter in parameters {
                validate_nominal_lifetime_uses(parameter, &nested, context, true)?;
            }
            validate_nominal_lifetime_uses(result, &nested, context, true).map_err(|error| {
                if error.message.contains("does not declare it") {
                    TypeError {
                        message: format!(
                            "{}; a nested function output lifetime requires that a function \
                             parameter borrows with that lifetime",
                            error.message
                        ),
                    }
                } else {
                    error
                }
            })
        }
    }
}

pub(super) fn collect_declared_lifetime_uses(t: &ast::Type, used: &mut HashSet<String>) {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(lifetime)
            | ast::TypeQual::LegacyBorrow(lifetime)
            | ast::TypeQual::BorrowMut(lifetime),
            inner,
        ) => {
            used.insert(lifetime.clone());
            collect_declared_lifetime_uses(inner, used);
        }
        ast::Type::Qualified(_, inner) => collect_declared_lifetime_uses(inner, used),
        ast::Type::Slice(inner) => collect_declared_lifetime_uses(inner, used),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            for argument in arguments {
                if let Some(lifetime) = lifetime_argument_name(argument) {
                    used.insert(lifetime.to_string());
                } else {
                    collect_declared_lifetime_uses(argument, used);
                }
            }
        }
        ast::Type::RecordCompose { base, fields } => {
            collect_declared_lifetime_uses(base, used);
            for (_, field) in fields {
                collect_declared_lifetime_uses(field, used);
            }
        }
        // A nested function type binds its own lifetime relations. A same-spelled
        // nested binder must not make an outer nominal lifetime look used.
        ast::Type::Fn(..) => {}
    }
}

pub(super) fn type_contains_nominal_lifetime_relation(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => true,
        ast::Type::Qualified(_, inner) => type_contains_nominal_lifetime_relation(inner),
        ast::Type::Slice(inner) => type_contains_nominal_lifetime_relation(inner),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            arguments.iter().any(|argument| {
                lifetime_argument_name(argument).is_some()
                    || type_contains_nominal_lifetime_relation(argument)
            })
        }
        ast::Type::RecordCompose { base, fields } => {
            type_contains_nominal_lifetime_relation(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_contains_nominal_lifetime_relation(field))
        }
        // The function value owns its independently quantified relations; it is
        // not itself a borrowed aggregate stored by the enclosing shell.
        ast::Type::Fn(..) => false,
    }
}

/// Explicit `&`/`&mut` fields use the executable RFC-0122 place carrier.
/// Legacy `View`/`let('a)` fields deliberately do not match: their runtime
/// transport remains guarded until owner-root lowering is available.
pub(super) fn type_contains_explicit_reference_relation(t: &ast::Type) -> bool {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => true,
        ast::Type::Qualified(_, inner) => type_contains_explicit_reference_relation(inner),
        ast::Type::Slice(inner) => type_contains_explicit_reference_relation(inner),
        ast::Type::Named(_, arguments)
        | ast::Type::Tuple(arguments)
        | ast::Type::Dyn(_, arguments) => arguments
            .iter()
            .any(type_contains_explicit_reference_relation),
        ast::Type::RecordCompose { base, fields } => {
            type_contains_explicit_reference_relation(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_contains_explicit_reference_relation(field))
        }
        // Nested function values have independently quantified relations and
        // are handled by the callable carrier path, not this aggregate set.
        ast::Type::Fn(..) => false,
    }
}

pub(super) fn borrowed_nominal_relation_name<'a>(
    t: &'a ast::Type,
    lifetime_nominals: &HashSet<String>,
) -> Option<&'a str> {
    match t {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => {
            Some("View")
        }
        ast::Type::Qualified(_, inner) => {
            borrowed_nominal_relation_name(inner, lifetime_nominals)
        }
        ast::Type::Slice(inner) => {
            borrowed_nominal_relation_name(inner, lifetime_nominals)
        }
        ast::Type::Tuple(items) => items
            .iter()
            .find_map(|item| borrowed_nominal_relation_name(item, lifetime_nominals)),
        ast::Type::Named(name, arguments) => {
            let borrowed_shell = lifetime_nominals.contains(name)
                || arguments.iter().any(|argument| lifetime_argument_name(argument).is_some());
            borrowed_shell.then_some(name.as_str()).or_else(|| {
                arguments.iter().find_map(|argument| {
                    borrowed_nominal_relation_name(argument, lifetime_nominals)
                })
            })
        }
        ast::Type::Dyn(_, arguments) => arguments.iter().find_map(|argument| {
            borrowed_nominal_relation_name(argument, lifetime_nominals)
        }),
        // A function value owns its separately quantified relation. Its own
        // parameter conventions are validated when this traversal reaches the
        // nested callable below.
        ast::Type::Fn(..) | ast::Type::RecordCompose { .. } => None,
    }
}

pub(super) fn reject_borrowed_nominal_containers(
    t: &ast::Type,
    lifetime_nominals: &HashSet<String>,
    context: &str,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => {
            reject_borrowed_nominal_containers(inner, lifetime_nominals, context)
        }
        ast::Type::Slice(inner) => {
            reject_borrowed_nominal_containers(inner, lifetime_nominals, context)
        }
        ast::Type::Tuple(items) => items
            .iter()
            .try_for_each(|item| reject_borrowed_nominal_containers(item, lifetime_nominals, context)),
        ast::Type::Named(name, arguments) => {
            // A cross-module borrowed nominal is not present in the source
            // module's local declaration set yet. Its direct lifetime argument
            // still identifies the shell; only an enclosing owner such as
            // `List(Holder('a))` is a container boundary.
            let is_borrowed_shell = lifetime_nominals.contains(name)
                || arguments.iter().any(|argument| lifetime_argument_name(argument).is_some());
            // RFC-0122 admits a list whose immediate element is either an
            // executable direct reference or a lifetime-parameterized nominal
            // shell. Direct references use the place-carrier list layout;
            // nominal shells retain their checked hidden companions. Nested
            // containers, structural records, dyn, and Dict remain outside
            // that representation.
            let is_borrowed_nominal_list = name == "List"
                && arguments.len() == 1
                && matches!(
                    arguments.first(),
                    Some(ast::Type::Named(element, element_arguments))
                        if lifetime_nominals.contains(element)
                            || element_arguments.iter().any(|argument| lifetime_argument_name(argument).is_some())
                );
            let is_borrowed_nominal_option = name == "Option"
                && arguments.len() == 1
                && arguments
                    .first()
                    .is_some_and(|payload| type_contains_nominal_lifetime_relation(payload));
            let is_borrowed_nominal_result = name == "Result"
                && arguments.len() == 2
                && arguments.first().is_some_and(|payload| {
                    let ast::Type::Named(container, elements) = payload else { return false };
                    if container == "List" {
                        elements.first().is_some_and(|element| {
                            matches!(
                                element,
                                ast::Type::Named(element, element_arguments)
                                    if lifetime_nominals.contains(element)
                                        || element_arguments.iter().any(|argument| lifetime_argument_name(argument).is_some())
                            )
                        })
                    } else {
                        lifetime_nominals.contains(container)
                            || elements
                                .iter()
                                .any(|argument| lifetime_argument_name(argument).is_some())
                    }
                });
            let is_direct_reference_list = name == "List"
                && arguments.len() == 1
                && matches!(
                    arguments.first(),
                    Some(ast::Type::Qualified(
                        ast::TypeQual::Borrow(_)
                            | ast::TypeQual::BorrowMut(_)
                            | ast::TypeQual::LegacyBorrow(_),
                        _
                    ))
                );
            let is_explicit_reference_list = name == "List"
                && arguments.len() == 1
                && arguments
                    .first()
                    .is_some_and(type_contains_explicit_reference_relation);
            let is_explicit_reference_option = name == "Option"
                && arguments.len() == 1
                && arguments
                    .first()
                    .is_some_and(type_contains_explicit_reference_relation);
            let is_explicit_reference_result = name == "Result"
                && arguments.len() == 2
                && arguments
                    .first()
                    .is_some_and(type_contains_explicit_reference_relation);
            if !is_borrowed_shell
                && !is_borrowed_nominal_list
                && !is_borrowed_nominal_option
                && !is_borrowed_nominal_result
                && !is_direct_reference_list
                && !is_explicit_reference_list
                && !is_explicit_reference_option
                && !is_explicit_reference_result
                && arguments
                    .iter()
                    .any(type_contains_nominal_lifetime_relation)
            {
                return terr(format!(
                    "{context} stores a borrowed nominal relation inside `{}`; RFC-0112 stage 1 \
                     supports fixed borrowed records and tuples only. Borrowed containers require \
                     the later descriptor/root-lowering stage",
                    name.rsplit('.').next().unwrap_or(name)
                ));
            }
            arguments.iter().try_for_each(|argument| {
                reject_borrowed_nominal_containers(argument, lifetime_nominals, context)
            })
        }
        ast::Type::RecordCompose { base, fields } => {
            if type_contains_nominal_lifetime_relation(t) {
                return terr(format!(
                    "{context} stores a borrowed nominal relation inside an anonymous structural \
                     record; RFC-0112 stage 1 supports fixed nominal records and tuples only"
                ));
            }
            reject_borrowed_nominal_containers(base, lifetime_nominals, context)?;
            fields.iter().try_for_each(|(_, field)| {
                reject_borrowed_nominal_containers(field, lifetime_nominals, context)
            })
        }
        ast::Type::Dyn(_, arguments) => {
            if arguments
                .iter()
                .any(type_contains_nominal_lifetime_relation)
            {
                return terr(format!(
                    "{context} stores a borrowed nominal relation inside `dyn`; borrowed \
                     existentials are outside RFC-0112 stage 1"
                ));
            }
            Ok(())
        }
        // Nested callable relations are separately quantified, but each
        // callable surface still has to reject borrowed aggregate containers.
        ast::Type::Fn(parameters, result, conventions, _) => {
            for (index, parameter) in parameters.iter().enumerate() {
                if let Some(element) = direct_borrowed_nominal_list_name(parameter, lifetime_nominals) {
                    return terr(format!(
                        "{context} stores a borrowed nominal relation inside `List` (element `{}`); direct borrowed lists are confined to one function until their cross-call descriptor/root-lowering stage ABI is implemented",
                        element.rsplit('.').next().unwrap_or(element)
                    ));
                }
                if conventions.get(index) == Some(&Convention::Own)
                    && !type_is_explicit_reference(parameter)
                    && let Some(relation) =
                        borrowed_nominal_relation_name(parameter, lifetime_nominals)
                {
                    return terr(format!(
                        "{context} passes borrowed relation `{}` to `own`; borrowed values may \
                         only cross relation-preserving `let` parameters",
                        relation.rsplit('.').next().unwrap_or(relation)
                    ));
                }
                reject_borrowed_nominal_containers(parameter, lifetime_nominals, context)?;
            }
            if let Some(element) = direct_borrowed_nominal_list_name(result, lifetime_nominals) {
                return terr(format!(
                    "{context} stores a borrowed nominal relation inside `List` (element `{}`); direct borrowed lists are confined to one function until their cross-call descriptor/root-lowering stage ABI is implemented",
                    element.rsplit('.').next().unwrap_or(element)
                ));
            }
            reject_borrowed_nominal_containers(result, lifetime_nominals, context)
        }
    }
}

pub(super) fn direct_borrowed_nominal_list_name<'a>(
    t: &'a ast::Type,
    lifetime_nominals: &HashSet<String>,
) -> Option<&'a str> {
    let ast::Type::Named(name, arguments) = t else { return None };
    if name != "List" || arguments.len() != 1 {
        return None;
    }
    let ast::Type::Named(element, element_arguments) = arguments.first()? else {
        return None;
    };
    (lifetime_nominals.contains(element)
        || element_arguments
            .iter()
            .any(|argument| lifetime_argument_name(argument).is_some()))
    .then_some(element.as_str())
}

pub(super) fn validate_callable_nominal_lifetimes(
    name: &str,
    parameters: &[ast::Param],
    result: Option<&ast::Type>,
    lifetime_nominals: &HashSet<String>,
) -> Result<(), TypeError> {
    let mut lifetimes = HashSet::new();
    for parameter in parameters {
        if let Some(ty) = &parameter.ty {
            collect_parameter_lifetime_binders(ty, &mut lifetimes);
        }
    }
    let context = format!("callable `{}`", name.rsplit('.').next().unwrap_or(name));
    for parameter in parameters {
        if let Some(ty) = &parameter.ty {
            if let Some(element) = direct_borrowed_nominal_list_name(ty, lifetime_nominals) {
                return terr(format!(
                    "{context} stores a borrowed nominal relation inside `List` (element `{}`); direct borrowed lists are confined to one function until their cross-call descriptor/root-lowering stage ABI is implemented",
                    element.rsplit('.').next().unwrap_or(element)
                ));
            }
            if parameter.convention == Convention::Own
                && !type_is_explicit_reference(ty)
                && let Some(relation) =
                    borrowed_nominal_relation_name(ty, lifetime_nominals)
            {
                return terr(format!(
                    "{context} passes borrowed relation `{}` to `own`; borrowed values may only \
                     cross relation-preserving `let` parameters",
                    relation.rsplit('.').next().unwrap_or(relation)
                ));
            }
            // Direct callable view relations retain RFC-0083's loan-specific
            // diagnostics. Nested `fn` types recurse with strict binding above.
            validate_nominal_lifetime_uses(ty, &lifetimes, &context, false)?;
            reject_borrowed_nominal_containers(ty, lifetime_nominals, &context)?;
        }
    }
    if let Some(result) = result {
        validate_nominal_lifetime_uses(result, &lifetimes, &context, false)?;
        reject_borrowed_nominal_containers(result, lifetime_nominals, &context)?;
    }
    Ok(())
}

/// RFC-0112 stage 1: validate lifetime-bearing nominal declarations and their
/// signature relations before any runtime representation work. Lifetimes are
/// compile-time-only and remain restricted to fixed single-variant shells.
pub(super) fn check_nominal_lifetime_declarations(module: &Module) -> Result<(), TypeError> {
    let opt = module.modes.iter().any(|mode| mode == "opt");
    if !opt
        && module.linked_entry.is_none()
        && module
            .items
            .iter()
            .any(normal_mode_item_mentions_reference_surface)
    {
        return terr(
            "explicit references and lifetime parameters are available only in `mode opt` files; \
             normal Witchy uses owned values and does not require lifetime annotations",
        );
    }
    let lifetime_nominals = module
        .items
        .iter()
        .filter_map(|item| {
            let Item::Type(definition) = item else { return None };
            definition
                .params
                .iter()
                .any(|parameter| ast::is_lifetime_param(parameter))
                .then(|| definition.name.clone())
        })
        .collect::<HashSet<_>>();
    for item in &module.items {
        match item {
            Item::Type(definition) => {
                let declared = definition
                    .params
                    .iter()
                    .filter_map(|parameter| ast::lifetime_param_name(parameter))
                    .map(str::to_string)
                    .collect::<HashSet<_>>();
                if !declared.is_empty() && !opt && module.linked_entry.is_none() {
                    return terr(format!(
                        "type `{}` declares lifetime parameters, which are only available in a \
                         `mode opt` module; add `mode opt` at the top of the file",
                        definition.name.rsplit('.').next().unwrap_or(&definition.name)
                    ));
                }
                if !declared.is_empty() && definition.variants.len() != 1 {
                    return terr(format!(
                        "borrowed nominal type `{}` has {} variants; RFC-0112's initial scope \
                         supports named-field records and single-variant positional types only",
                        definition.name.rsplit('.').next().unwrap_or(&definition.name),
                        definition.variants.len()
                    ));
                }
                let context = format!(
                    "type `{}`",
                    definition.name.rsplit('.').next().unwrap_or(&definition.name)
                );
                let mut used = HashSet::new();
                for variant in &definition.variants {
                    for field in &variant.fields {
                        validate_nominal_lifetime_uses(field, &declared, &context, true)?;
                        if let Some(element) =
                            direct_borrowed_nominal_list_name(field, &lifetime_nominals)
                        {
                            return terr(format!(
                                "{context} stores a borrowed nominal relation inside `List` (element `{}`); direct borrowed lists are confined to one function until their stored descriptor/root-lowering stage ABI is implemented",
                                element.rsplit('.').next().unwrap_or(element)
                            ));
                        }
                        reject_borrowed_nominal_containers(
                            field,
                            &lifetime_nominals,
                            &context,
                        )?;
                        collect_declared_lifetime_uses(field, &mut used);
                    }
                }
                for lifetime in &declared {
                    if !used.contains(lifetime) {
                        return terr(format!(
                            "{context} declares lifetime parameter `'{lifetime}` but no field uses \
                             it; remove the parameter or relate a borrowed field to it"
                        ));
                    }
                }
            }
            Item::Function(function) => validate_callable_nominal_lifetimes(
                &function.name,
                &function.params,
                function.ret.as_ref(),
                &lifetime_nominals,
            )?,
            Item::Trait(definition) => {
                for method in &definition.methods {
                    validate_callable_nominal_lifetimes(
                        &method.name,
                        &method.params,
                        method.ret.as_ref(),
                        &lifetime_nominals,
                    )?;
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    validate_callable_nominal_lifetimes(
                        &method.name,
                        &method.params,
                        method.ret.as_ref(),
                        &lifetime_nominals,
                    )?;
                }
            }
            Item::TypeAlias { name, ty, .. } => {
                validate_nominal_lifetime_uses(
                    ty,
                    &HashSet::new(),
                    &format!("type alias `{}`", name.rsplit('.').next().unwrap_or(name)),
                    true,
                )?;
                reject_borrowed_nominal_containers(
                    ty,
                    &lifetime_nominals,
                    &format!("type alias `{}`", name.rsplit('.').next().unwrap_or(name)),
                )?;
            }
            Item::Const { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

pub(super) fn normal_mode_item_mentions_reference_surface(item: &Item) -> bool {
    match item {
        Item::Function(f)
            if matches!(
                f.name.as_str(),
                "as_str"
                    | "slice"
                    | "to_string"
                    | "len"
                    | "string.as_str"
                    | "string.slice"
                    | "string.to_string"
                    | "string.len"
                    | "String__as_str"
                    | "str__slice"
                    | "str__length"
                    | "str__len"
                    | "str__char_count"
                    | "str__is_empty"
                    | "str__to_string"
                    | "update_str"
                    | "contains_str"
                    | "get_str_or"
                    | "at_str"
                    | "get_str"
                    | "dict.update_str"
                    | "dict.contains_str"
                    | "dict.get_str_or"
                    | "dict.at_str"
                    | "dict.get_str"
                    | "Dict__update_str"
                    | "Dict__contains_str"
                    | "Dict__get_str_or"
                    | "Dict__at_str"
                    | "Dict__get_str"
            ) =>
        {
            return false;
        }
        Item::Impl(i) if i.type_name == "str" || i.type_name == "String" || matches!(i.trait_name.as_deref(), Some("Equivalent")) => {
            return false;
        }
        _ => {}
    }
    let callable_mentions_reference = |params: &[ast::Param], result: Option<&ast::Type>| {
        params
            .iter()
            .filter_map(|parameter| parameter.ty.as_ref())
            .any(type_mentions_reference_surface)
            || result.is_some_and(type_mentions_reference_surface)
    };
    match item {
        Item::Type(definition) => {
            definition
                .params
                .iter()
                .any(|parameter| ast::is_lifetime_param(parameter))
                || definition
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .any(type_mentions_reference_surface)
        }
        Item::Function(function) => {
            callable_mentions_reference(&function.params, function.ret.as_ref())
        }
        Item::Trait(definition) => definition
            .methods
            .iter()
            .any(|method| callable_mentions_reference(&method.params, method.ret.as_ref())),
        Item::Impl(definition) => definition
            .methods
            .iter()
            .any(|method| callable_mentions_reference(&method.params, method.ret.as_ref())),
        Item::TypeAlias { ty, .. } => type_mentions_reference_surface(ty),
        Item::Const { .. } | Item::Comptime(_) => false,
    }
}

pub(super) fn type_mentions_reference_surface(ty: &ast::Type) -> bool {
    match ty {
        ast::Type::Qualified(
            ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
            _,
        ) => true,
        ast::Type::Qualified(_, inner) => type_mentions_reference_surface(inner),
        ast::Type::Slice(inner) => type_mentions_reference_surface(inner),
        ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
            arguments.iter().any(|argument| {
                lifetime_argument_name(argument).is_some() || type_mentions_reference_surface(argument)
            })
        }
        ast::Type::RecordCompose { base, fields } => {
            type_mentions_reference_surface(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_mentions_reference_surface(field))
        }
        ast::Type::Fn(parameters, result, _, _) => {
            parameters.iter().any(type_mentions_reference_surface)
                || type_mentions_reference_surface(result)
        }
    }
}

pub(super) fn validate_type(
    t: &ast::Type,
    known: &HashSet<&str>,
    arities: &HashMap<&str, usize>,
    nominal_parameters: &HashMap<&str, Vec<String>>,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => {
            validate_type(inner, known, arities, nominal_parameters)
        }
        ast::Type::Slice(elem) => {
            validate_type(elem, known, arities, nominal_parameters)
        }
        // (RFC-0081) The dyn head is a trait name validated by its own pass;
        // only the type arguments are ordinary types.
        ast::Type::Dyn(_, args) => {
            args.iter()
                .try_for_each(|a| validate_type(a, known, arities, nominal_parameters))
        }
        ast::Type::Tuple(ts) => ts
            .iter()
            .try_for_each(|x| validate_type(x, known, arities, nominal_parameters)),
        ast::Type::RecordCompose { base, fields } => {
            validate_type(base, known, arities, nominal_parameters)?;
            fields
                .iter()
                .try_for_each(|(_, ty)| validate_type(ty, known, arities, nominal_parameters))
        }
        ast::Type::Fn(params, ret, _, _) => {
            params
                .iter()
                .try_for_each(|p| validate_type(p, known, arities, nominal_parameters))?;
            validate_type(ret, known, arities, nominal_parameters)
        }
        ast::Type::Named(n, args) => {
            if let Some(lifetime) = lifetime_argument_name(t) {
                return terr(format!(
                    "lifetime `'{lifetime}` may only appear as an argument for a declared \
                     lifetime parameter"
                ));
            }
            // `Dir`/`File`/`Net`/`Secret` carry capability *rights* markers
            // (`Dir[Read]`, `Net[Connect]`, `Secret[Seal]`) in their arguments, not
            // types. Validate the marker vocabulary here (BUG-154) so a typo
            // (`Dir[Reed]`, `Secret[Sealed]`) or a rejected right (`Net[Tls]` — TLS is
            // an endpoint scheme, not a Net right; RFC-0009) is a clear error instead
            // of a silently-normalized capability whose authority shape no longer
            // matches the source.
            if witchy_cap_model::bears_rights_markers(n) {
                return validate_cap_markers(n, args);
            }
            // A monomorphized instance spells a `&[T]` argument with the
            // checker's private `Slice(T)` payload; it is the surface `[T]`.
            if n == "Slice" && args.len() == 1 && !known.contains(n.as_str()) {
                return validate_type(&args[0], known, arities, nominal_parameters);
            }
            if known.contains(n.as_str()) || is_synthetic_type_name(n) {
                if let Some(expected) = arities
                    .get(n.as_str())
                    .copied()
                    .or_else(|| tuple_synthetic_arity(n))
                    .or_else(|| anon_union_synthetic_arity(n))
                {
                    let got = args.len();
                    if got != expected {
                        return terr(format!(
                            "type `{n}` expects {expected} type argument(s) but got {got}"
                        ));
                    }
                }
                if let Some(parameters) = nominal_parameters.get(n.as_str()) {
                    for (index, (argument, parameter)) in
                        args.iter().zip(parameters).enumerate()
                    {
                        let expected_lifetime = ast::is_lifetime_param(parameter);
                        let supplied_lifetime = lifetime_argument_name(argument);
                        match (expected_lifetime, supplied_lifetime) {
                            (true, None) => {
                                return terr(format!(
                                    "type `{n}` expects a lifetime argument for parameter \
                                     `{parameter}` at position {}, but got ordinary type `{}`",
                                    index + 1,
                                    witchy_syntax::format::type_str(argument)
                                ));
                            }
                            (false, Some(lifetime)) => {
                                return terr(format!(
                                    "lifetime argument `'{lifetime}` cannot be used for ordinary \
                                     type parameter `{parameter}` of `{n}`"
                                ));
                            }
                            (true, Some(_)) => {}
                            (false, None) => {
                                validate_type(argument, known, arities, nominal_parameters)?;
                            }
                        }
                    }
                    Ok(())
                } else {
                    for (index, argument) in args.iter().enumerate() {
                        if let Some(lifetime) = lifetime_argument_name(argument) {
                            return terr(format!(
                                "lifetime argument `'{lifetime}` cannot be used in ordinary type \
                                 position {} of `{n}`",
                                index + 1
                            ));
                        }
                        validate_type(argument, known, arities, nominal_parameters)?;
                    }
                    Ok(())
                }
            } else if args.is_empty() && n.chars().next().is_some_and(|c| c.is_lowercase()) && !n.contains('.') {
                // A lowercase, argument-less name is a generic type parameter.
                Ok(())
            } else {
                terr(format!("unknown type `{n}`"))
            }
        }
    }
}

pub(super) fn is_builtin_authority_type_name(name: &str) -> bool {
    matches!(
        name,
        "Console"
            | "Clock"
            | "Rand"
            | "Env"
            | "Secret"
            | "SecretStore"
            | "Dir"
            | "File"
            | "Net"
            | "Fetch"
            | "Exec"
            | "Socket"
            | "Listener"
            | "BuildOut"
            | "BuildRead"
            | "BuildEnv"
            | "BuildNet"
            | "BuildExec"
    )
}

pub(super) fn is_anon_record_synthetic_name(name: &str) -> bool {
    name.strip_prefix("__anon")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn structural_type_kind(name: &str) -> Option<&'static str> {
    if is_anon_record_synthetic_name(name) {
        Some("record")
    } else if anon_union_synthetic_variants(name).is_some() {
        Some("union")
    } else {
        None
    }
}

pub(super) fn authority_taint_type(
    ty: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match ty {
        ast::Type::Qualified(_, inner) => authority_taint_type(inner, defs, seen),
        ast::Type::Slice(inner) => authority_taint_type(inner, defs, seen),
        // (RFC-0081) A dyn value is never itself a capability; only its type
        // arguments can carry taint.
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| authority_taint_type(a, defs, seen)),
        ast::Type::Tuple(items) => items.iter().find_map(|t| authority_taint_type(t, defs, seen)),
        ast::Type::Fn(params, ret, _, _) => params
            .iter()
            .chain(std::iter::once(ret.as_ref()))
            .find_map(|t| authority_taint_type(t, defs, seen)),
        ast::Type::RecordCompose { base, fields } => std::iter::once(base.as_ref())
            .chain(fields.iter().map(|(_, ty)| ty))
            .find_map(|ty| authority_taint_type(ty, defs, seen)),
        ast::Type::Named(name, args) => {
            if is_builtin_authority_type_name(name) {
                return Some(name.clone());
            }
            if let Some(hit) = args.iter().find_map(|a| authority_taint_type(a, defs, seen)) {
                return Some(hit);
            }
            let def = defs.get(name.as_str())?;
            if def.is_capability {
                return Some(name.clone());
            }
            if !seen.insert(name.clone()) {
                return None;
            }
            let hit = def
                .variants
                .iter()
                .flat_map(|v| v.fields.iter())
                .find_map(|field| authority_taint_type(field, defs, seen));
            seen.remove(name);
            hit
        }
    }
}

pub(super) fn reject_structural_authority_type(
    ty: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
) -> Result<(), TypeError> {
    match ty {
        ast::Type::Qualified(_, inner) => reject_structural_authority_type(inner, defs),
        ast::Type::Slice(inner) => reject_structural_authority_type(inner, defs),
        ast::Type::Dyn(_, args) => {
            args.iter().try_for_each(|arg| reject_structural_authority_type(arg, defs))
        }
        ast::Type::Tuple(items) => {
            items.iter().try_for_each(|item| reject_structural_authority_type(item, defs))
        }
        ast::Type::Fn(params, ret, _, _) => {
            params.iter().try_for_each(|param| reject_structural_authority_type(param, defs))?;
            reject_structural_authority_type(ret, defs)
        }
        ast::Type::RecordCompose { base, fields } => {
            let components = || {
                std::iter::once(base.as_ref()).chain(fields.iter().map(|(_, ty)| ty))
            };
            if let Some(cap) = components()
                .find_map(|ty| authority_taint_type(ty, defs, &mut HashSet::new()))
            {
                return terr(format!(
                    "anonymous record types cannot contain capability `{cap}` — structural values \
                     cannot carry authority; name a capability type or pass the capability directly"
                ));
            }
            components().try_for_each(|ty| reject_structural_authority_type(ty, defs))
        }
        ast::Type::Named(name, args) => {
            if let Some(kind) = structural_type_kind(name) {
                if let Some(cap) =
                    args.iter().find_map(|arg| authority_taint_type(arg, defs, &mut HashSet::new()))
                {
                    return terr(format!(
                        "anonymous {kind} types cannot contain capability `{cap}` — structural values \
                         cannot carry authority; name a capability type or pass the capability directly"
                    ));
                }
            }
            args.iter().try_for_each(|arg| reject_structural_authority_type(arg, defs))
        }
    }
}

pub(super) fn reject_borrowed_capability_views(
    ty: &ast::Type,
    storage: &ReferenceStorageClassifier<'_>,
) -> Result<(), TypeError> {
    match ty {
        ast::Type::Qualified(ast::TypeQual::View, inner) => {
            if let Some(capability) = storage.first_stored_capability(inner) {
                return terr(format!(
                    "immutable view of `{}` would retain capability `{capability}`, but no authenticated retained-authority carrier is admitted",
                    witchy_syntax::format::type_str(inner)
                ));
            }
            reject_borrowed_capability_views(inner, storage)
        }
        ast::Type::Qualified(
            ast::TypeQual::Borrow(lifetime)
            | ast::TypeQual::LegacyBorrow(lifetime)
            | ast::TypeQual::BorrowMut(lifetime),
            inner,
        ) => {
            if let Some(capability) = storage.first_stored_capability(inner) {
                return terr(format!(
                    "explicit reference with lifetime `'{lifetime}` to `{}` names capability `{capability}` as an \
                     ordinary owner, but host capabilities require a lease-bearing API; an \
                     ordinary lifetime cannot extend capability authority",
                    witchy_syntax::format::type_str(inner)
                ));
            }
            reject_borrowed_capability_views(inner, storage)
        }
        ast::Type::Qualified(_, inner) => reject_borrowed_capability_views(inner, storage),
        ast::Type::Slice(inner) => reject_borrowed_capability_views(inner, storage),
        ast::Type::Named(_, arguments)
        | ast::Type::Tuple(arguments)
        | ast::Type::Dyn(_, arguments) => arguments
            .iter()
            .try_for_each(|argument| reject_borrowed_capability_views(argument, storage)),
        ast::Type::RecordCompose { base, fields } => {
            reject_borrowed_capability_views(base, storage)?;
            fields.iter().try_for_each(|(_, field)| {
                reject_borrowed_capability_views(field, storage)
            })
        }
        ast::Type::Fn(parameters, result, _, _) => {
            parameters.iter().try_for_each(|parameter| {
                reject_borrowed_capability_views(parameter, storage)
            })?;
            reject_borrowed_capability_views(result, storage)
        }
    }
}

pub(super) fn validate_type_model(
    t: &ast::Type,
    known: &HashSet<&str>,
    arities: &HashMap<&str, usize>,
    type_defs: &HashMap<&str, &ast::TypeDef>,
    nominal_parameters: &HashMap<&str, Vec<String>>,
    storage: &ReferenceStorageClassifier<'_>,
) -> Result<(), TypeError> {
    validate_type(t, known, arities, nominal_parameters)?;
    reject_bare_unsized_types(t)?;
    validate_frozen_unique_reference_qualifiers(t)?;
    reject_invalid_frozen_payloads(t, type_defs)?;
    reject_structural_authority_type(t, type_defs)?;
    reject_borrowed_capability_views(t, storage)
}

/// `frozen` is deep payload immutability, not a way to turn authority into a
/// shareable value or to hide an exclusive loan inside an immutable shell.
/// Check it while source nominal structure is still available: `Ty` erases
/// qualifiers and cannot recover a nested record field after monomorphization.
pub(super) fn reject_invalid_frozen_payloads(
    t: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
) -> Result<(), TypeError> {
    fn substitute(
        t: &ast::Type,
        substitutions: &HashMap<String, ast::Type>,
    ) -> ast::Type {
        match t {
            ast::Type::Named(name, arguments) if arguments.is_empty() => substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| t.clone()),
            ast::Type::Named(name, arguments) => ast::Type::Named(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| substitute(argument, substitutions))
                    .collect(),
            ),
            ast::Type::Tuple(items) => ast::Type::Tuple(
                items.iter().map(|item| substitute(item, substitutions)).collect(),
            ),
            ast::Type::Slice(item) => ast::Type::Slice(Box::new(substitute(item, substitutions))),
            ast::Type::RecordCompose { base, fields } => ast::Type::RecordCompose {
                base: Box::new(substitute(base, substitutions)),
                fields: fields
                    .iter()
                    .map(|(name, field)| (name.clone(), substitute(field, substitutions)))
                    .collect(),
            },
            ast::Type::Fn(parameters, result, conventions, qualifiers) => ast::Type::Fn(
                parameters.iter().map(|parameter| substitute(parameter, substitutions)).collect(),
                Box::new(substitute(result, substitutions)),
                conventions.clone(),
                *qualifiers,
            ),
            ast::Type::Qualified(qualifier, inner) => {
                ast::Type::Qualified(qualifier.clone(), Box::new(substitute(inner, substitutions)))
            }
            ast::Type::Dyn(name, arguments) => ast::Type::Dyn(
                name.clone(),
                arguments
                    .iter()
                    .map(|argument| substitute(argument, substitutions))
                    .collect(),
            ),
        }
    }

    fn stored_payload_hazard(
        t: &ast::Type,
        defs: &HashMap<&str, &ast::TypeDef>,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        match t {
            ast::Type::Qualified(ast::TypeQual::BorrowMut(_), _) => {
                Some("an exclusive-reference field".into())
            }
            // Frozen preserves shared loans; it neither owns nor copies their
            // referents. An exclusive reference is handled above because the
            // frozen aggregate could otherwise retain mutable access.
            ast::Type::Qualified(
                ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_),
                _,
            ) => None,
            ast::Type::Qualified(ast::TypeQual::Unique, _) => {
                Some("a unique (affine) payload".into())
            }
            ast::Type::Qualified(_, inner) | ast::Type::Slice(inner) => {
                stored_payload_hazard(inner, defs, seen)
            }
            ast::Type::Named(name, arguments) => {
                if is_builtin_authority_type_name(name) {
                    return Some(format!("capability `{name}`"));
                }
                let Some(definition) = defs.get(name.as_str()) else {
                    // Built-in containers own their arguments. User nominals
                    // are always present in `defs`; an unbound type parameter
                    // has no arguments and remains transportable (e.g.
                    // `frozen a`).
                    return arguments
                        .iter()
                        .find_map(|argument| stored_payload_hazard(argument, defs, seen));
                };
                if definition.is_capability {
                    return Some(format!("capability `{name}`"));
                }
                if definition.must_consume {
                    return Some(format!("must-consume payload `{name}`"));
                }
                if !seen.insert(name.clone()) {
                    return None;
                }
                let parameters = ast::effective_nominal_type_def_params(definition);
                let substitutions = parameters
                    .into_iter()
                    .zip(arguments.iter().cloned())
                    .collect::<HashMap<_, _>>();
                let hazard = definition
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .find_map(|field| {
                        stored_payload_hazard(&substitute(field, &substitutions), defs, seen)
                    });
                seen.remove(name);
                hazard
            }
            ast::Type::Tuple(items) => items
                .iter()
                .find_map(|item| stored_payload_hazard(item, defs, seen)),
            // A `dyn` value is an owned, erased concrete payload plus a
            // witness. Its type arguments describe only the method surface,
            // so no source type can prove that every value it may pack is
            // copyable. In particular, `dynamic.Dynamic` stores `dyn Reflect`;
            // accepting `frozen Dynamic` would let its erased payload hide a
            // must/unique/once value, capability, or exclusive reference.
            // Reject the shell before qualifier erasure, rather than trying to
            // recover a packing fact from a later runtime descriptor.
            ast::Type::Dyn(_, _) => Some("an existential payload".into()),
            ast::Type::Fn(_, _, _, qualifiers) => qualifiers
                .once
                .then_some("a once-callable (affine) payload".into()),
            ast::Type::RecordCompose { base, fields } => {
                stored_payload_hazard(base, defs, seen).or_else(|| {
                    fields
                        .iter()
                        .find_map(|(_, field)| stored_payload_hazard(field, defs, seen))
                })
            }
        }
    }

    fn visit(
        t: &ast::Type,
        defs: &HashMap<&str, &ast::TypeDef>,
    ) -> Result<(), TypeError> {
        match t {
            ast::Type::Qualified(ast::TypeQual::Frozen, inner) => {
                if let Some(hazard) = stored_payload_hazard(inner, defs, &mut HashSet::new()) {
                    let diagnostic = if let Some(capability) = hazard.strip_prefix("capability ") {
                        format!("frozen capabilities do not exist (payload carries {capability})")
                    } else {
                        format!("frozen values cannot contain {hazard}")
                    };
                    return terr(format!(
                        "`frozen {}` is invalid: {diagnostic}",
                        witchy_syntax::format::type_str(inner),
                    ));
                }
                visit(inner, defs)
            }
            ast::Type::Qualified(_, inner) | ast::Type::Slice(inner) => visit(inner, defs),
            ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
                arguments.iter().try_for_each(|argument| visit(argument, defs))
            }
            ast::Type::RecordCompose { base, fields } => {
                visit(base, defs)?;
                fields.iter().try_for_each(|(_, field)| visit(field, defs))
            }
            ast::Type::Fn(parameters, result, _, _) => {
                parameters.iter().try_for_each(|parameter| visit(parameter, defs))?;
                visit(result, defs)
            }
        }
    }

    visit(t, defs)
}

pub(super) fn reject_bare_unsized_types(t: &ast::Type) -> Result<(), TypeError> {
    fn visit(t: &ast::Type, under_reference: bool) -> Result<(), TypeError> {
        match t {
            ast::Type::Qualified(
                ast::TypeQual::View
                | ast::TypeQual::Borrow(_)
                | ast::TypeQual::LegacyBorrow(_)
                | ast::TypeQual::BorrowMut(_),
                inner,
            ) => visit(inner, true),
            ast::Type::Qualified(_, inner) => visit(inner, under_reference),
            ast::Type::Slice(elem) => {
                if !under_reference {
                    return terr(format!(
                        "slice type `[{}]` cannot appear as a bare value; use a reference type like `&[{}]`",
                        witchy_syntax::format::type_str(elem),
                        witchy_syntax::format::type_str(elem)
                    ));
                }
                visit(elem, false)
            }
            ast::Type::Named(name, args) => {
                if name == "str" && !under_reference {
                    return terr(
                        "string slice `str` cannot appear as a bare value; use a reference type like `&str` or owned `String`".to_string()
                    );
                }
                args.iter().try_for_each(|arg| visit(arg, false))
            }
            ast::Type::Tuple(items) => items.iter().try_for_each(|item| visit(item, false)),
            ast::Type::Dyn(_, args) => args.iter().try_for_each(|arg| visit(arg, false)),
            ast::Type::Fn(params, ret, _, _) => {
                params.iter().try_for_each(|p| visit(p, false))?;
                visit(ret, false)
            }
            ast::Type::RecordCompose { base, fields } => {
                visit(base, false)?;
                fields.iter().try_for_each(|(_, ty)| visit(ty, false))
            }
        }
    }
    visit(t, false)
}

/// Validate the RFC-0167 ownership/reference qualifier matrix without changing
/// the erased runtime type. `frozen` and `unique` qualify owned values, while a
/// reference's outermost qualifier is its access relation. A qualifier below a
/// reference therefore describes the referent: `&'a frozen T` and both forms of
/// `&'a unique T` are valid, but `&'a mut frozen T` is not.
pub(super) fn validate_frozen_unique_reference_qualifiers(t: &ast::Type) -> Result<(), TypeError> {
    fn visit(t: &ast::Type) -> Result<(), TypeError> {
        let mut cursor = t;
        let mut frozen = false;
        let mut unique = false;
        let mut outer_reference_qualifier = None;
        loop {
            match cursor {
                ast::Type::Qualified(ast::TypeQual::Frozen, inner) => {
                    frozen = true;
                    outer_reference_qualifier.get_or_insert("frozen");
                    cursor = inner;
                }
                ast::Type::Qualified(ast::TypeQual::Unique, inner) => {
                    unique = true;
                    outer_reference_qualifier.get_or_insert("unique");
                    cursor = inner;
                }
                ast::Type::Qualified(
                    ast::TypeQual::View | ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_),
                    _,
                ) if outer_reference_qualifier.is_some() => {
                    return terr(format!(
                        "outer `{}` reference qualifiers are invalid; write the ownership qualifier inside the reference target",
                        outer_reference_qualifier.expect("set above")
                    ));
                }
                _ => break,
            }
        }
        if frozen && unique {
            return terr(
                "`frozen` and `unique` cannot qualify the same value; transfer unique storage into a frozen snapshot instead",
            );
        }
        match t {
            ast::Type::Qualified(reference, inner)
                if matches!(reference, ast::TypeQual::View | ast::TypeQual::Borrow(_) | ast::TypeQual::LegacyBorrow(_) | ast::TypeQual::BorrowMut(_)) => {
                let mut target = inner.as_ref();
                let mut target_frozen = false;
                loop {
                    match target {
                        ast::Type::Qualified(ast::TypeQual::Frozen, next) => {
                            target_frozen = true;
                            target = next;
                        }
                        ast::Type::Qualified(ast::TypeQual::Unique, next) => target = next,
                        _ => break,
                    }
                }
                if target_frozen && matches!(reference, ast::TypeQual::BorrowMut(_)) {
                    return terr(format!(
                        "`{}` cannot target `frozen` storage; an exclusive reference requires a mutable referent",
                        witchy_syntax::format::type_str(t)
                    ));
                }
                visit(inner)
            }
            ast::Type::Qualified(_, inner) => visit(inner),
            ast::Type::Slice(inner) => visit(inner),
            ast::Type::Named(_, arguments) | ast::Type::Tuple(arguments) | ast::Type::Dyn(_, arguments) => {
                arguments.iter().try_for_each(visit)
            }
            ast::Type::RecordCompose { base, fields } => {
                visit(base)?;
                fields.iter().try_for_each(|(_, field)| visit(field))
            }
            ast::Type::Fn(parameters, result, _, _) => {
                parameters.iter().try_for_each(visit)?;
                visit(result)
            }
        }
    }
    visit(t)
}
