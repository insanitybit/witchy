//! RFC-0081 compiler-owned existential construction.
//!
//! This pass consumes a typed source AST after checking has selected concrete
//! payload types. It builds the deterministic closed-program witness plan and
//! replaces every directed concrete-to-`dyn Trait` coercion with a typed,
//! source-unspellable AST node. Backends therefore consume one construction
//! contract without rediscovering coercions, impl identity, or runtime types.

use witchy_syntax::ast::{Block, Expr, Item, Module, Stmt, Type};
use witchy_syntax::{format, intrinsics};

use crate::runtime_type::{
    RuntimeDeclarationCatalog, RuntimeTypeIdentity, RuntimeTypePlan,
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

    pub fn into_runtime_parts(self) -> (Module, TypeTable, WitnessPlan, RuntimeTypePlan) {
        (self.module, self.table, self.witnesses, self.runtime_types)
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
    let dynamic_identities =
        collect_dynamic_identities(typed.module(), typed.table(), runtime_catalog)?;
    let runtime_types = RuntimeTypePlan::build(dynamic_identities)
        .map_err(|error| error.to_string())?;
    let (requests, upcasts) = collect_requests(typed.module(), typed.table())?;
    let witnesses = witness::build_from_catalog_with_upcasts(catalog, requests, upcasts)?;
    let (module, table, result) = typed.rewrite_into_module(|table, module| {
        rewrite_dynamic_module(module, table, runtime_catalog, &runtime_types)?;
        rewrite_module(module, table, &witnesses)
    });
    result?;
    Ok(PreparedExistentials { module, table, witnesses, runtime_types })
}

fn dynamic_intrinsic(name: &str, intrinsic: &str) -> bool {
    name == intrinsic || name.rsplit('.').next() == Some(intrinsic)
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
        if matches.next().is_some() {
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

fn collect_dynamic_identities(
    module: &Module,
    table: &TypeTable,
    runtime_catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<Vec<RuntimeTypeIdentity>, String> {
    let mut requested = Vec::new();
    visit_module_exprs(module, &mut |expr| {
        if let Some(ty) = dynamic_identity_request(module, table, expr)? {
            requested.push(ty);
        }
        Ok(())
    })?;
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    let runtime_catalog = runtime_catalog.ok_or_else(|| {
        "Dynamic operations require authenticated runtime declaration ownership".to_string()
    })?;
    requested
        .iter()
        .map(|ty| {
            runtime_catalog
                .capability_free_type_identity(ty, module)
                .map_err(|error| error.to_string())
        })
        .collect()
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
    let request = dynamic_identity_request(module, table, expr)?;
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
            Expr::Call { name, args }
                if dynamic_intrinsic(name, intrinsics::DYNAMIC_TRY_DECODE) =>
            {
                *name = intrinsics::DYNAMIC_TRY_DECODE_TYPED.into();
                args.push(Expr::Int(i64::from(descriptor.index())));
            }
            _ => {}
        }
    }
    Ok(())
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
