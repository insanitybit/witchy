//! Name resolution checks (`check_type_names`, `check_trait_names`) and
//! packed-list / externref-capability boundary checks.

use super::*;

/// Reject references to undeclared types in function signatures. The
/// set of known names is the builtins plus every `type` declared in the
/// module; lowercase argument-less names are generic parameters.
pub(super) fn check_type_names(items: &[&Item]) -> Result<(), TypeError> {
    let mut known: HashSet<&str> = BUILTIN_TYPE_NAMES.iter().copied().collect();
    known.extend(CapabilityKind::ALL.iter().map(|kind| kind.name()));
    let mut arities: HashMap<&str, usize> = BUILTIN_TYPE_NAMES
        .iter()
        .chain(AMBIENT_STD_TYPE_NAMES.iter())
        .copied()
        .chain(CapabilityKind::ALL.iter().map(|kind| kind.name()))
        .filter_map(|name| builtin_type_arity(name).map(|arity| (name, arity)))
        .collect();
    let mut packed_names: HashSet<&str> = HashSet::new();
    // (RFC-0005) User `type`/`capability` declarations used by the stage-specific
    // representation checks. The transitive reference fact itself comes from
    // `ReferenceStorageClassifier` below.
    let mut type_defs: HashMap<&str, &ast::TypeDef> = HashMap::new();
    for &item in items {
        if let Item::Type(t) = item {
            known.insert(t.name.as_str());
            arities.insert(t.name.as_str(), type_def_arity(t));
            type_defs.insert(t.name.as_str(), t);
            if t.packed {
                packed_names.insert(t.name.as_str());
            }
        } else if let Item::TypeAlias { name, params, .. } = item {
            known.insert(name.as_str());
            arities.insert(name.as_str(), params.len());
        }
    }
    // Computed once per module check: `validate_type_model` previously rebuilt
    // this map (recomputing `effective_nominal_type_def_params` for every type
    // def) on EVERY call, and it's called once per type annotation across the
    // whole module — quadratic in (annotations x type defs). It depends only
    // on `type_defs`, which is fixed for the rest of this function.
    let nominal_parameters: HashMap<&str, Vec<String>> = type_defs
        .iter()
        .map(|(name, definition)| (*name, ast::effective_nominal_type_def_params(definition)))
        .collect();
    let reference_storage = ReferenceStorageClassifier::from_items(items.iter().copied());
    for &item in items {
        let in_ctx = |e: TypeError, ctx: &str| TypeError {
            message: format!("in `{}`: {}", ctx.rsplit('.').next().unwrap_or(ctx), e.message),
        };
        fn validate_block_types(
            block: &Block,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            nominal_parameters: &HashMap<&str, Vec<String>>,
            storage: &ReferenceStorageClassifier<'_>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            if let Some(region) = &block.region {
                if let Some(ty) = &region.ty {
                    validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))?;
                }
            }
            for stmt in &block.stmts {
                validate_stmt_types(stmt, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
            }
            Ok(())
        }
        fn validate_stmt_types(
            stmt: &Stmt,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            nominal_parameters: &HashMap<&str, Vec<String>>,
            storage: &ReferenceStorageClassifier<'_>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match stmt {
                Stmt::Let { ty, value, .. } => {
                    if let Some(ty) = ty {
                        validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                            .map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_expr_types(value, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Stmt::Assign { value, .. }
                | Stmt::LetPattern { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Yield(value)
                | Stmt::Expr(value) => {
                    validate_expr_types(value, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
            }
        }
        fn validate_expr_types(
            expr: &Expr,
            known: &HashSet<&str>,
            arities: &HashMap<&str, usize>,
            type_defs: &HashMap<&str, &ast::TypeDef>,
            nominal_parameters: &HashMap<&str, Vec<String>>,
            storage: &ReferenceStorageClassifier<'_>,
            ctx: &str,
            in_ctx: &impl Fn(TypeError, &str) -> TypeError,
        ) -> Result<(), TypeError> {
            match expr {
                Expr::Int(_)
                | Expr::Float(_)
                | Expr::Duration(_)
                | Expr::Str(_)
                | Expr::Bool(_)
                | Expr::Var(_)
                | Expr::TaggedLit { .. } => Ok(()),
                Expr::List(values) | Expr::Tuple(values) => {
                    for value in values {
                        validate_expr_types(value, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Call { args, .. } | Expr::AnonCtor { args, .. } => {
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Ctor { args, .. } => {
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
            Expr::LabeledCall { args, .. } => {
                for (_, arg) in args {
                    validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                }
                Ok(())
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                validate_expr_types(
                    receiver,
                    known,
                    arities,
                    type_defs,
                    nominal_parameters,
                    storage,
                    ctx,
                    in_ctx,
                )?;
                for (_, arg) in args {
                    validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                }
                Ok(())
            }
            Expr::MethodCall { receiver, args, .. } => {
                validate_expr_types(receiver, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                for arg in args {
                    validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                }
                    Ok(())
                }
                Expr::ExistentialCall {
                    receiver,
                    args,
                    ty,
                    result,
                    ..
                } => {
                    validate_expr_types(receiver, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))?;
                    validate_type_model(result, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))
                }
                Expr::Apply { func, args } => {
                    validate_expr_types(func, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    for arg in args {
                        validate_expr_types(arg, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Unary { expr, .. }
                | Expr::Field { base: expr, .. }
                | Expr::Try(expr) => {
                    validate_expr_types(expr, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::As { expr, ty } => {
                    validate_expr_types(expr, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))
                }
                Expr::ExistentialPack { expr, ty, .. } => {
                    validate_expr_types(expr, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))
                }
                Expr::ExistentialUpcast { expr, ty } => {
                    validate_expr_types(expr, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                        .map_err(|e| in_ctx(e, ctx))
                }
                Expr::Lambda { params, body, ret, .. } => {
                    for param in params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(ty, known, arities, type_defs, nominal_parameters, storage)
                                .map_err(|e| in_ctx(e, ctx))?;
                        }
                    }
                    if let Some(ret) = ret {
                        validate_type_model(ret, known, arities, type_defs, nominal_parameters, storage)
                            .map_err(|e| in_ctx(e, ctx))?;
                    }
                    validate_block_types(body, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::RecordUpdate { name: _, base, fields } => {
                    validate_expr_types(base, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Record { fields, spread, .. } => {
                    for (_, value) in fields {
                        validate_expr_types(value, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    if let Some(base) = spread {
                        validate_expr_types(base, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Binary { lhs, rhs, .. } | Expr::Range { lo: lhs, hi: rhs, .. } => {
                    validate_expr_types(lhs, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_expr_types(rhs, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::If { cond, then_block, else_block } => {
                    validate_expr_types(cond, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_block_types(then_block, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    if let Some(block) = else_block {
                        validate_block_types(block, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Match { scrutinee, arms } => {
                    validate_expr_types(scrutinee, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    for arm in arms {
                        if let Some(guard) = &arm.guard {
                            validate_expr_types(guard, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                        }
                        validate_expr_types(&arm.body, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    }
                    Ok(())
                }
                Expr::Block(block) => {
                    validate_block_types(block, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::While { cond, body } => {
                    validate_expr_types(cond, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::For { iter, body, .. } => {
                    validate_expr_types(iter, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::Index { base, index } => {
                    validate_expr_types(base, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_expr_types(index, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
                Expr::WhileLet { scrutinee, body, .. } => {
                    validate_expr_types(scrutinee, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)?;
                    validate_block_types(body, known, arities, type_defs, nominal_parameters, storage, ctx, in_ctx)
                }
            }
        }
        match item {
            Item::Function(f) => {
                for p in &f.params {
                    if let Some(t) = &p.ty {
                        validate_type_model(t, &known, &arities, &type_defs, &nominal_parameters, &reference_storage)
                            .map_err(|e| in_ctx(e, &f.name))?;
                        reject_cap_slot_boundary(
                            t,
                            &type_defs,
                            &reference_storage,
                            &f.name,
                            "a parameter",
                        )?;
                    }
                }
                if let Some(t) = &f.ret {
                    validate_type_model(t, &known, &arities, &type_defs, &nominal_parameters, &reference_storage)
                        .map_err(|e| in_ctx(e, &f.name))?;
                    reject_cap_slot_boundary(
                        t,
                        &type_defs,
                        &reference_storage,
                        &f.name,
                        "a return type",
                    )?;
                }
                validate_block_types(
                    &f.body,
                    &known,
                    &arities,
                    &type_defs,
                    &nominal_parameters,
                    &reference_storage,
                    &f.name,
                    &in_ctx,
                )?;
            }
            Item::Trait(tr) => {
                let mut trait_known = known.clone();
                let mut trait_arities = arities.clone();
                trait_known.insert("Self");
                trait_arities.insert("Self", 0);
                for method in &tr.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(
                                ty,
                                &trait_known,
                                &trait_arities,
                                &type_defs,
                                &nominal_parameters,
                                &reference_storage,
                            )
                                .map_err(|e| in_ctx(e, &tr.name))?;
                            reject_packed_list_boundary(
                                ty,
                                &packed_names,
                                &method.name,
                                "a parameter",
                            )?;
                            reject_cap_slot_boundary(
                                ty,
                                &type_defs,
                                &reference_storage,
                                &method.name,
                                "a parameter",
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type_model(
                            ret,
                            &trait_known,
                            &trait_arities,
                            &type_defs,
                            &nominal_parameters,
                            &reference_storage,
                        )
                            .map_err(|e| in_ctx(e, &tr.name))?;
                        reject_packed_list_boundary(
                            ret,
                            &packed_names,
                            &method.name,
                            "a return type",
                        )?;
                        reject_cap_slot_boundary(
                            ret,
                            &type_defs,
                            &reference_storage,
                            &method.name,
                            "a return type",
                        )?;
                    }
                    if let Some(default) = &method.default {
                        validate_block_types(
                            default,
                            &trait_known,
                            &trait_arities,
                            &type_defs,
                            &nominal_parameters,
                            &reference_storage,
                            &tr.name,
                            &in_ctx,
                        )?;
                    }
                }
            }
            Item::Impl(im) => {
                let target = ast::Type::Named(im.type_name.clone(), im.target_args.clone());
                if let Some(kind) = structural_type_kind(&im.type_name)
                    && !is_compiler_generated_structural_impl(im)
                {
                    return terr(format!(
                        "anonymous {kind} type `{}` cannot be an impl target; structural types \
                         cannot carry user behavior; define a nominal `type Name:` and implement \
                         the trait or methods for that",
                        witchy_syntax::format::type_str(&target)
                    ));
                }
                if im.type_name != "str" {
                    validate_type_model(
                        &target,
                        &known,
                        &arities,
                        &type_defs,
                        &nominal_parameters,
                        &reference_storage,
                    )
                        .map_err(|e| in_ctx(e, &im.type_name))?;
                } else {
                    validate_type(
                        &target,
                        &known,
                        &arities,
                        &nominal_parameters,
                    )
                        .map_err(|e| in_ctx(e, &im.type_name))?;
                }
                for arg in &im.trait_args {
                    validate_type_model(arg, &known, &arities, &type_defs, &nominal_parameters, &reference_storage)
                        .map_err(|e| in_ctx(e, &im.type_name))?;
                }
                for (_, _, trait_args) in &im.bounds {
                    for arg in trait_args {
                        validate_type_model(
                            arg,
                            &known,
                            &arities,
                            &type_defs,
                            &nominal_parameters,
                            &reference_storage,
                        )
                            .map_err(|e| in_ctx(e, &im.type_name))?;
                    }
                }
                let mut method_known = known.clone();
                let mut method_arities = arities.clone();
                method_known.insert("Self");
                method_arities.insert("Self", 0);
                for method in &im.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_type_model(
                                ty,
                                &method_known,
                                &method_arities,
                                &type_defs,
                                &nominal_parameters,
                                &reference_storage,
                            )
                                .map_err(|e| in_ctx(e, &method.name))?;
                            reject_packed_list_boundary(
                                ty,
                                &packed_names,
                                &method.name,
                                "a parameter",
                            )?;
                            reject_cap_slot_boundary(
                                ty,
                                &type_defs,
                                &reference_storage,
                                &method.name,
                                "a parameter",
                            )?;
                        }
                    }
                    if let Some(ret) = &method.ret {
                        validate_type_model(
                            ret,
                            &method_known,
                            &method_arities,
                            &type_defs,
                            &nominal_parameters,
                            &reference_storage,
                        )
                            .map_err(|e| in_ctx(e, &method.name))?;
                        reject_packed_list_boundary(
                            ret,
                            &packed_names,
                            &method.name,
                            "a return type",
                        )?;
                        reject_cap_slot_boundary(
                            ret,
                            &type_defs,
                            &reference_storage,
                            &method.name,
                            "a return type",
                        )?;
                    }
                    for (_, _, trait_args) in &method.bounds {
                        for arg in trait_args {
                            validate_type_model(
                                arg,
                                &method_known,
                                &method_arities,
                                &type_defs,
                                &nominal_parameters,
                                &reference_storage,
                            )
                                .map_err(|e| in_ctx(e, &method.name))?;
                        }
                    }
                    validate_block_types(
                        &method.body,
                        &method_known,
                        &method_arities,
                        &type_defs,
                        &nominal_parameters,
                        &reference_storage,
                        &method.name,
                        &in_ctx,
                    )?;
                }
            }
            // A type's variant field types must also be known. The type's own
            // name (and any sibling type) is already in `known`, so recursive and
            // mutually-recursive types check out; lowercase fields are its params.
            Item::Type(t) => {
                // `Slice` is the checker's private payload of a `&[T]` view;
                // carrier recognition keys on that name, so no source type may
                // take it.
                if t.name.rsplit('.').next() == Some("Slice") {
                    return Err(TypeError {
                        message: format!(
                            "type `{}`: the name `Slice` is reserved for the slice view `&[T]`",
                            t.name
                        ),
                    });
                }
                for variant in &t.variants {
                    for field in &variant.fields {
                        validate_type_model(
                            field,
                            &known,
                            &arities,
                            &type_defs,
                            &nominal_parameters,
                            &reference_storage,
                        )
                            .map_err(|e| in_ctx(e, &t.name))?;
                        reject_cap_slot_boundary(
                            field,
                            &type_defs,
                            &reference_storage,
                            &t.name,
                            "a field",
                        )?;
                    }
                }
                // (RFC-0027) A `packed` type's every field must be packable — a
                // scalar (Int/Float/Bool/Duration) or another `packed` type — so the
                // inline layout has a fixed, statically-known size. A variable-size
                // field (String, List, a non-packed record, …) is a check-time error
                // naming the offending field.
                if t.packed {
                    for variant in &t.variants {
                        for (i, field) in variant.fields.iter().enumerate() {
                            if !is_packable_type(field, &packed_names) {
                                let fname = variant
                                    .field_names
                                    .get(i)
                                    .map(String::as_str)
                                    .unwrap_or("(positional field)");
                                return Err(TypeError {
                                    message: format!(
                                        "`packed` type `{}` has a non-packable field `{}`: \
                                         a packed type's fields must be scalars \
                                         (Int/Float/Bool/Duration) or other `packed` types",
                                        t.name, fname
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            Item::Const { value, .. } => {
                validate_expr_types(
                    value,
                    &known,
                    &arities,
                    &type_defs,
                    &nominal_parameters,
                    &reference_storage,
                    "<const>",
                    &in_ctx,
                )?;
            }
            Item::Comptime(block) => {
                validate_block_types(
                    block,
                    &known,
                    &arities,
                    &type_defs,
                    &nominal_parameters,
                    &reference_storage,
                    "comptime",
                    &in_ctx,
                )?;
            }
            Item::TypeAlias { ty, name, .. } => {
                validate_type_model(ty, &known, &arities, &type_defs, &nominal_parameters, &reference_storage)
                    .map_err(|e| in_ctx(e, name))?;
            }
        }
    }
    Ok(())
}

/// Reject trait names that do not resolve to a declared trait before trait/impl
/// lowering turns those names into dispatch strings. A misspelled trait in a
/// bound or impl head should not become an inert contract.
pub(super) fn check_trait_names(items: &[&Item]) -> Result<(), TypeError> {
    fn bare(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    let mut arities: HashMap<&str, usize> = HashMap::new();
    arities.extend(AMBIENT_TRAIT_NAMES.iter().copied().map(|name| (name, 0)));
    for &item in items {
        if let Item::Trait(tr) = item {
            arities.insert(tr.name.as_str(), tr.typarams.len());
        }
    }

    let validate_trait_use =
        |trait_name: &str, arg_count: usize, context: String| -> Result<(), TypeError> {
            match arities.get(trait_name).copied() {
                Some(expected) if expected == arg_count => Ok(()),
                Some(expected) => terr(format!(
                    "trait `{}` expects {expected} type argument(s) but got {arg_count} in {context}",
                    bare(trait_name)
                )),
                None => terr(format!("unknown trait `{}` in {context}", bare(trait_name))),
            }
        };

    for &item in items {
        match item {
            Item::Trait(tr) => {
                for supertrait in &tr.supertraits {
                    validate_trait_use(
                        supertrait,
                        0,
                        format!("trait `{}` supertrait list", bare(&tr.name)),
                    )?;
                }
            }
            Item::Impl(im) => {
                if let Some(trait_name) = &im.trait_name {
                    validate_trait_use(
                        trait_name,
                        im.trait_args.len(),
                        format!("impl head for `{}`", bare(&im.type_name)),
                    )?;
                }
                for (_, trait_name, trait_args) in &im.bounds {
                    validate_trait_use(
                        trait_name,
                        trait_args.len(),
                        format!("impl `{}` where clause", bare(&im.type_name)),
                    )?;
                }
                for method in &im.methods {
                    for (_, trait_name, trait_args) in &method.bounds {
                        validate_trait_use(
                            trait_name,
                            trait_args.len(),
                            format!("method `{}` where clause", bare(&method.name)),
                        )?;
                    }
                }
            }
            Item::Function(f) => {
                for (var, trait_name, trait_args) in &f.bounds {
                    let bound_kind = if var.starts_with("impltrait_") {
                        "impl-trait parameter"
                    } else {
                        "where clause"
                    };
                    validate_trait_use(
                        trait_name,
                        trait_args.len(),
                        format!("{bound_kind} of function `{}`", bare(&f.name)),
                    )?;
                }
            }
            Item::Type(_) | Item::Const { .. } | Item::Comptime(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

/// (RFC-0027) Whether `t` is a packable field type for a `packed` layout: a
/// statically-fixed-size scalar (`Int`/`Float`/`Bool`/`Duration`) or another
/// `packed` type. Variable-size types (`String`, `List`, non-packed records, sum
/// types with differing payloads) are not packable.
pub(super) fn is_packable_type(t: &ast::Type, packed_names: &HashSet<&str>) -> bool {
    matches!(t, ast::Type::Named(n, args)
        if args.is_empty()
            && (matches!(n.as_str(), "Int" | "Float" | "Bool" | "Duration")
                || packed_names.contains(n.as_str())))
}

/// (RFC-0027 declared `packed`) Reject `t` if it contains a `List(P)` where `P` is
/// declared `packed`, in a boundary position (parameter / return / field). A
/// `packed` type's flat inline layout is a CONFINED LOCAL buffer — it has no
/// cross-function or stored-field ABI (that is the deep host-layout change the
/// confined inference deliberately sidesteps), so a `List` of a packed type may
/// only be built and read within one function. This keeps the layout whole-program
/// consistent: every site that handles a packed list either packs it (a confined
/// local) or is a clean compile error here, never a silent boxed fall-back.
pub(super) fn reject_packed_list_boundary(
    t: &ast::Type,
    packed_names: &HashSet<&str>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    if let Some(p) = packed_list_in_type(t, packed_names) {
        return Err(TypeError {
            message: format!(
                "`{ctx}`: a `List({p})` cannot appear in {position} — `{p}` is declared `packed`, so its \
                 list is a confined local buffer with no cross-function or stored layout. Build and read a \
                 `packed` list within one function, or drop `packed` from `{p}` to use the uniform boxed layout"
            ),
        });
    }
    Ok(())
}

/// The name of a declared-`packed` type `P` if `t` contains a `List(P)` anywhere —
/// directly, nested in another list/tuple/function type, or under an ownership
/// qualifier (`frozen`/`unique`). `None` if no packed-typed list appears.
pub(super) fn packed_list_in_type(t: &ast::Type, packed_names: &HashSet<&str>) -> Option<String> {
    match t {
        ast::Type::Named(n, args) => {
            if n == "List" {
                if let Some(ast::Type::Named(en, eargs)) = args.first().map(ast::Type::unqualified) {
                    if eargs.is_empty() && packed_names.contains(en.as_str()) {
                        return Some(en.clone());
                    }
                }
            }
            args.iter().find_map(|a| packed_list_in_type(a, packed_names))
        }
        ast::Type::Dyn(_, args) => args.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Tuple(items) => items.iter().find_map(|a| packed_list_in_type(a, packed_names)),
        ast::Type::Fn(args, ret, _, _) => args
            .iter()
            .find_map(|a| packed_list_in_type(a, packed_names))
            .or_else(|| packed_list_in_type(ret, packed_names)),
        ast::Type::RecordCompose { base, fields } => packed_list_in_type(base, packed_names)
            .or_else(|| {
                fields
                    .iter()
                    .find_map(|(_, ty)| packed_list_in_type(ty, packed_names))
            }),
        ast::Type::Qualified(_, inner) => packed_list_in_type(inner, packed_names),
        ast::Type::Slice(elem) => packed_list_in_type(elem, packed_names),
    }
}

/// (RFC-0005) Names of the capabilities represented as an unforgeable `externref`
/// on the compiled backend — the caps with NO boxed i64-slot representation.
/// `Console`/`Clock`/`Rand` are zero-representation (no runtime handle), while
/// `SecretStore` is a root authority with no guest-held handle to migrate.
/// `Option(cap)` is represented as nullable externref; structural
/// containers/tuples/functions over these caps still need typed GC/closure
/// lowering and are rejected at boundaries.
pub(super) fn is_externref_cap(name: &str) -> bool {
    externref_cap_name(name).is_some()
}

/// Route every inferred capability variant through the same source-level
/// externref set used by boundary checking and compiled lowering. Adding an
/// existing capability kind to that set therefore updates all three paths.
pub(super) fn ty_externref_cap_name(ty: &Ty) -> Option<&'static str> {
    let name = match ty {
        Ty::Console(_) => "Console",
        Ty::Clock => "Clock",
        Ty::Rand => "Rand",
        Ty::Env => "Env",
        Ty::Secret(_) => "Secret",
        Ty::Exec => "Exec",
        Ty::Fetch => "Fetch",
        Ty::Dir(_) => "Dir",
        Ty::File(_) => "File",
        Ty::Net(_) => "Net",
        Ty::Socket => "Socket",
        Ty::Listener => "Listener",
        Ty::BuildOut => "BuildOut",
        Ty::BuildRead => "BuildRead",
        Ty::BuildEnv => "BuildEnv",
        Ty::BuildNet => "BuildNet",
        Ty::BuildExec => "BuildExec",
        _ => return None,
    };
    externref_cap_name(name)
}

pub(super) fn transparent_externref_brand_cap(
    name: &str,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    if is_externref_cap(name) {
        return Some(name.to_string());
    }
    if !seen.insert(name.to_string()) {
        return None;
    }
    let out = (|| {
        let def = defs.get(name)?;
        if !def.is_capability || def.variants.len() != 1 {
            return None;
        }
        let variant = def.variants.first()?;
        if variant.name != def.name || !variant.field_names.is_empty() || variant.fields.len() != 1 {
            return None;
        }
        transparent_externref_brand_field_cap(variant.fields.first()?, defs, seen)
    })();
    seen.remove(name);
    out
}

pub(super) fn transparent_externref_brand_field_cap(
    t: &ast::Type,
    defs: &HashMap<&str, &ast::TypeDef>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match t {
        ast::Type::Qualified(_, inner) => transparent_externref_brand_field_cap(inner, defs, seen),
        ast::Type::Slice(inner) => transparent_externref_brand_field_cap(inner, defs, seen),
        ast::Type::Named(n, args) if args.is_empty() => transparent_externref_brand_cap(n, defs, seen),
        ast::Type::Named(n, _) if is_externref_cap(n) => Some(n.clone()),
        // (RFC-0081) A dyn type is never a transparent externref brand.
        ast::Type::Named(_, _) | ast::Type::Dyn(_, _) | ast::Type::Tuple(_)
        | ast::Type::Fn(..) => None,
        ast::Type::RecordCompose { .. } => unreachable!(
            "compiler invariant violated: structural record composition reached externref representation selection before records::lower normalized it"
        ),
    }
}

/// (RFC-0005 stage 4) A non-generic nominal aggregate whose variants
/// transitively carry a migrated externref capability. These lower to one typed
/// wasm GC struct; a transparent one-field capability brand remains the direct
/// externref representation instead.
pub(super) fn gc_reference_aggregate_supported(
    name: &str,
    defs: &HashMap<&str, &ast::TypeDef>,
    storage: &ReferenceStorageClassifier<'_>,
) -> bool {
    if transparent_externref_brand_cap(name, defs, &mut HashSet::new()).is_some() {
        return false;
    }
    let Some(def) = defs.get(name) else { return false };
    if def.variants.is_empty() {
        return false;
    }
    type_def_params(def).is_empty()
        && storage
            .first_reference(&ast::Type::Named(name.to_string(), Vec::new()))
            .is_some()
}

/// (RFC-0005 stage 4) The module's GC-lowered cap-carrying nominal aggregate
/// names. The representation-neutral reference fact comes from
/// `ReferenceStorageClassifier`; this function adds only the current lowering's
/// non-generic restriction. Typeck and codegen both consume this set, so they
/// cannot disagree about which types have a reference-safe representation.
pub fn gc_cap_aggregate_names(module: &ast::Module) -> Vec<String> {
    let storage = ReferenceStorageClassifier::new(module);
    let type_defs: HashMap<&str, &ast::TypeDef> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(t)
                if gc_reference_aggregate_supported(&t.name, &type_defs, &storage) =>
            {
                Some(t.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// (RFC-0005 §4.4/§7) Reject `t` when it would carry a migrated externref capability
/// through a representation the current lowering cannot preserve.
///
/// A bare capability parameter/return stays an `externref`. Closed
/// `Option`/`Result`/`List`/nominal/tuple shapes use typed GC storage. `Dict`
/// remains rejected because its key/value cells still use universal i64 slots.
pub(super) fn reject_cap_slot_boundary(
    t: &ast::Type,
    _defs: &HashMap<&str, &ast::TypeDef>,
    storage: &ReferenceStorageClassifier<'_>,
    ctx: &str,
    position: &str,
) -> Result<(), TypeError> {
    match t {
        ast::Type::Qualified(_, inner) => {
            reject_cap_slot_boundary(inner, _defs, storage, ctx, position)
        }
        ast::Type::Slice(inner) => {
            reject_cap_slot_boundary(inner, _defs, storage, ctx, position)
        }
        // (RFC-0081) A dyn value is not itself a capability slot; only its type
        // arguments need checking.
        ast::Type::Dyn(_, args) => {
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
        ast::Type::Tuple(items) => {
            items
                .iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
        ast::Type::RecordCompose { base, fields } => {
            reject_cap_slot_boundary(base, _defs, storage, ctx, position)?;
            fields.iter().try_for_each(|(_, ty)| {
                reject_cap_slot_boundary(ty, _defs, storage, ctx, position)
            })
        }
        ast::Type::Fn(args, ret, _, _) => {
            // Function signatures may mention capabilities: the typed closure ABI
            // preserves direct reference parameters and results. Still recurse so
            // unsupported containers nested in the signature are diagnosed at their
            // declaration site.
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))?;
            reject_cap_slot_boundary(ret, _defs, storage, ctx, position)
        }
        ast::Type::Named(n, args) => {
            // Dict still stores keys and values in universal i64 slots. Lists
            // use typed GC arrays for reference elements; Option uses null for a
            // direct reference or a typed wrapper; Result and closed nominals use
            // demand-planned GC structs.
            if n == "Dict" {
                for a in args {
                    match storage.first_reference(a) {
                        Some(ReferenceLeaf::Function) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: `Dict` cannot store a function value in {position}; \
                                     its key/value ABI still uses i64 slots"
                                ),
                            });
                        }
                        Some(ReferenceLeaf::ExternRef(cap)) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: a `{cap}` capability cannot be wrapped in `Dict` in {position} — \
                                     it is an unforgeable reference with no boxed representation"
                                ),
                            });
                        }
                        Some(ReferenceLeaf::Existential) => {
                            return Err(TypeError {
                                message: format!(
                                    "`{ctx}`: an owned existential cannot be wrapped in `Dict` in {position} — \
                                     its key/value ABI still uses i64 slots"
                                ),
                            });
                        }
                        None => {}
                    }
                }
            }
            // Recurse so an unsupported Dict nested in an otherwise represented
            // aggregate is still rejected at its declaration boundary.
            args.iter()
                .try_for_each(|a| reject_cap_slot_boundary(a, _defs, storage, ctx, position))
        }
    }
}

/// Whether `t` names a host capability that `main` may receive as a root
/// authority (the rights of `Dir`/`Net` don't matter here — any are grantable).
pub(crate) fn is_capability_type(t: &ast::Type) -> bool {
    matches!(t, ast::Type::Named(n, _)
        if CapabilityKind::from_name(n)
            .is_some_and(|kind| kind.class() == CapabilityClass::Host))
}
