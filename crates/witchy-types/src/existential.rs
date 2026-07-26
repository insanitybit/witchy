//! RFC-0081 compiler-owned existential construction.
//!
//! This pass consumes a typed source AST after checking has selected concrete
//! payload types. It builds the deterministic closed-program witness plan and
//! replaces every directed concrete-to-`dyn Trait` coercion with a typed,
//! source-unspellable AST node. Backends therefore consume one construction
//! contract without rediscovering coercions, impl identity, or runtime types.

use std::collections::{BTreeMap, BTreeSet};

use witchy_syntax::ast::{Block, Expr, Item, Module, Stmt, Type};
use witchy_syntax::{format, intrinsics};

use crate::runtime_type::{
    RuntimeDeclarationCatalog, RuntimeMethodArgumentDescriptor,
    RuntimeMethodCapabilityDescriptor, RuntimeMethodDescriptor,
    RuntimeMethodParameterDescriptor, RuntimeTypeError, RuntimeTypePlan, RuntimeTypeShape,
};
use crate::typeck::{TypeTable, TypedModule, ty_to_ast};
use crate::witness::{self, WitnessCatalog, WitnessPlan};

type ExistentialRequest = (Type, Type);
type CollectedRequests = (Vec<ExistentialRequest>, Vec<ExistentialRequest>);

pub struct PreparedExistentials {
    module: Module,
    table: TypeTable,
    witnesses: WitnessPlan,
    runtime_types: RuntimeTypePlan,
}

impl std::fmt::Debug for PreparedExistentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedExistentials")
            .field("module", &self.module)
            .field("witnesses", &self.witnesses)
            .finish_non_exhaustive()
    }
}

impl PreparedExistentials {
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub fn witnesses(&self) -> &WitnessPlan {
        &self.witnesses
    }

    pub fn runtime_types(&self) -> &RuntimeTypePlan {
        &self.runtime_types
    }

    /// Type facts for the exact payload expressions retained inside compiler-owned
    /// packs. Backends use these to choose concrete payload field kinds.
    pub fn table(&self) -> &TypeTable {
        &self.table
    }

    pub fn into_parts(self) -> (Module, TypeTable, WitnessPlan) {
        (self.module, self.table, self.witnesses)
    }

}

/// Lower every concrete-to-existential construction in one final typed module.
///
/// This function is the shared internal seam both backends consume. `typed`
/// must be the exact, trait-lowered and
/// monomorphized executable module whose expression identities produced the
/// type table. `catalog` is captured from the resolved linked module before
/// trait lowering removes its declarations.
pub fn lower_explicit_packs(
    typed: TypedModule,
    catalog: &WitnessCatalog,
) -> Result<PreparedExistentials, String> {
    lower_explicit_packs_inner(typed, catalog, None)
}

pub fn lower_explicit_packs_with_runtime_types(
    typed: TypedModule,
    catalog: &WitnessCatalog,
    runtime_catalog: &RuntimeDeclarationCatalog,
) -> Result<PreparedExistentials, String> {
    lower_explicit_packs_inner(typed, catalog, Some(runtime_catalog))
}

fn lower_explicit_packs_inner(
    typed: TypedModule,
    catalog: &WitnessCatalog,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<PreparedExistentials, String> {
    if typed
        .module()
        .items
        .iter()
        .any(|item| matches!(item, Item::Trait(_) | Item::Impl(_)))
    {
        return Err(
            "existential preparation requires a trait-lowered, monomorphic executable module"
                .to_string(),
        );
    }
    let typed = crate::record_projection::lower_explicit_projections(typed)?;
    let dynamic_methods = collect_dynamic_method_definitions(typed.module(), runtime_catalog)?;
    let mut dynamic_types = collect_dynamic_types(typed.module(), typed.table(), runtime_catalog)?;
    for method in &dynamic_methods {
        dynamic_types.push(method.receiver.clone());
        dynamic_types.extend(method.parameters.iter().filter_map(|parameter| match parameter {
            DynamicMethodParameterDefinition::Value(ty) => Some(ty.clone()),
            DynamicMethodParameterDefinition::Capability(_) => None,
        }));
        dynamic_types.push(method.result.clone());
    }
    let mut runtime_types = match runtime_catalog {
        Some(runtime_catalog) => RuntimeTypePlan::build_with_runtime_shapes(
            dynamic_types.iter(),
            runtime_catalog,
            typed.module(),
        ),
        None => RuntimeTypePlan::build(Vec::new()),
    }
    .map_err(|error| error.to_string())?;
    if let Some(runtime_catalog) = runtime_catalog {
        runtime_types.set_methods(prepare_dynamic_methods(
            dynamic_methods,
            runtime_catalog,
            &runtime_types,
        )?);
        runtime_types.set_trait_relations(prepare_dynamic_trait_relations(
            &dynamic_types,
            catalog,
            runtime_catalog,
            &runtime_types,
        )?);
    }
    let (module, _, dynamic_result) = typed.rewrite_into_module(|table, module| {
        rewrite_dynamic_module(module, table, runtime_catalog, &runtime_types)
    });
    dynamic_result?;
    let typed = crate::typeck::annotate(module);
    let (requests, upcasts) = collect_requests(typed.module(), typed.table())?;
    let witnesses = witness::build_from_catalog_with_upcasts(catalog, requests, upcasts)?;
    let (module, table, result) = typed.rewrite_into_module(|table, module| {
        rewrite_module(module, table, &witnesses)
    });
    result?;
    Ok(PreparedExistentials { module, table, witnesses, runtime_types })
}

#[derive(Clone, Debug)]
struct DynamicMethodDefinition {
    name: String,
    function: String,
    receiver: Type,
    parameters: Vec<DynamicMethodParameterDefinition>,
    result: Type,
}

#[derive(Clone, Debug)]
enum DynamicMethodParameterDefinition {
    Value(Type),
    Capability(Type),
}

fn collect_dynamic_method_definitions(
    module: &Module,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
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
        });
    }
    Ok(methods)
}

fn prepare_dynamic_methods(
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
            Ok(RuntimeMethodDescriptor {
                receiver,
                receiver_type: method.receiver,
                name: method.name,
                function: method.function,
                parameters,
                result,
                result_display: witchy_syntax::format::type_str(&method.result),
                result_type: method.result,
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

fn collect_dynamic_types(
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

fn prepare_dynamic_trait_relations(
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

fn rewrite_dynamic_module(
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

fn pack_request(table: &TypeTable, expr: &Expr) -> Result<Option<ExistentialRequest>, String> {
    let Some((existential, concrete)) = table.existential_pack(expr) else {
        return Ok(None);
    };
    let existential = ty_to_ast(existential).ok_or_else(|| {
        "existential construction requires one fully resolved target type".to_string()
    })?;
    let concrete = ty_to_ast(concrete)
        .ok_or_else(|| {
            "existential construction requires one fully resolved concrete payload type"
                .to_string()
        })?;
    Ok(Some((
        existential.unqualified().clone(),
        concrete.unqualified().clone(),
    )))
}

fn upcast_request(table: &TypeTable, expr: &Expr) -> Result<Option<ExistentialRequest>, String> {
    let Some((target, source)) = table.existential_upcast(expr) else {
        return Ok(None);
    };
    let target = ty_to_ast(target).ok_or_else(|| {
        "existential supertrait conversion requires one fully resolved target type".to_string()
    })?;
    let source = ty_to_ast(source).ok_or_else(|| {
        "existential supertrait conversion requires one fully resolved source type".to_string()
    })?;
    Ok(Some((
        target.unqualified().clone(),
        source.unqualified().clone(),
    )))
}

fn collect_requests(
    module: &Module,
    table: &TypeTable,
) -> Result<CollectedRequests, String> {
    let mut requests = Vec::new();
    let mut upcasts = Vec::new();
    visit_module_exprs(module, &mut |expr| {
        if let Some(request) = pack_request(table, expr)? {
            requests.push(request);
        }
        if let Some(upcast) = upcast_request(table, expr)? {
            upcasts.push(upcast);
        }
        Ok(())
    })?;
    Ok((requests, upcasts))
}

fn type_contains_unresolved_variable(ty: &Type) -> bool {
    match ty {
        Type::Named(name, args) | Type::Dyn(name, args) => {
            (args.is_empty()
                && name
                    .rsplit('.')
                    .next()
                    .and_then(|name| name.chars().next())
                    .is_some_and(char::is_lowercase))
                || args.iter().any(type_contains_unresolved_variable)
        }
        Type::Tuple(items) => items.iter().any(type_contains_unresolved_variable),
        Type::Fn(params, result, _) => {
            params.iter().any(type_contains_unresolved_variable)
                || type_contains_unresolved_variable(result)
        }
        Type::Qualified(_, inner) => type_contains_unresolved_variable(inner),
        Type::RecordCompose { base, fields } => {
            type_contains_unresolved_variable(base)
                || fields
                    .iter()
                    .any(|(_, field)| type_contains_unresolved_variable(field))
        }
    }
}

/// Concrete existential constructions visible before generic templates are
/// discarded. Trait monomorphization uses these requests to materialize the
/// adapter functions that the later closed witness plan will reference.
pub(crate) fn concrete_pack_requests(
    module: &Module,
    table: &TypeTable,
) -> Result<Vec<ExistentialRequest>, String> {
    let (requests, _) = collect_requests(module, table)?;
    Ok(requests
        .into_iter()
        .filter(|(existential, concrete)| {
            !type_contains_unresolved_variable(existential)
                && !type_contains_unresolved_variable(concrete)
        })
        .collect())
}

fn rewrite_module(
    module: &mut Module,
    table: &TypeTable,
    witnesses: &WitnessPlan,
) -> Result<(), String> {
    for item in &mut module.items {
        match item {
            Item::Function(function) => rewrite_block(&mut function.body, table, witnesses)?,
            Item::Trait(definition) => {
                for method in &mut definition.methods {
                    if let Some(default) = &mut method.default {
                        rewrite_block(default, table, witnesses)?;
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &mut definition.methods {
                    rewrite_block(&mut method.body, table, witnesses)?;
                }
            }
            Item::Const { value, .. } => rewrite_expr(value, table, witnesses)?,
            Item::Comptime(block) => rewrite_block(block, table, witnesses)?,
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

fn rewrite_block(
    block: &mut Block,
    table: &TypeTable,
    witnesses: &WitnessPlan,
) -> Result<(), String> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => rewrite_expr(value, table, witnesses)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr(
    expr: &mut Expr,
    table: &TypeTable,
    witnesses: &WitnessPlan,
) -> Result<(), String> {
    let request = pack_request(table, expr)?;
    let upcast = upcast_request(table, expr)?;

    match expr {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                rewrite_expr(item, table, witnesses)?;
            }
        }
        Expr::Call { args, .. } => {
            for argument in args {
                rewrite_expr(argument, table, witnesses)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, table, witnesses)?;
            for argument in args {
                rewrite_expr(argument, table, witnesses)?;
            }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            rewrite_expr(receiver, table, witnesses)?;
            for argument in args {
                rewrite_expr(argument, table, witnesses)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                rewrite_expr(argument, table, witnesses)?;
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr(func, table, witnesses)?;
            for argument in args {
                rewrite_expr(argument, table, witnesses)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => rewrite_expr(expr, table, witnesses)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => {
            rewrite_block(body, table, witnesses)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewrite_expr(base, table, witnesses)?;
            for (_, value) in fields {
                rewrite_expr(value, table, witnesses)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr(value, table, witnesses)?;
            }
            if let Some(spread) = spread {
                rewrite_expr(spread, table, witnesses)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, table, witnesses)?;
            rewrite_expr(rhs, table, witnesses)?;
        }
        Expr::If { cond, then_block, else_block } => {
            rewrite_expr(cond, table, witnesses)?;
            rewrite_block(then_block, table, witnesses)?;
            if let Some(else_block) = else_block {
                rewrite_block(else_block, table, witnesses)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, table, witnesses)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewrite_expr(guard, table, witnesses)?;
                }
                rewrite_expr(&mut arm.body, table, witnesses)?;
            }
        }
        Expr::While { cond, body } => {
            rewrite_expr(cond, table, witnesses)?;
            rewrite_block(body, table, witnesses)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr(iter, table, witnesses)?;
            rewrite_block(body, table, witnesses)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr(lo, table, witnesses)?;
            rewrite_expr(hi, table, witnesses)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr(base, table, witnesses)?;
            rewrite_expr(index, table, witnesses)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            rewrite_expr(scrutinee, table, witnesses)?;
            rewrite_block(body, table, witnesses)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }

    if let Some((existential, concrete)) = request {
        let witness = witnesses
            .get(&existential, &concrete)
            .ok_or_else(|| {
                format!(
                    "existential witness plan lost construction `{concrete:?} as {existential:?}`"
                )
            })?
            .id;
        let old = std::mem::replace(expr, Expr::Bool(false));
        let payload = match old {
            Expr::As { expr: payload, .. } => *payload,
            other => other,
        };
        *expr = Expr::ExistentialPack {
            expr: Box::new(payload),
            ty: existential,
            witness,
        };
    } else if let Some((target, _source)) = upcast {
        let old = std::mem::replace(expr, Expr::Bool(false));
        let payload = match old {
            Expr::As { expr: payload, .. } => *payload,
            other => other,
        };
        *expr = Expr::ExistentialUpcast {
            expr: Box::new(payload),
            ty: target,
        };
    }
    Ok(())
}

fn visit_module_exprs(
    module: &Module,
    visitor: &mut impl FnMut(&Expr) -> Result<(), String>,
) -> Result<(), String> {
    for item in &module.items {
        match item {
            Item::Function(function) => visit_block(&function.body, visitor)?,
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if let Some(default) = &method.default {
                        visit_block(default, visitor)?;
                    }
                }
            }
            Item::Impl(definition) => {
                for method in &definition.methods {
                    visit_block(&method.body, visitor)?;
                }
            }
            Item::Const { value, .. } => visit_expr(value, visitor)?,
            Item::Comptime(block) => visit_block(block, visitor)?,
            Item::Type(_) | Item::TypeAlias { .. } => {}
        }
    }
    Ok(())
}

fn visit_block(
    block: &Block,
    visitor: &mut impl FnMut(&Expr) -> Result<(), String>,
) -> Result<(), String> {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr(value, visitor)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn visit_expr(
    expr: &Expr,
    visitor: &mut impl FnMut(&Expr) -> Result<(), String>,
) -> Result<(), String> {
    visitor(expr)?;
    match expr {
        Expr::List(items)
        | Expr::Tuple(items)
        | Expr::Ctor { args: items, .. }
        | Expr::AnonCtor { args: items, .. } => {
            for item in items {
                visit_expr(item, visitor)?;
            }
        }
        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
            if let Expr::MethodCall { receiver, .. } = expr {
                visit_expr(receiver, visitor)?;
            }
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr(receiver, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Apply { func, args } => {
            visit_expr(func, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. }
        | Expr::Field { base: expr, .. } => visit_expr(expr, visitor)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block(body, visitor)?,
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr(base, visitor)?;
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
            if let Some(spread) = spread {
                visit_expr(spread, visitor)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, visitor)?;
            visit_expr(rhs, visitor)?;
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr(cond, visitor)?;
            visit_block(then_block, visitor)?;
            if let Some(else_block) = else_block {
                visit_block(else_block, visitor)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr(scrutinee, visitor)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expr(guard, visitor)?;
                }
                visit_expr(&arm.body, visitor)?;
            }
        }
        Expr::While { cond, body } => {
            visit_expr(cond, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::For { iter, body, .. } => {
            visit_expr(iter, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr(lo, visitor)?;
            visit_expr(hi, visitor)?;
        }
        Expr::Index { base, index } => {
            visit_expr(base, visitor)?;
            visit_expr(index, visitor)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            visit_expr(scrutinee, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser;

    #[test]
    fn explicit_erasure_becomes_a_typed_compiler_owned_pack() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

impl Render for Label:
    fn render(self) -> String:
        match self:
            Label(text) -> text

fn erase(value: Label) -> dyn Render:
    value as dyn Render
"#;
        let mut module = parser::parse_module(source).expect("parse existential pack");
        let catalog = WitnessCatalog::from_module(&module);
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let prepared = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect("lower pack");
        assert_eq!(prepared.witnesses().witnesses.len(), 1);
        assert_eq!(prepared.witnesses().witnesses[0].id, 0);

        let function = prepared
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "erase" => Some(function),
                _ => None,
            })
            .expect("erase function");
        assert!(matches!(
            function.body.stmts.as_slice(),
            [Stmt::Expr(Expr::ExistentialPack {
                ty: Type::Dyn(name, args),
                witness: 0,
                ..
            })] if name == "Render" && args.is_empty()
        ));

        let retyped = crate::typeck::annotate(prepared.module().clone());
        let function = retyped
            .module()
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) if function.name == "erase" => Some(function),
                _ => None,
            })
            .expect("retyped erase function");
        let [Stmt::Expr(pack @ Expr::ExistentialPack { .. })] =
            function.body.stmts.as_slice()
        else {
            panic!("reannotation must preserve the compiler-owned pack");
        };
        assert!(matches!(
            retyped.table().type_of(pack).and_then(ty_to_ast),
            Some(Type::Dyn(name, args)) if name == "Render" && args.is_empty()
        ));
    }

    #[test]
    fn explicit_erasure_without_an_impl_fails_before_either_backend() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

fn erase(value: Label) -> dyn Render:
    value as dyn Render
"#;
        let mut module = parser::parse_module(source).expect("parse missing witness");
        let catalog = WitnessCatalog::from_module(&module);
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let error = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect_err("missing impl must reject")
            .to_string();
        assert!(
            error.contains("no linked `impl Render` for `named:Label()`"),
            "{error}"
        );
    }

    #[test]
    fn directed_coercions_become_packs_at_the_exact_expected_type_sites() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

type Wrapped:
    Wrapped(dyn Render)

type Envelope:
    item: dyn Render

impl Render for Label:
    fn render(self) -> String:
        match self:
            Label(text) -> text

fn accept(value: dyn Render) -> Nil:
    Nil

fn accept_owned(own value: dyn Render) -> Nil:
    Nil

fn erase_return(value: Label) -> dyn Render:
    value

fn infer_then_erase(value: Label) -> dyn Render:
    let concrete = value
    concrete

fn erase_branch(flag: Bool, value: Label) -> dyn Render:
    if flag:
        let marker = 1
        value
    else:
        let marker = 2
        value

fn erase_argument(value: Label) -> Nil:
    accept(value)

fn erase_owned_argument(value: Label) -> Nil:
    accept_owned(value)

fn erase_element(value: Label) -> List(dyn Render):
    [value]

fn erase_annotation(value: Label) -> Nil:
    let item: dyn Render = value
    Nil

fn erase_assignment(value: Label) -> Nil:
    var item: dyn Render = value
    item = value
    Nil

fn erase_tuple(value: Label) -> (dyn Render, Int):
    (value, 1)

fn erase_field(value: Label) -> Wrapped:
    Wrapped(value)

fn erase_update(value: Label, base: Envelope) -> Envelope:
    Envelope(item: value, ..base)
"#;
        let mut module = parser::parse_module(source).expect("parse directed coercions");
        let catalog = WitnessCatalog::from_module(&module);
        let checked = witchy_syntax::source_check::check(module).expect("source check");
        let checked = witchy_syntax::generators::lower(checked).expect("generator lowering");
        let checked = witchy_syntax::async_lower::lower(checked).expect("async lowering");
        module = witchy_syntax::records::lower(checked)
            .expect("lower named record updates")
            .into_module();
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let prepared = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect("lower directed packs");

        assert_eq!(
            prepared.witnesses().witnesses.len(),
            1,
            "all construction sites share one concrete witness"
        );
        let mut packs = 0;
        visit_module_exprs(prepared.module(), &mut |expr| {
            if matches!(
                expr,
                Expr::ExistentialPack {
                    ty: Type::Dyn(name, args),
                    witness: 0,
                    ..
                } if name == "Render" && args.is_empty()
            ) {
                packs += 1;
            }
            Ok(())
        })
        .expect("visit prepared module");
        assert_eq!(
            packs, 13,
            "every directed expected-type site needs one exact pack"
        );
    }

    #[test]
    fn directed_coercion_without_an_impl_fails_before_either_backend() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

fn erase(value: Label) -> dyn Render:
    value
"#;
        let mut module = parser::parse_module(source).expect("parse missing directed witness");
        let catalog = WitnessCatalog::from_module(&module);
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let error = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect_err("missing directed witness must reject")
            .to_string();
        assert!(
            error.contains("no linked `impl Render` for `named:Label()`"),
            "{error}"
        );
    }

    #[test]
    fn heterogeneous_match_arms_select_their_own_witnesses() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

type Badge:
    Badge(Int)

impl Render for Label:
    fn render(self) -> String:
        match self:
            Label(text) -> text

impl Render for Badge:
    fn render(self) -> String:
        "badge"

fn choose(flag: Bool, label: Label, badge: Badge) -> dyn Render:
    match flag:
        true -> label
        false -> badge
"#;
        let mut module = parser::parse_module(source).expect("parse heterogeneous match");
        let catalog = WitnessCatalog::from_module(&module);
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let prepared = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect("lower heterogeneous packs");

        assert_eq!(prepared.witnesses().witnesses.len(), 2);
        let mut witness_ids = Vec::new();
        visit_module_exprs(prepared.module(), &mut |expr| {
            if let Expr::ExistentialPack { witness, .. } = expr {
                witness_ids.push(*witness);
            }
            Ok(())
        })
        .expect("visit heterogeneous packs");
        witness_ids.sort_unstable();
        assert_eq!(witness_ids, [0, 1]);
    }

    #[test]
    fn bounded_generic_erasure_resolves_after_monomorphization() {
        let source = r#"
trait Render:
    fn render(self) -> String

type Label:
    Label(String)

type Badge:
    Badge(Int)

impl Render for Label:
    fn render(self) -> String:
        match self:
            Label(text) -> text

impl Render for Badge:
    fn render(self) -> String:
        "badge"

fn erase(value: a) -> dyn Render where a: Render:
    value

fn pair(label: Label, badge: Badge) -> (dyn Render, dyn Render):
    (erase(label), erase(badge))
"#;
        let module = parser::parse_module(source).expect("parse generic erasure");
        let catalog = WitnessCatalog::from_module(&module);
        let lowered = crate::traits::lower_checked(module).expect("monomorphize erasure");
        assert!(
            lowered
                .items
                .iter()
                .all(|item| !matches!(item, Item::Trait(_) | Item::Impl(_))),
            "existential preparation must see the final trait-lowered module"
        );
        let prepared = lower_explicit_packs(crate::typeck::annotate(lowered), &catalog)
            .expect("prepare specialized erasures");

        assert_eq!(prepared.witnesses().witnesses.len(), 2);
        let mut packs = 0;
        visit_module_exprs(prepared.module(), &mut |expr| {
            if matches!(expr, Expr::ExistentialPack { .. }) {
                packs += 1;
            }
            Ok(())
        })
        .expect("visit specialized packs");
        assert_eq!(packs, 2);
    }

    #[test]
    fn unresolved_directed_payload_fails_instead_of_dropping_the_pack() {
        let source = r#"
trait Render:
    fn render(self) -> String

fn erase() -> dyn Render:
    None
"#;
        let mut module = parser::parse_module(source).expect("parse unresolved payload");
        let catalog = WitnessCatalog::from_module(&module);
        module
            .items
            .retain(|item| !matches!(item, Item::Trait(_) | Item::Impl(_)));
        let error = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect_err("an unresolved payload must not erase the pack request");
        assert!(
            error.contains(
                "existential construction requires one fully resolved concrete payload type"
            ),
            "{error}"
        );
    }

    #[test]
    fn preparation_rejects_a_pre_lowering_module() {
        let module = parser::parse_module(
            "trait Render:\n\
             \x20   fn render(self) -> String\n",
        )
        .expect("parse trait");
        let catalog = WitnessCatalog::from_module(&module);
        let error = lower_explicit_packs(crate::typeck::annotate(module), &catalog)
            .expect_err("trait declarations prove preparation ran too early");
        assert!(
            error.contains("trait-lowered, monomorphic executable module"),
            "{error}"
        );
    }
}
