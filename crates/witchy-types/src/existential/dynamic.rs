use super::*;

use crate::access::{AccessSignature, CheckedAccessFacts};
#[derive(Clone, Debug)]
pub(super) struct DynamicMethodDefinition {
    name: String,
    function: String,
    pub(super) receiver: Type,
    pub(super) parameters: Vec<DynamicMethodParameterDefinition>,
    pub(super) result: Type,
    pub(super) access: AccessSignature,
}

#[derive(Clone, Debug)]
pub(super) enum DynamicMethodParameterDefinition {
    Value(Type),
    Capability(Type),
}

pub(super) fn collect_dynamic_method_definitions(
    module: &Module,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
    access_facts: &CheckedAccessFacts<'_>,
) -> Result<Vec<DynamicMethodDefinition>, String> {
    let functions = module.items.iter().filter_map(|item| match item {
        Item::Function(function) if function.attributes.iter().any(|attr| attr == "dynamic") => {
            Some(function)
        }
        _ => None,
    });
    let mut methods = Vec::new();
    let mut identities = BTreeSet::new();
    for function in functions {
        let runtime_catalog = runtime_catalog.ok_or_else(|| {
            "@dynamic methods require authenticated runtime declaration ownership".to_string()
        })?;
        if !function.public {
            return Err(format!("@dynamic function `{}` must be public", function.name));
        }
        if function.comptime_only || function.is_async || function.is_gen {
            return Err(format!(
                "@dynamic function `{}` must be an ordinary runtime function",
                function.name
            ));
        }
        let signature_types = function
            .params
            .iter()
            .filter_map(|parameter| parameter.ty.as_ref())
            .chain(function.ret.iter());
        if !function.bounds.is_empty()
            || !crate::typeck::type_param_names(signature_types).is_empty()
        {
            return Err(format!(
                "@dynamic function `{}` must have a closed non-generic signature",
                function.name
            ));
        }
        let Some(receiver) = function.params.first() else {
            return Err(format!(
                "@dynamic function `{}` requires `self` as its first parameter",
                function.name
            ));
        };
        if receiver.name != "self" {
            return Err(format!(
                "@dynamic function `{}` requires `self` as its first parameter",
                function.name
            ));
        }
        let receiver = receiver.ty.clone().ok_or_else(|| {
            format!(
                "@dynamic function `{}` requires an explicit receiver type",
                function.name
            )
        })?;
        let parameter_types = function
            .params
            .iter()
            .skip(1)
            .map(|parameter| {
                parameter.ty.clone().ok_or_else(|| {
                    format!(
                        "@dynamic function `{}` parameter `{}` requires an explicit type",
                        function.name, parameter.name
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = function.ret.clone().unwrap_or_else(|| Type::Tuple(Vec::new()));
        for (role, ty) in [("receiver", &receiver), ("result", &result)] {
            runtime_catalog
                .capability_free_type_identity(ty, module)
                .map_err(|error| {
                    format!(
                        "@dynamic function `{}` {role} must be capability-free: {error}",
                        function.name,
                    )
                })?;
        }
        let parameters = parameter_types
            .into_iter()
            .map(|ty| match runtime_catalog.capability_free_type_identity(&ty, module) {
                Ok(_) => Ok(DynamicMethodParameterDefinition::Value(ty)),
                Err(RuntimeTypeError::CapabilityType(_)
                | RuntimeTypeError::CapabilityRetained { .. }) => {
                    Ok(DynamicMethodParameterDefinition::Capability(ty))
                }
                Err(error) => Err(format!(
                    "@dynamic function `{}` has an unsupported parameter `{}`: {error}",
                    function.name,
                    witchy_syntax::format::type_str(&ty),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let name = function
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&function.name)
            .to_string();
        let access = access_facts.declaration(&function.name).cloned().ok_or_else(|| {
            format!(
                "@dynamic function `{}` lost its checked access signature",
                function.name,
            )
        })?;
        let receiver_identity = runtime_catalog
            .type_identity(&receiver)
            .map_err(|error| error.to_string())?;
        if !identities.insert((receiver_identity, name.clone())) {
            return Err(format!(
                "duplicate @dynamic method `{name}` for `{}`",
                witchy_syntax::format::type_str(&receiver)
            ));
        }
        methods.push(DynamicMethodDefinition {
            name,
            function: function.name.clone(),
            receiver,
            parameters,
            result,
            access,
        });
    }
    Ok(methods)
}

pub(super) fn prepare_dynamic_methods(
    methods: Vec<DynamicMethodDefinition>,
    runtime_catalog: &RuntimeDeclarationCatalog,
    runtime_types: &RuntimeTypePlan,
) -> Result<Vec<RuntimeMethodDescriptor>, String> {
    let descriptor = |ty: &Type| {
        let identity = runtime_catalog
            .type_identity(ty)
            .map_err(|error| error.to_string())?;
        runtime_types.id(&identity).ok_or_else(|| {
            format!(
                "dynamic method plan lost descriptor for `{}`",
                witchy_syntax::format::type_str(ty)
            )
        })
    };
    methods
        .into_iter()
        .map(|method| {
            let receiver = descriptor(&method.receiver)?;
            let parameters = method
                .parameters
                .into_iter()
                .map(|parameter| match parameter {
                    DynamicMethodParameterDefinition::Value(ty) => {
                        Ok(RuntimeMethodParameterDescriptor::Value(
                            RuntimeMethodArgumentDescriptor {
                                descriptor: descriptor(&ty)?,
                                display: witchy_syntax::format::type_str(&ty),
                                ty,
                            },
                        ))
                    }
                    DynamicMethodParameterDefinition::Capability(ty) => {
                        Ok(RuntimeMethodParameterDescriptor::Capability(
                            RuntimeMethodCapabilityDescriptor {
                                display: witchy_syntax::format::type_str(&ty),
                                ty,
                            },
                        ))
                    }
                })
                .collect::<Result<Vec<_>, String>>()?;
            let result = descriptor(&method.result)?;
            let callable_identity = runtime_catalog
                .checked_callable_identity(&method.access)
                .map_err(|error| error.to_string())?;
            if runtime_types.id(&callable_identity).is_none() {
                return Err(format!(
                    "dynamic method plan lost the checked callable descriptor for `{}`",
                    method.function,
                ));
            }
            let RuntimeTypeIdentity::Function { access, .. } = &callable_identity else {
                unreachable!("checked callable identities are functions")
            };
            let access = (**access).clone();
            let borrow_relation_storage_displays = method
                .access
                .borrow_relations()
                .iter()
                .map(|relation| witchy_syntax::format::type_str(relation.storage_type()))
                .collect();
            Ok(RuntimeMethodDescriptor {
                receiver,
                receiver_type: method.receiver,
                name: method.name,
                function: method.function,
                parameters,
                result,
                result_display: witchy_syntax::format::type_str(&method.result),
                result_type: method.result,
                callable_identity,
                access,
                borrow_relation_storage_displays,
            })
        })
        .collect()
}

fn dynamic_intrinsic(name: &str, intrinsic: &str) -> bool {
    name == intrinsic || name.rsplit('.').next() == Some(intrinsic)
}

fn dynamic_call_with(name: &str) -> bool {
    name == "dynamic.call_with" || dynamic_intrinsic(name, intrinsics::DYNAMIC_CALL_WITH)
}

fn resolved_expr_type(table: &TypeTable, expr: &Expr) -> Result<Option<Type>, String> {
    table
        .type_of(expr)
        .map(|ty| {
            ty_to_ast(ty).ok_or_else(|| {
                "Dynamic operation requires one fully resolved concrete type".to_string()
            })
        })
        .transpose()
}

fn dynamic_identity_request(
    module: &Module,
    table: &TypeTable,
    expr: &Expr,
    include_internal_descriptor_ids: bool,
) -> Result<Option<Type>, String> {
    let Expr::Call { name, args } = expr else { return Ok(None) };
    if dynamic_intrinsic(name, intrinsics::DYNAMIC_RUNTIME_TYPE) {
        let [Expr::Str(handle), Expr::Str(_source)] = args.as_slice() else {
            return Err("dynamic.runtime_type requires one compiler-owned type argument".into());
        };
        let mut matches = module
            .compiler_type_syntax
            .iter()
            .filter(|syntax| syntax.runtime_identity && syntax.handle == *handle);
        let syntax = matches.next().ok_or_else(|| {
            "dynamic.runtime_type lost its compiler-owned type syntax".to_string()
        })?;
        if matches.any(|candidate| candidate.ty != syntax.ty) {
            return Err("dynamic.runtime_type has an ambiguous compiler-owned type handle".into());
        }
        return Ok(Some(syntax.ty.clone()));
    }
    if dynamic_intrinsic(name, intrinsics::DYNAMIC_DESCRIPTOR) {
        let [value] = args.as_slice() else {
            return Err("Dynamic descriptor construction requires one value".into());
        };
        return resolved_expr_type(table, value);
    }
    if dynamic_intrinsic(name, intrinsics::DYNAMIC_DESCRIPTOR_ID) {
        if !include_internal_descriptor_ids {
            return Ok(None);
        }
        let [value] = args.as_slice() else {
            return Err("Dynamic descriptor identity requires one value".into());
        };
        return resolved_expr_type(table, value);
    }
    if dynamic_intrinsic(name, intrinsics::DYNAMIC_TRY_DECODE) {
        let Some(result) = resolved_expr_type(table, expr)? else { return Ok(None) };
        let Type::Named(option, arguments) = result.unqualified() else {
            return Err("Dynamic try_decode must infer Option(T)".into());
        };
        if option.rsplit('.').next() != Some("Option") || arguments.len() != 1 {
            return Err("Dynamic try_decode must infer Option(T)".into());
        }
        return Ok(Some(arguments[0].clone()));
    }
    Ok(None)
}

pub(super) fn collect_dynamic_types(
    module: &Module,
    table: &TypeTable,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<Vec<Type>, String> {
    let mut requested = Vec::new();
    visit_module_exprs(module, &mut |expr| {
        if let Some(ty) =
            dynamic_identity_request(module, table, expr, runtime_catalog.is_some())?
        {
            let trait_descriptor = matches!(
                expr,
                Expr::Call { name, .. }
                    if dynamic_intrinsic(name, intrinsics::DYNAMIC_RUNTIME_TYPE)
                        && matches!(ty.unqualified(), Type::Dyn(..))
            );
            requested.push((ty, trait_descriptor));
        }
        Ok(())
    })?;
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    let runtime_catalog = runtime_catalog.ok_or_else(|| {
        "Dynamic operations require authenticated runtime declaration ownership".to_string()
    })?;
    for (ty, trait_descriptor) in &requested {
        if *trait_descriptor {
            runtime_catalog
                .type_identity(ty)
                .map_err(|error| error.to_string())?;
        } else {
            runtime_catalog
                .capability_free_type_identity(ty, module)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(requested.into_iter().map(|(ty, _)| ty).collect())
}

pub(super) fn prepare_dynamic_trait_relations(
    dynamic_types: &[Type],
    catalog: &WitnessCatalog,
    runtime_catalog: &RuntimeDeclarationCatalog,
    runtime_types: &RuntimeTypePlan,
) -> Result<Vec<(crate::runtime_type::RuntimeTypeId, crate::runtime_type::RuntimeTypeId)>, String> {
    let traits = dynamic_types
        .iter()
        .filter(|ty| matches!(ty.unqualified(), Type::Dyn(..)))
        .collect::<Vec<_>>();
    let concrete_types = dynamic_types
        .iter()
        .filter(|ty| !matches!(ty.unqualified(), Type::Dyn(..)))
        .collect::<Vec<_>>();
    let mut relations = Vec::new();
    for concrete in concrete_types {
        let concrete_identity = runtime_catalog
            .type_identity(concrete)
            .map_err(|error| error.to_string())?;
        let concrete_id = runtime_types.id(&concrete_identity).ok_or_else(|| {
            "Dynamic trait plan lost a prepared concrete descriptor".to_string()
        })?;
        for trait_type in &traits {
            if !catalog.implements(concrete, trait_type)? {
                continue;
            }
            let trait_identity = runtime_catalog
                .type_identity(trait_type)
                .map_err(|error| error.to_string())?;
            let trait_id = runtime_types.id(&trait_identity).ok_or_else(|| {
                "Dynamic trait plan lost a prepared trait descriptor".to_string()
            })?;
            relations.push((concrete_id, trait_id));
        }
    }
    Ok(relations)
}

pub(super) fn rewrite_dynamic_module(
    module: &mut Module,
    table: &TypeTable,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
    runtime_types: &RuntimeTypePlan,
) -> Result<(), String> {
    let dynamic_context = module.clone();
    for item in &mut module.items {
        match item {
            Item::Function(function) => {
                rewrite_dynamic_block(
                    &mut function.body,
                    &dynamic_context,
                    table,
                    runtime_catalog,
                    runtime_types,
                )?;
            }
            Item::Trait(definition) => {
                for method in &mut definition.methods {
                    if let Some(default) = &mut method.default {
                        rewrite_dynamic_block(
                            default,
                            &dynamic_context,
                            table,
                            runtime_catalog,
                            runtime_types,
                        )?;
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &mut definition.methods {
                    rewrite_dynamic_block(
                        &mut method.body,
                        &dynamic_context,
                        table,
                        runtime_catalog,
                        runtime_types,
                    )?;
                }
            }
            Item::Const { value, .. } => {
                rewrite_dynamic_expr(
                    value,
                    &dynamic_context,
                    table,
                    runtime_catalog,
                    runtime_types,
                )?;
            }
            Item::Comptime(block) => {
                rewrite_dynamic_block(
                    block,
                    &dynamic_context,
                    table,
                    runtime_catalog,
                    runtime_types,
                )?;
            }
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

fn rewrite_dynamic_block(
    block: &mut Block,
    module: &Module,
    table: &TypeTable,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
    runtime_types: &RuntimeTypePlan,
) -> Result<(), String> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => {
                rewrite_dynamic_expr(value, module, table, runtime_catalog, runtime_types)?;
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_dynamic_expr(
    expr: &mut Expr,
    module: &Module,
    table: &TypeTable,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
    runtime_types: &RuntimeTypePlan,
) -> Result<(), String> {
    let request = dynamic_identity_request(module, table, expr, runtime_catalog.is_some())?;
    let call_with_capability_type = match expr {
        Expr::Call { name, args } if dynamic_call_with(name) => args
            .get(3)
            .map(|capabilities| resolved_expr_type(table, capabilities))
            .transpose()?
            .flatten(),
        _ => None,
    };
    match expr {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                rewrite_dynamic_expr(item, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Call { args, .. } => {
            for argument in args {
                rewrite_dynamic_expr(argument, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            rewrite_dynamic_expr(receiver, module, table, runtime_catalog, runtime_types)?;
            for argument in args {
                rewrite_dynamic_expr(argument, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                rewrite_dynamic_expr(argument, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Apply { func, args } => {
            rewrite_dynamic_expr(func, module, table, runtime_catalog, runtime_types)?;
            for argument in args {
                rewrite_dynamic_expr(argument, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => {
            rewrite_dynamic_expr(expr, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::Lambda { body, .. } | Expr::Block(body) => {
            rewrite_dynamic_block(body, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewrite_dynamic_expr(base, module, table, runtime_catalog, runtime_types)?;
            for (_, value) in fields {
                rewrite_dynamic_expr(value, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_dynamic_expr(value, module, table, runtime_catalog, runtime_types)?;
            }
            if let Some(spread) = spread {
                rewrite_dynamic_expr(spread, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_dynamic_expr(lhs, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_expr(rhs, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::If { cond, then_block, else_block } => {
            rewrite_dynamic_expr(cond, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_block(then_block, module, table, runtime_catalog, runtime_types)?;
            if let Some(else_block) = else_block {
                rewrite_dynamic_block(else_block, module, table, runtime_catalog, runtime_types)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_dynamic_expr(scrutinee, module, table, runtime_catalog, runtime_types)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewrite_dynamic_expr(guard, module, table, runtime_catalog, runtime_types)?;
                }
                rewrite_dynamic_expr(
                    &mut arm.body,
                    module,
                    table,
                    runtime_catalog,
                    runtime_types,
                )?;
            }
        }
        Expr::While { cond, body } => {
            rewrite_dynamic_expr(cond, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_block(body, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_dynamic_expr(iter, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_block(body, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_dynamic_expr(lo, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_expr(hi, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::Index { base, index } => {
            rewrite_dynamic_expr(base, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_expr(index, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_dynamic_expr(scrutinee, module, table, runtime_catalog, runtime_types)?;
            rewrite_dynamic_block(body, module, table, runtime_catalog, runtime_types)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }

    if let Some(ty) = request {
        let runtime_catalog = runtime_catalog.ok_or_else(|| {
            "Dynamic operations require authenticated runtime declaration ownership".to_string()
        })?;
        let identity = runtime_catalog
            .type_identity(&ty)
            .map_err(|error| error.to_string())?;
        let descriptor = runtime_types.id(&identity).ok_or_else(|| {
            "Dynamic descriptor plan lost a prepared concrete type".to_string()
        })?;
        match expr {
            Expr::Call { name, args }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_RUNTIME_TYPE) =>
            {
                let display = match args.as_slice() {
                    [Expr::Str(_), Expr::Str(source)] => source.clone(),
                    _ => format::type_str(&ty),
                };
                *expr = Expr::Ctor {
                    name: "dynamic.RuntimeType".into(),
                    args: vec![
                        Expr::Int(i64::from(descriptor.index())),
                        Expr::Str(display),
                    ],
                };
            }
            Expr::Call { name, .. }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_DESCRIPTOR) =>
            {
                *expr = Expr::Ctor {
                    name: "dynamic.RuntimeType".into(),
                    args: vec![
                        Expr::Int(i64::from(descriptor.index())),
                        Expr::Str(format::type_str(&ty)),
                    ],
                };
            }
            Expr::Call { name, .. }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_DESCRIPTOR_ID) =>
            {
                *expr = Expr::Int(i64::from(descriptor.index()));
            }
            Expr::Call { name, args }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_TRY_DECODE) =>
            {
                *name = intrinsics::DYNAMIC_TRY_DECODE_TYPED.into();
                args.push(Expr::Int(i64::from(descriptor.index())));
            }
            _ => {}
        }
    } else if runtime_catalog.is_none()
        && matches!(
            expr,
            Expr::Call { name, .. }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_DESCRIPTOR_ID)
        )
    {
        // Derive(Reflect) installs this compiler-private helper on every
        // reflected record. Legacy raw-AST runners have no loader provenance,
        // but must not reject a program merely because the dormant helper is
        // present. The invalid sentinel cannot authenticate a field payload;
        // public Dynamic construction still requires a runtime catalog above.
        *expr = Expr::Int(-1);
    }
    let fields_lookup = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_FIELDS)
    );
    let field_status_lookup = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_FIELD_STATUS)
    );
    let methods_lookup = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_METHODS)
    );
    let method_call = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_CALL)
    );
    let method_call_with = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_call_with(name)
    );
    let trait_query = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_IMPLEMENTS)
    );
    let trait_cast = matches!(
        expr,
        Expr::Call { name, .. } if dynamic_intrinsic(name, intrinsics::DYNAMIC_AS_TRAIT)
    );
    if fields_lookup {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [ty] = std::mem::take(args).try_into().map_err(|_| {
            "Dynamic field enumeration requires one runtime descriptor".to_string()
        })?;
        *expr = runtime_fields_lookup(ty, runtime_types);
    } else if field_status_lookup {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [ty, field] = std::mem::take(args)
            .try_into()
            .map_err(|_| "Dynamic field lookup requires a descriptor and name".to_string())?;
        *expr = runtime_field_status_lookup(ty, field, runtime_types);
    } else if methods_lookup {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [ty] = std::mem::take(args)
            .try_into()
            .map_err(|_| "Dynamic method enumeration requires one descriptor".to_string())?;
        *expr = runtime_methods_lookup(ty, runtime_types);
    } else if method_call {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [receiver, name, arguments] = std::mem::take(args)
            .try_into()
            .map_err(|_| "Dynamic method call requires a receiver, name, and arguments".to_string())?;
        *expr = runtime_method_call(receiver, name, arguments, None, runtime_types);
    } else if method_call_with {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [receiver, name, arguments, capabilities] = std::mem::take(args)
            .try_into()
            .map_err(|_| {
                "Dynamic method call with capabilities requires a receiver, name, arguments, and capability bundle"
                    .to_string()
            })?;
        *expr = runtime_method_call(
            receiver,
            name,
            arguments,
            Some((capabilities, call_with_capability_type)),
            runtime_types,
        );
    } else if trait_query || trait_cast {
        let Expr::Call { args, .. } = expr else { unreachable!() };
        let [value, trait_type] = std::mem::take(args)
            .try_into()
            .map_err(|_| "Dynamic trait queries require a value and trait descriptor".to_string())?;
        *expr = runtime_trait_query(value, trait_type, trait_cast, runtime_types);
    }
    Ok(())
}

fn runtime_type_expr(id: crate::runtime_type::RuntimeTypeId, display: &str) -> Expr {
    Expr::Ctor {
        name: "dynamic.RuntimeType".into(),
        args: vec![Expr::Int(i64::from(id.index())), Expr::Str(display.into())],
    }
}

fn runtime_trait_query(
    value: Expr,
    trait_type: Expr,
    checked_view: bool,
    runtime_types: &RuntimeTypePlan,
) -> Expr {
    let mut arms = runtime_types
        .trait_relations()
        .iter()
        .map(|(concrete, trait_id)| witchy_syntax::ast::MatchArm {
            line: u32::MAX,
            pattern: witchy_syntax::ast::Pattern::Tuple(vec![
                witchy_syntax::ast::Pattern::Ctor {
                    name: "dynamic.RuntimeType".into(),
                    args: vec![
                        witchy_syntax::ast::Pattern::Int(i64::from(concrete.index())),
                        witchy_syntax::ast::Pattern::Wildcard,
                    ],
                },
                witchy_syntax::ast::Pattern::Ctor {
                    name: "dynamic.RuntimeType".into(),
                    args: vec![
                        witchy_syntax::ast::Pattern::Int(i64::from(trait_id.index())),
                        witchy_syntax::ast::Pattern::Wildcard,
                    ],
                },
            ]),
            guard: None,
            body: if checked_view {
                Expr::Ctor {
                    name: "Ok".into(),
                    args: vec![Expr::Var("$dynamic_trait_value".into())],
                }
            } else {
                Expr::Bool(true)
            },
        })
        .collect::<Vec<_>>();
    arms.push(witchy_syntax::ast::MatchArm {
        line: u32::MAX,
        pattern: witchy_syntax::ast::Pattern::Wildcard,
        guard: None,
        body: if checked_view {
            dynamic_error(
                "TraitMismatch",
                vec![Expr::Var("$dynamic_trait_type".into())],
            )
        } else {
            Expr::Bool(false)
        },
    });
    Expr::Block(Block {
        stmts: vec![
            Stmt::Let {
                name: "$dynamic_trait_value".into(),
                ty: None,
                value,
                mutable: false,
            },
            Stmt::Let {
                name: "$dynamic_trait_type".into(),
                ty: None,
                value: trait_type,
                mutable: false,
            },
            Stmt::Expr(Expr::Match {
                scrutinee: Box::new(Expr::Tuple(vec![
                    dynamic_type_of("$dynamic_trait_value"),
                    Expr::Var("$dynamic_trait_type".into()),
                ])),
                arms,
            }),
        ],
        lines: vec![u32::MAX; 3],
        region: None,
    })
}

fn runtime_fields_lookup(ty: Expr, runtime_types: &RuntimeTypePlan) -> Expr {
    let mut descriptor_arms = runtime_types
        .descriptors()
        .iter()
        .map(|descriptor| {
            let fields = match runtime_types.shape(descriptor.id) {
                Some(RuntimeTypeShape::Record(fields)) => fields
                    .iter()
                    .map(|field| Expr::Ctor {
                        name: "dynamic.RuntimeField".into(),
                        args: vec![
                            Expr::Str(field.name.clone()),
                            runtime_type_expr(field.descriptor, &field.display),
                        ],
                    })
                    .collect(),
                Some(RuntimeTypeShape::Opaque | RuntimeTypeShape::Sealed) | None => Vec::new(),
            };
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::Int(i64::from(descriptor.id.index())),
                guard: None,
                body: Expr::List(fields),
            }
        })
        .collect::<Vec<_>>();
    descriptor_arms.push(witchy_syntax::ast::MatchArm {
        line: u32::MAX,
        pattern: witchy_syntax::ast::Pattern::Wildcard,
        guard: None,
        body: Expr::List(Vec::new()),
    });
    Expr::Match {
        scrutinee: Box::new(ty),
        arms: vec![witchy_syntax::ast::MatchArm {
            line: u32::MAX,
            pattern: witchy_syntax::ast::Pattern::Ctor {
                name: "dynamic.RuntimeType".into(),
                args: vec![
                    witchy_syntax::ast::Pattern::Var("$dynamic_descriptor".into()),
                    witchy_syntax::ast::Pattern::Wildcard,
                ],
            },
            guard: None,
            body: Expr::Match {
                scrutinee: Box::new(Expr::Var("$dynamic_descriptor".into())),
                arms: descriptor_arms,
            },
        }],
    }
}

fn dynamic_error(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Ctor {
        name: "Err".into(),
        args: vec![Expr::Ctor {
            name: format!("dynamic.{name}"),
            args,
        }],
    }
}

fn runtime_methods_lookup(ty: Expr, runtime_types: &RuntimeTypePlan) -> Expr {
    let mut by_receiver = BTreeMap::<u32, Vec<&RuntimeMethodDescriptor>>::new();
    for method in runtime_types.methods() {
        by_receiver
            .entry(method.receiver.index())
            .or_default()
            .push(method);
    }
    let mut descriptor_arms = runtime_types
        .descriptors()
        .iter()
        .map(|descriptor| {
            let methods = by_receiver
                .get(&descriptor.id.index())
                .into_iter()
                .flatten()
                .map(|method| Expr::Ctor {
                    name: "dynamic.RuntimeMethod".into(),
                    args: vec![
                        Expr::Str(method.name.clone()),
                        Expr::List(
                            method
                                .parameters
                                .iter()
                                .filter_map(|parameter| match parameter {
                                    RuntimeMethodParameterDescriptor::Value(argument) => Some(
                                        runtime_type_expr(argument.descriptor, &argument.display),
                                    ),
                                    RuntimeMethodParameterDescriptor::Capability(_) => None,
                                })
                                .collect(),
                        ),
                        runtime_type_expr(method.result, &method.result_display),
                        Expr::List(
                            method
                                .parameters
                                .iter()
                                .filter_map(|parameter| match parameter {
                                    RuntimeMethodParameterDescriptor::Value(_) => None,
                                    RuntimeMethodParameterDescriptor::Capability(capability) => {
                                        Some(Expr::Str(capability.display.clone()))
                                    }
                                })
                                .collect(),
                        ),
                        runtime_callable_access_expr(
                            &method.access,
                            &method.borrow_relation_storage_displays,
                            runtime_types,
                        ),
                    ],
                })
                .collect();
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::Int(i64::from(descriptor.id.index())),
                guard: None,
                body: Expr::List(methods),
            }
        })
        .collect::<Vec<_>>();
    descriptor_arms.push(witchy_syntax::ast::MatchArm {
        line: u32::MAX,
        pattern: witchy_syntax::ast::Pattern::Wildcard,
        guard: None,
        body: Expr::List(Vec::new()),
    });
    Expr::Match {
        scrutinee: Box::new(ty),
        arms: vec![witchy_syntax::ast::MatchArm {
            line: u32::MAX,
            pattern: witchy_syntax::ast::Pattern::Ctor {
                name: "dynamic.RuntimeType".into(),
                args: vec![
                    witchy_syntax::ast::Pattern::Var("$dynamic_method_descriptor".into()),
                    witchy_syntax::ast::Pattern::Wildcard,
                ],
            },
            guard: None,
            body: Expr::Match {
                scrutinee: Box::new(Expr::Var("$dynamic_method_descriptor".into())),
                arms: descriptor_arms,
            },
        }],
    }
}

fn runtime_access_qualifier_expr(qualifier: &crate::runtime_type::RuntimeAccessQualifier) -> Expr {
    use crate::runtime_type::RuntimeAccessQualifier as Qualifier;
    let (name, args) = match qualifier {
        Qualifier::Frozen => ("dynamic.AccessFrozen", Vec::new()),
        Qualifier::Unique => ("dynamic.AccessUnique", Vec::new()),
        Qualifier::LocalUnique => ("dynamic.AccessLocalUnique", Vec::new()),
        Qualifier::Borrow(lifetime) => (
            "dynamic.AccessView",
            vec![Expr::Int(i64::from(*lifetime))],
        ),
    };
    Expr::Ctor { name: name.into(), args }
}

fn runtime_qualifier_path_step_expr(
    step: &crate::runtime_type::RuntimeQualifierPathStep,
) -> Expr {
    use crate::runtime_type::RuntimeQualifierPathStep as Step;
    let index = |index: usize| Expr::Int(i64::try_from(index).unwrap_or(i64::MAX));
    let (name, args) = match step {
        Step::TypeArgument(at) => ("dynamic.AccessTypeArgument", vec![index(*at)]),
        Step::TupleItem(at) => ("dynamic.AccessTupleItem", vec![index(*at)]),
        Step::FunctionParameter(at) => {
            ("dynamic.AccessFunctionParameter", vec![index(*at)])
        }
        Step::FunctionResult => ("dynamic.AccessFunctionResult", Vec::new()),
        Step::ExistentialArgument(at) => {
            ("dynamic.AccessExistentialArgument", vec![index(*at)])
        }
        Step::RecordBase => ("dynamic.AccessRecordBase", Vec::new()),
        Step::RecordField(field) => {
            ("dynamic.AccessRecordField", vec![Expr::Str(field.clone())])
        }
    };
    Expr::Ctor { name: name.into(), args }
}

fn runtime_qualifier_site_expr(site: &crate::runtime_type::RuntimeQualifierSite) -> Expr {
    Expr::Ctor {
        name: "dynamic.RuntimeQualifierSite".into(),
        args: vec![
            Expr::List(
                site.path
                    .iter()
                    .map(runtime_qualifier_path_step_expr)
                    .collect(),
            ),
            Expr::List(
                site.qualifiers
                    .iter()
                    .map(runtime_access_qualifier_expr)
                    .collect(),
            ),
        ],
    }
}

fn runtime_loan_projection_step_expr(
    step: &crate::runtime_type::RuntimeLoanProjectionStep,
) -> Expr {
    use crate::runtime_type::RuntimeLoanProjectionStep as Step;
    let index = |index: usize| Expr::Int(i64::try_from(index).unwrap_or(i64::MAX));
    let (name, args) = match step {
        Step::Field(field) => ("dynamic.AccessField", vec![Expr::Str(field.clone())]),
        Step::Tuple(at) => ("dynamic.AccessTuple", vec![index(*at)]),
        Step::Index(at) => ("dynamic.AccessIndex", vec![Expr::Int(*at)]),
        Step::Range { lo, hi, inclusive } => (
            "dynamic.AccessRange",
            vec![Expr::Int(*lo), Expr::Int(*hi), Expr::Bool(*inclusive)],
        ),
    };
    Expr::Ctor { name: name.into(), args }
}

fn runtime_projection_expr(projection: &crate::runtime_type::RuntimeLoanProjection) -> Expr {
    Expr::List(
        projection
            .steps
            .iter()
            .map(runtime_loan_projection_step_expr)
            .collect(),
    )
}

fn runtime_callable_access_expr(
    access: &crate::runtime_type::RuntimeCallableAccessIdentity,
    storage_displays: &[String],
    runtime_types: &RuntimeTypePlan,
) -> Expr {
    use crate::runtime_type::RuntimeAccessKind as Kind;
    let callable_qualifiers = Expr::List(
        access
            .callable_qualifiers
            .iter()
            .map(runtime_access_qualifier_expr)
            .collect(),
    );
    let parameters = Expr::List(
        access
            .parameters
            .iter()
            .map(|parameter| {
                let kind = match parameter.kind {
                    Kind::OwnedImmutable => "dynamic.AccessValue",
                    Kind::SharedBorrow => "dynamic.AccessBorrow",
                    Kind::ExclusiveWriteback => "dynamic.AccessVar",
                    Kind::Consuming => "dynamic.AccessOwn",
                };
                Expr::Ctor {
                    name: "dynamic.RuntimeParameterAccess".into(),
                    args: vec![
                        Expr::Ctor { name: kind.into(), args: Vec::new() },
                        Expr::List(
                            parameter
                                .qualifier_sites
                                .iter()
                                .map(runtime_qualifier_site_expr)
                                .collect(),
                        ),
                        Expr::Bool(parameter.ownership_input),
                        Expr::Bool(parameter.writeback_output),
                    ],
                }
            })
            .collect(),
    );
    let result = Expr::Ctor {
        name: "dynamic.RuntimeResultAccess".into(),
        args: vec![
            Expr::List(
                access
                    .result
                    .qualifier_sites
                    .iter()
                    .map(runtime_qualifier_site_expr)
                    .collect(),
            ),
            Expr::Bool(access.result.ownership_output),
        ],
    };
    let relations = Expr::List(
        access
            .borrow_relations
            .iter()
            .enumerate()
            .map(|(index, relation)| {
                let storage = runtime_types
                    .id(&relation.storage)
                    .expect("runtime callable plan retains relation storage identity");
                let storage_display = storage_displays
                    .get(index)
                    .expect("runtime callable storage displays align with relations");
                Expr::Ctor {
                    name: "dynamic.RuntimeBorrowRelation".into(),
                    args: vec![
                        Expr::Int(i64::from(relation.lifetime)),
                        runtime_projection_expr(&relation.output_projection),
                        Expr::List(
                            relation
                                .owners
                                .iter()
                                .map(|owner| Expr::Ctor {
                                    name: "dynamic.RuntimeBorrowOwner".into(),
                                    args: vec![
                                        Expr::Int(
                                            i64::try_from(owner.parameter).unwrap_or(i64::MAX),
                                        ),
                                        runtime_projection_expr(&owner.input_projection),
                                    ],
                                })
                                .collect(),
                        ),
                        runtime_type_expr(storage, storage_display),
                        Expr::List(
                            relation
                                .storage_qualifier_sites
                                .iter()
                                .map(runtime_qualifier_site_expr)
                                .collect(),
                        ),
                    ],
                }
            })
            .collect(),
    );
    Expr::Ctor {
        name: "dynamic.RuntimeCallableAccess".into(),
        args: vec![callable_qualifiers, parameters, result, relations],
    }
}

fn dynamic_type_of(value: &str) -> Expr {
    Expr::Call {
        name: "dynamic.type_of".into(),
        args: vec![Expr::Var(value.into())],
    }
}

fn dynamic_decode_step(
    value: &str,
    decoded: &str,
    ty: &Type,
    descriptor: crate::runtime_type::RuntimeTypeId,
    success: Expr,
    failure: Expr,
) -> Expr {
    let option = Type::Named("Option".into(), vec![ty.clone()]);
    Expr::Block(Block {
        stmts: vec![
            Stmt::Let {
                name: format!("{decoded}_option"),
                ty: Some(option),
                value: Expr::Call {
                    name: intrinsics::DYNAMIC_TRY_DECODE_TYPED.into(),
                    args: vec![
                        Expr::Var(value.into()),
                        Expr::Int(i64::from(descriptor.index())),
                    ],
                },
                mutable: false,
            },
            Stmt::Expr(Expr::Match {
                scrutinee: Box::new(Expr::Var(format!("{decoded}_option"))),
                arms: vec![
                    witchy_syntax::ast::MatchArm {
                        line: u32::MAX,
                        pattern: witchy_syntax::ast::Pattern::Ctor {
                            name: "Some".into(),
                            args: vec![witchy_syntax::ast::Pattern::Var(decoded.into())],
                        },
                        guard: None,
                        body: success,
                    },
                    witchy_syntax::ast::MatchArm {
                        line: u32::MAX,
                        pattern: witchy_syntax::ast::Pattern::Ctor {
                            name: "None".into(),
                            args: Vec::new(),
                        },
                        guard: None,
                        body: failure,
                    },
                ],
            }),
        ],
        lines: vec![u32::MAX, u32::MAX],
        region: None,
    })
}

fn capability_bundle_types(ty: &Type) -> Vec<Type> {
    match ty.unqualified() {
        Type::Tuple(types) => types.clone(),
        _ => vec![ty.clone()],
    }
}

fn runtime_method_invocation(
    method: &RuntimeMethodDescriptor,
    method_index: usize,
    with_capabilities: bool,
    capability_type: Option<&Type>,
) -> Expr {
    let receiver = "$dynamic_call_receiver";
    let self_name = format!("$dynamic_self_{method_index}");
    let arguments = method
        .parameters
        .iter()
        .filter_map(|parameter| match parameter {
            RuntimeMethodParameterDescriptor::Value(argument) => Some(argument),
            RuntimeMethodParameterDescriptor::Capability(_) => None,
        })
        .collect::<Vec<_>>();
    let capabilities = method
        .parameters
        .iter()
        .filter_map(|parameter| match parameter {
            RuntimeMethodParameterDescriptor::Value(_) => None,
            RuntimeMethodParameterDescriptor::Capability(capability) => Some(capability),
        })
        .collect::<Vec<_>>();
    let supplied_capabilities = capability_type.map(capability_bundle_types);
    let expected_capabilities = capabilities
        .iter()
        .map(|capability| capability.ty.clone())
        .collect::<Vec<_>>();
    if capabilities.is_empty() && with_capabilities
        || !capabilities.is_empty()
            && (!with_capabilities
                || supplied_capabilities.as_ref() != Some(&expected_capabilities))
    {
        return dynamic_error("CapabilityDenied", vec![Expr::Str(method.name.clone())]);
    }
    let argument_values = (0..arguments.len())
        .map(|index| format!("$dynamic_argument_{method_index}_{index}"))
        .collect::<Vec<_>>();
    let decoded_arguments = (0..arguments.len())
        .map(|index| format!("$dynamic_decoded_{method_index}_{index}"))
        .collect::<Vec<_>>();
    let capability_values = (0..capabilities.len())
        .map(|index| format!("$dynamic_capability_{method_index}_{index}"))
        .collect::<Vec<_>>();
    let mut call_arguments = vec![Expr::Var(self_name.clone())];
    let mut argument_index = 0usize;
    let mut capability_index = 0usize;
    for parameter in &method.parameters {
        match parameter {
            RuntimeMethodParameterDescriptor::Value(_) => {
                call_arguments.push(Expr::Var(decoded_arguments[argument_index].clone()));
                argument_index += 1;
            }
            RuntimeMethodParameterDescriptor::Capability(_) => {
                let value = if capabilities.len() == 1 {
                    "$dynamic_call_capabilities".to_string()
                } else {
                    capability_values[capability_index].clone()
                };
                call_arguments.push(Expr::Var(value));
                capability_index += 1;
            }
        }
    }
    let mut body = Expr::Ctor {
        name: "Ok".into(),
        args: vec![Expr::Ctor {
            name: "dynamic.Dynamic".into(),
            args: vec![
                runtime_type_expr(method.result, &method.result_display),
                Expr::Call {
                    name: method.function.clone(),
                    args: call_arguments,
                },
            ],
        }],
    };
    if capabilities.len() > 1 {
        body = Expr::Match {
            scrutinee: Box::new(Expr::Var("$dynamic_call_capabilities".into())),
            arms: vec![
                witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Tuple(
                        capability_values
                            .iter()
                            .cloned()
                            .map(witchy_syntax::ast::Pattern::Var)
                            .collect(),
                    ),
                    guard: None,
                    body,
                },
                witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Wildcard,
                    guard: None,
                    body: dynamic_error(
                        "CapabilityDenied",
                        vec![Expr::Str(method.name.clone())],
                    ),
                },
            ],
        };
    }
    for (index, argument) in arguments.iter().enumerate().rev() {
        let value = &argument_values[index];
        let actual_type = dynamic_type_of(value);
        let decode = dynamic_decode_step(
            value,
            &decoded_arguments[index],
            &argument.ty,
            argument.descriptor,
            body,
            dynamic_error("MalformedPayload", vec![actual_type.clone()]),
        );
        body = Expr::Match {
            scrutinee: Box::new(actual_type.clone()),
            arms: vec![
                witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Ctor {
                        name: "dynamic.RuntimeType".into(),
                        args: vec![
                            witchy_syntax::ast::Pattern::Int(i64::from(
                                argument.descriptor.index(),
                            )),
                            witchy_syntax::ast::Pattern::Wildcard,
                        ],
                    },
                    guard: None,
                    body: decode,
                },
                witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Wildcard,
                    guard: None,
                    body: dynamic_error(
                        "ArgumentMismatch",
                        vec![
                            Expr::Int(i64::try_from(index).unwrap_or(i64::MAX)),
                            runtime_type_expr(argument.descriptor, &argument.display),
                            actual_type,
                        ],
                    ),
                },
            ],
        };
    }
    body = dynamic_decode_step(
        receiver,
        &self_name,
        &method.receiver_type,
        method.receiver,
        body,
        dynamic_error("MalformedPayload", vec![dynamic_type_of(receiver)]),
    );
    Expr::Match {
        scrutinee: Box::new(Expr::Var("$dynamic_call_arguments".into())),
        arms: vec![
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::List {
                    elems: argument_values
                        .iter()
                        .cloned()
                        .map(witchy_syntax::ast::Pattern::Var)
                        .collect(),
                    rest: None,
                },
                guard: None,
                body,
            },
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::Wildcard,
                guard: None,
                body: dynamic_error(
                    "ArityMismatch",
                    vec![
                        Expr::Str(method.name.clone()),
                        Expr::Int(i64::try_from(arguments.len()).unwrap_or(i64::MAX)),
                        Expr::Call {
                            name: "list.length".into(),
                            args: vec![Expr::Var("$dynamic_call_arguments".into())],
                        },
                    ],
                ),
            },
        ],
    }
}

fn runtime_method_call(
    receiver: Expr,
    name: Expr,
    arguments: Expr,
    capabilities: Option<(Expr, Option<Type>)>,
    runtime_types: &RuntimeTypePlan,
) -> Expr {
    let mut by_receiver = BTreeMap::<u32, Vec<(usize, &RuntimeMethodDescriptor)>>::new();
    for (index, method) in runtime_types.methods().iter().enumerate() {
        by_receiver
            .entry(method.receiver.index())
            .or_default()
            .push((index, method));
    }
    let descriptor_value = dynamic_type_of("$dynamic_call_receiver");
    let mut descriptor_arms = runtime_types
        .descriptors()
        .iter()
        .map(|descriptor| {
            let methods = by_receiver.get(&descriptor.id.index());
            let body = if let Some(methods) = methods {
                let mut method_arms = methods
                    .iter()
                    .map(|(index, method)| witchy_syntax::ast::MatchArm {
                        line: u32::MAX,
                        pattern: witchy_syntax::ast::Pattern::Str(method.name.clone()),
                        guard: None,
                        body: runtime_method_invocation(
                            method,
                            *index,
                            capabilities.is_some(),
                            capabilities
                                .as_ref()
                                .and_then(|(_, capability_type)| capability_type.as_ref()),
                        ),
                    })
                    .collect::<Vec<_>>();
                method_arms.push(witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Wildcard,
                    guard: None,
                    body: dynamic_error(
                        "MissingMethod",
                        vec![Expr::Var("$dynamic_call_name".into())],
                    ),
                });
                Expr::Match {
                    scrutinee: Box::new(Expr::Var("$dynamic_call_name".into())),
                    arms: method_arms,
                }
            } else {
                dynamic_error(
                    "MissingMethod",
                    vec![Expr::Var("$dynamic_call_name".into())],
                )
            };
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::Ctor {
                    name: "dynamic.RuntimeType".into(),
                    args: vec![
                        witchy_syntax::ast::Pattern::Int(i64::from(descriptor.id.index())),
                        witchy_syntax::ast::Pattern::Wildcard,
                    ],
                },
                guard: None,
                body,
            }
        })
        .collect::<Vec<_>>();
    descriptor_arms.push(witchy_syntax::ast::MatchArm {
        line: u32::MAX,
        pattern: witchy_syntax::ast::Pattern::Wildcard,
        guard: None,
        body: dynamic_error("MalformedDescriptor", vec![descriptor_value.clone()]),
    });
    let mut stmts = vec![
            Stmt::Let {
                name: "$dynamic_call_receiver".into(),
                ty: None,
                value: receiver,
                mutable: false,
            },
            Stmt::Let {
                name: "$dynamic_call_name".into(),
                ty: None,
                value: name,
                mutable: false,
            },
            Stmt::Let {
                name: "$dynamic_call_arguments".into(),
                ty: None,
                value: arguments,
                mutable: false,
            },
        ];
    if let Some((capabilities, _)) = capabilities {
        stmts.push(Stmt::Let {
            name: "$dynamic_call_capabilities".into(),
            ty: None,
            value: capabilities,
            mutable: false,
        });
    }
    stmts.push(Stmt::Expr(Expr::Match {
        scrutinee: Box::new(descriptor_value),
        arms: descriptor_arms,
    }));
    let lines = vec![u32::MAX; stmts.len()];
    Expr::Block(Block {
        stmts,
        lines,
        region: None,
    })
}

fn runtime_field_status_lookup(
    ty: Expr,
    field_name: Expr,
    runtime_types: &RuntimeTypePlan,
) -> Expr {
    let status = |name: &str, args: Vec<Expr>| Expr::Ctor {
        name: format!("dynamic.{name}"),
        args,
    };
    let mut descriptor_arms = runtime_types
        .descriptors()
        .iter()
        .map(|descriptor| {
            let body = match runtime_types.shape(descriptor.id) {
                Some(RuntimeTypeShape::Record(fields)) => {
                    let mut field_arms = fields
                        .iter()
                        .map(|field| witchy_syntax::ast::MatchArm {
                            line: u32::MAX,
                            pattern: witchy_syntax::ast::Pattern::Str(field.name.clone()),
                            guard: None,
                            body: status(
                                "FieldFound",
                                vec![runtime_type_expr(field.descriptor, &field.display)],
                            ),
                        })
                        .collect::<Vec<_>>();
                    field_arms.push(witchy_syntax::ast::MatchArm {
                        line: u32::MAX,
                        pattern: witchy_syntax::ast::Pattern::Wildcard,
                        guard: None,
                        body: status("FieldMissing", Vec::new()),
                    });
                    Expr::Match {
                        scrutinee: Box::new(Expr::Var("$dynamic_field".into())),
                        arms: field_arms,
                    }
                }
                Some(RuntimeTypeShape::Sealed) => status("FieldSealed", Vec::new()),
                Some(RuntimeTypeShape::Opaque) | None => status("FieldMissing", Vec::new()),
            };
            witchy_syntax::ast::MatchArm {
                line: u32::MAX,
                pattern: witchy_syntax::ast::Pattern::Int(i64::from(descriptor.id.index())),
                guard: None,
                body,
            }
        })
        .collect::<Vec<_>>();
    descriptor_arms.push(witchy_syntax::ast::MatchArm {
        line: u32::MAX,
        pattern: witchy_syntax::ast::Pattern::Wildcard,
        guard: None,
        body: status("FieldMalformed", Vec::new()),
    });
    Expr::Match {
        scrutinee: Box::new(ty),
        arms: vec![witchy_syntax::ast::MatchArm {
            line: u32::MAX,
            pattern: witchy_syntax::ast::Pattern::Ctor {
                name: "dynamic.RuntimeType".into(),
                args: vec![
                    witchy_syntax::ast::Pattern::Var("$dynamic_descriptor".into()),
                    witchy_syntax::ast::Pattern::Wildcard,
                ],
            },
            guard: None,
            body: Expr::Match {
                scrutinee: Box::new(field_name),
                arms: vec![witchy_syntax::ast::MatchArm {
                    line: u32::MAX,
                    pattern: witchy_syntax::ast::Pattern::Var("$dynamic_field".into()),
                    guard: None,
                    body: Expr::Match {
                        scrutinee: Box::new(Expr::Var("$dynamic_descriptor".into())),
                        arms: descriptor_arms,
                    },
                }],
            },
        }],
    }
}
