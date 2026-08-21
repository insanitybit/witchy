//! Whole-graph proof for RFC-0111 packed-list RC-header elision.
//!
//! Header presence is a physical-layout fact, so this pass decides it once,
//! before layout interning. Lowering never reconstructs ownership from a local
//! AST shape: it consumes only the resulting descriptor's [`RcHeader`].

use super::*;

/// Return exact `List(Packed)` types whose complete checked use-domain proves
/// one header-free representation. This first admitted class is intentionally
/// narrow: immutable non-empty literals that the existing confinement oracle
/// proves field-read-only in `main`, and never mentioned by a signature, call,
/// return, alias, mutation, nested scope, dynamic wrapper, or checked loan.
pub(super) fn proven_header_free_lists(
    module: &Module,
    table: &witchy_types::typeck::TypeTable,
    loans: &witchy_types::loans::LoanFacts,
) -> Vec<Type> {
    if !module.modes.iter().any(|mode| mode == "opt")
        || !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::RcElide)
    {
        return Vec::new();
    }
    let packed_names: HashSet<&str> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) if definition.packed => Some(definition.name.as_str()),
            _ => None,
        })
        .collect();
    let Some(main) = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function),
        _ => None,
    }) else {
        return Vec::new();
    };
    // This is the established ownership/escape oracle used by packed lowering.
    // Header elision consumes it; the whole-graph scan below does not invent a
    // second AST-local alias classifier.
    let confined = crate::escape::confined_record_list_candidates(main);

    let mut candidates = Vec::new();
    for statement in &main.body.stmts {
        let Stmt::Let {
            name,
            mutable: false,
            value: value @ Expr::List(items),
            ..
        } = statement
        else {
            continue;
        };
        if items.is_empty() || !confined.contains(name) {
            continue;
        }
        let Some(ty) = table.type_of(value).and_then(witchy_types::typeck::ty_to_ast) else {
            continue;
        };
        if is_declared_packed_list(&ty, &packed_names)
            && !candidates.iter().any(|known: &Type| same_type(known, &ty))
        {
            candidates.push(ty);
        }
    }

    candidates
        .into_iter()
        .filter(|candidate| {
            candidate_is_closed(module, main, table, loans, &confined, candidate)
        })
        .collect()
}

fn is_declared_packed_list(ty: &Type, packed_names: &HashSet<&str>) -> bool {
    let Type::Named(name, arguments) = ty.unqualified() else { return false };
    if name != "List" || arguments.len() != 1 {
        return false;
    }
    matches!(arguments[0].unqualified(), Type::Named(name, _) if packed_names.contains(name.as_str()))
}

fn candidate_is_closed(
    module: &Module,
    main: &Function,
    table: &witchy_types::typeck::TypeTable,
    loans: &witchy_types::loans::LoanFacts,
    confined: &HashSet<String>,
    candidate: &Type,
) -> bool {
    for item in &module.items {
        match item {
            Item::Function(function) => {
                if function.params.iter().any(|parameter| {
                    parameter.ty.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                }) || function.ret.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                    || function.bounds.iter().flat_map(|(_, _, arguments)| arguments).any(|ty| {
                        type_contains(ty, candidate)
                    })
                {
                    return false;
                }
            }
            Item::Type(definition) => {
                if definition
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .any(|ty| type_contains(ty, candidate))
                {
                    return false;
                }
            }
            Item::Trait(definition) => {
                if definition.methods.iter().any(|method| {
                    method.params.iter().any(|parameter| {
                        parameter.ty.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                    }) || method.ret.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                }) {
                    return false;
                }
            }
            Item::Impl(definition) => {
                if definition
                    .trait_args
                    .iter()
                    .chain(&definition.target_args)
                    .any(|ty| type_contains(ty, candidate))
                    || definition
                        .bounds
                        .iter()
                        .flat_map(|(_, _, arguments)| arguments)
                        .any(|ty| type_contains(ty, candidate))
                    || definition.methods.iter().any(|method| {
                        method.params.iter().any(|parameter| {
                            parameter
                                .ty
                                .as_ref()
                                .is_some_and(|ty| type_contains(ty, candidate))
                        }) || method.ret.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                            || method
                                .bounds
                                .iter()
                                .flat_map(|(_, _, arguments)| arguments)
                                .any(|ty| type_contains(ty, candidate))
                    })
                {
                    return false;
                }
            }
            Item::TypeAlias { ty, .. } if type_contains(ty, candidate) => return false,
            Item::Const { .. } | Item::Comptime(_) | Item::TypeAlias { .. } => {}
        }
    }

    let mut roots = HashSet::new();
    let mut constructors = HashSet::new();
    for statement in &main.body.stmts {
        let Stmt::Let {
            name,
            mutable: false,
            value: value @ Expr::List(items),
            ..
        } = statement
        else {
            continue;
        };
        if !items.is_empty()
            && confined.contains(name)
            && expr_is_exact_type(table, value, candidate)
        {
            roots.insert(name.clone());
            constructors.insert(value as *const Expr as usize);
        }
    }
    if roots.is_empty() {
        return false;
    }

    for item in &module.items {
        match item {
            Item::Function(function) => {
                let allowed_roots = (function.name == main.name).then_some(&roots);
                if !check_block(
                    &function.body,
                    table,
                    loans,
                    candidate,
                    allowed_roots,
                    &constructors,
                ) {
                    return false;
                }
            }
            Item::Const { value, .. } => {
                if !check_expr(
                    value,
                    table,
                    loans,
                    candidate,
                    None,
                    &constructors,
                ) {
                    return false;
                }
            }
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if method.default.as_ref().is_some_and(|body| {
                        !check_block(body, table, loans, candidate, None, &constructors)
                    }) {
                        return false;
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    if !check_block(
                        &method.body,
                        table,
                        loans,
                        candidate,
                        None,
                        &constructors,
                    ) {
                        return false;
                    }
                }
            }
            Item::Comptime(body) => {
                if !check_block(body, table, loans, candidate, None, &constructors) {
                    return false;
                }
            }
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    true
}

fn check_block(
    block: &Block,
    table: &witchy_types::typeck::TypeTable,
    loans: &witchy_types::loans::LoanFacts,
    candidate: &Type,
    roots: Option<&HashSet<String>>,
    constructors: &HashSet<usize>,
) -> bool {
    for statement in &block.stmts {
        if loans
            .active_at(statement)
            .iter()
            .chain(loans.opens_after(statement))
            .chain(loans.closes_after(statement))
            .any(|event| {
                type_contains(&event.owner_type, candidate)
                    || event
                        .owner_root()
                        .direct_storage_type
                        .as_ref()
                        .is_some_and(|ty| type_contains(ty, candidate))
            })
        {
            return false;
        }
        match statement {
            Stmt::Let { name, mutable, ty, value } => {
                if ty.as_ref().is_some_and(|ty| type_contains(ty, candidate))
                    && (roots.is_none_or(|roots| !roots.contains(name)) || *mutable)
                {
                    return false;
                }
                if expr_is_exact_type(table, value, candidate)
                    && roots.is_none_or(|roots| !roots.contains(name))
                {
                    return false;
                }
                if !check_expr(
                    value,
                    table,
                    loans,
                    candidate,
                    roots,
                    constructors,
                ) {
                    return false;
                }
            }
            Stmt::Assign { name, value } => {
                if roots.is_some_and(|roots| roots.contains(name))
                    || expr_is_exact_type(table, value, candidate)
                    || !check_expr(
                        value,
                        table,
                        loans,
                        candidate,
                        roots,
                        constructors,
                    )
                {
                    return false;
                }
            }
            Stmt::LetPattern { value, .. } | Stmt::Yield(value) => {
                if expr_is_exact_type(table, value, candidate)
                    || !check_expr(
                        value,
                        table,
                        loans,
                        candidate,
                        roots,
                        constructors,
                    )
                {
                    return false;
                }
            }
            Stmt::Return(value) => {
                if value.as_ref().is_some_and(|value| expr_contains_type(table, value, candidate)) {
                    return false;
                }
                if let Some(value) = value
                    && !check_expr(
                        value,
                        table,
                        loans,
                        candidate,
                        roots,
                        constructors,
                    )
                {
                    return false;
                }
            }
            Stmt::Expr(value) => {
                if !check_expr(
                    value,
                    table,
                    loans,
                    candidate,
                    roots,
                    constructors,
                ) {
                    return false;
                }
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    true
}

fn check_expr(
    expr: &Expr,
    table: &witchy_types::typeck::TypeTable,
    loans: &witchy_types::loans::LoanFacts,
    candidate: &Type,
    roots: Option<&HashSet<String>>,
    constructors: &HashSet<usize>,
) -> bool {
    if expr_is_exact_type(table, expr, candidate) {
        let allowed = match expr {
            Expr::List(_) => constructors.contains(&(expr as *const Expr as usize)),
            Expr::Var(name) => roots.is_some_and(|roots| roots.contains(name)),
            _ => false,
        };
        if !allowed {
            return false;
        }
    }

    let ordinary = |child: &Expr| {
        check_expr(
            child,
            table,
            loans,
            candidate,
            roots,
            constructors,
        )
    };
    match expr {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => items.iter().all(ordinary),
        Expr::Call { args, .. } => args.iter().all(ordinary),
        Expr::LabeledCall { args, .. } => args.iter().all(|(_, argument)| ordinary(argument)),
        Expr::LabeledMethodCall { receiver, args, .. } => {
            ordinary(receiver) && args.iter().all(|(_, argument)| ordinary(argument))
        }
        Expr::MethodCall { receiver, args, .. } => {
            ordinary(receiver) && args.iter().all(ordinary)
        }
        Expr::ExistentialCall { receiver, args, params, result, ty, .. } => {
            !type_contains(ty, candidate)
                && !type_contains(result, candidate)
                && !params.iter().any(|ty| type_contains(ty, candidate))
                && ordinary(receiver)
                && args.iter().all(ordinary)
        }
        Expr::Apply { func, args } => ordinary(func) && args.iter().all(ordinary),
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::Field { base: expr, .. } => ordinary(expr),
        Expr::As { expr, ty }
        | Expr::ExistentialPack { expr, ty, .. }
        | Expr::ExistentialUpcast { expr, ty } => {
            !type_contains(ty, candidate) && ordinary(expr)
        }
        Expr::Lambda { params, body, ret, .. } => {
            !params.iter().any(|parameter| {
                parameter.ty.as_ref().is_some_and(|ty| type_contains(ty, candidate))
            }) && ret.as_ref().is_none_or(|ty| !type_contains(ty, candidate))
                && check_block(body, table, loans, candidate, None, constructors)
        }
        Expr::Block(body) => {
            check_block(body, table, loans, candidate, None, constructors)
        }
        Expr::RecordUpdate { base, fields, .. } => {
            ordinary(base) && fields.iter().all(|(_, value)| ordinary(value))
        }
        Expr::Record { fields, spread, .. } => {
            fields.iter().all(|(_, value)| ordinary(value))
                && spread.as_deref().is_none_or(ordinary)
        }
        Expr::Binary { lhs, rhs, .. }
        | Expr::Range { lo: lhs, hi: rhs, .. }
        | Expr::Index { base: lhs, index: rhs } => ordinary(lhs) && ordinary(rhs),
        Expr::If { cond, then_block, else_block } => {
            ordinary(cond)
                && check_block(then_block, table, loans, candidate, None, constructors)
                && else_block.as_ref().is_none_or(|block| {
                    check_block(block, table, loans, candidate, None, constructors)
                })
        }
        Expr::Match { scrutinee, arms } => {
            ordinary(scrutinee)
                && arms.iter().all(|arm| {
                    arm.guard.as_ref().is_none_or(ordinary) && ordinary(&arm.body)
                })
        }
        Expr::While { cond, body } => {
            ordinary(cond) && check_block(body, table, loans, candidate, None, constructors)
        }
        Expr::For { iter, body, .. } => {
            ordinary(iter) && check_block(body, table, loans, candidate, None, constructors)
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            ordinary(scrutinee)
                && check_block(body, table, loans, candidate, None, constructors)
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => true,
    }
}

fn expr_contains_type(
    table: &witchy_types::typeck::TypeTable,
    expr: &Expr,
    candidate: &Type,
) -> bool {
    table
        .type_of(expr)
        .and_then(witchy_types::typeck::ty_to_ast)
        .is_some_and(|ty| type_contains(&ty, candidate))
}

fn expr_is_exact_type(
    table: &witchy_types::typeck::TypeTable,
    expr: &Expr,
    candidate: &Type,
) -> bool {
    table
        .type_of(expr)
        .and_then(witchy_types::typeck::ty_to_ast)
        .is_some_and(|ty| same_type(&ty, candidate))
}

fn same_type(left: &Type, right: &Type) -> bool {
    left.unqualified() == right.unqualified()
}

fn type_contains(ty: &Type, candidate: &Type) -> bool {
    if same_type(ty, candidate) {
        return true;
    }
    match ty.unqualified() {
        Type::Named(_, arguments) | Type::Dyn(_, arguments) => {
            arguments.iter().any(|argument| type_contains(argument, candidate))
        }
        Type::Tuple(fields) => fields.iter().any(|field| type_contains(field, candidate)),
        Type::Fn(parameters, result, _, _) => {
            parameters.iter().any(|parameter| type_contains(parameter, candidate))
                || type_contains(result, candidate)
        }
        Type::RecordCompose { base, fields } => {
            type_contains(base, candidate)
                || fields.iter().any(|(_, field)| type_contains(field, candidate))
        }
        Type::Slice(inner) => type_contains(inner, candidate),
        Type::Qualified(_, inner) => type_contains(inner, candidate),
    }
}
