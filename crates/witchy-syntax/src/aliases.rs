//! Type-alias expansion.
//!
//! A top-level `type Id = Int` (`Item::TypeAlias`) is expanded to its target
//! everywhere a type is written — function signatures, record/variant fields,
//! trait/impl method signatures, `where`-clause trait arguments, impl heads
//! (`impl T for Id`), and every type written inside a body: `let`/`var`
//! ascriptions (`let x: Id = …`), `as` casts (`… as Id`), and lambda
//! parameter/return annotations — and then dropped, so the type checker and code
//! generator only ever see concrete types. Generic aliases substitute their
//! arguments before expansion (`type Pair(a) = (a, a)`; `Pair(Int)` →
//! `(Int, Int)`). Aliases may chain (`type B = A`, resolved to a fixpoint
//! first).

use crate::ast::{
    anon_record_field_names, collect_type_names, synthetic_anon_record_def, Block, Expr, Function,
    Item, MethodSig, Module, Stmt, Type,
};
use crate::intrinsics;
// foldhash: compiler-internal keys only — see witchy-types/src/typeck.rs.
use foldhash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Alias {
    pub params: Vec<String>,
    pub ty: Type,
}

#[derive(Clone, Copy)]
struct ResolveContext<'a> {
    aliases: &'a HashMap<String, Alias>,
    expand_aliases: bool,
}

/// The name of a type alias defined in terms of itself (directly or through a
/// chain), if any — so the linker can report it rather than letting the alias
/// expand to a dangling reference. Returns the first cyclic alias found.
pub(crate) fn find_cycle(module: &Module) -> Option<String> {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, ty, .. } = item {
            let mut refs = Vec::new();
            collect_type_names(ty, &mut refs);
            edges.insert(name.clone(), refs);
        }
    }
    // Restrict edges to other aliases, then DFS for a back edge.
    let names: HashSet<String> = edges.keys().cloned().collect();
    for refs in edges.values_mut() {
        refs.retain(|r| names.contains(r));
    }
    let mut state: HashMap<String, u8> = HashMap::new(); // 0=unseen,1=on-stack,2=done
    for start in edges.keys() {
        if let Some(c) = dfs_cycle(start, &edges, &mut state) {
            return Some(c);
        }
    }
    None
}

/// DFS from `node`, returning a node that lies on a cycle if one is reachable.
fn dfs_cycle(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    state: &mut HashMap<String, u8>,
) -> Option<String> {
    match state.get(node) {
        Some(2) => return None,                   // already fully explored
        Some(1) => return Some(node.to_string()), // back edge: cycle
        _ => {}
    }
    state.insert(node.to_string(), 1);
    if let Some(next) = edges.get(node) {
        for n in next {
            if let Some(c) = dfs_cycle(n, edges, state) {
                return Some(c);
            }
        }
    }
    state.insert(node.to_string(), 2);
    None
}

/// Expand every type alias, normalize RFC-0098 structural-record composition,
/// synthesize the resulting exact anonymous shapes, and drop alias items.
pub fn resolve(module: Module) -> Result<Module, String> {
    resolve_impl(module, true, true)
}

/// Normalize composition before record derives expand while retaining alias
/// declarations for compile-time generated source that may still name them.
/// Alias uses inside a composition are expanded so its exact shape is known;
/// unrelated written aliases remain intact for the later [`resolve`] pass.
pub(crate) fn normalize_record_compositions(module: Module) -> Result<Module, String> {
    resolve_impl(module, false, false)
}

fn resolve_impl(
    mut module: Module,
    drop_aliases: bool,
    expand_aliases: bool,
) -> Result<Module, String> {
    let map = resolved_map(&module)?;
    let context = ResolveContext {
        aliases: &map,
        expand_aliases,
    };
    let mut shapes = HashSet::new();
    for alias in map.values() {
        collect_anon_shapes(&alias.ty, &mut shapes);
    }

    for item in &mut module.items {
        resolve_item(item, &context, &mut shapes)?;
    }

    if drop_aliases {
        module
            .items
            .retain(|it| !matches!(it, Item::TypeAlias { .. }));
    }
    let existing: HashSet<String> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(definition) => Some(definition.name.clone()),
            _ => None,
        })
        .collect();
    let mut generated = shapes
        .into_iter()
        .filter(|fields| !existing.contains(&crate::ast::anon_record_type_name(fields)))
        .collect::<Vec<_>>();
    generated.sort();
    if !generated.is_empty() {
        let count = generated.len();
        let mut definitions = generated
            .iter()
            .map(|fields| Item::Type(synthetic_anon_record_def(fields)))
            .collect::<Vec<_>>();
        definitions.append(&mut module.items);
        module.items = definitions;
        let mut lines = vec![u32::MAX; count];
        lines.append(&mut module.item_lines);
        module.item_lines = lines;
        if !module.imports.iter().any(|import| import == "reflect") {
            module.imports.push("reflect".into());
            module.import_lines.push(u32::MAX);
        }
    }
    Ok(module)
}

/// Build the same fixpoint-resolved alias map [`resolve`] uses, without mutating
/// the module. Consumers that only need normalized type facts, such as
/// `derive(...)` TypeInfo construction, can apply this to a clone while leaving
/// the linker's later alias-cycle diagnostics and alias-erasure pass intact.
pub(crate) fn resolved_map(module: &Module) -> Result<HashMap<String, Alias>, String> {
    if let Some(cycle) = find_cycle(module) {
        return Err(format!("type alias `{cycle}` is defined cyclically"));
    }
    let mut map: HashMap<String, Alias> = HashMap::new();
    for item in &module.items {
        if let Item::TypeAlias { name, params, ty } = item {
            map.insert(
                name.clone(),
                Alias {
                    params: params.clone(),
                    ty: ty.clone(),
                },
            );
        }
    }
    if map.is_empty() {
        return Ok(map);
    }

    // Resolve alias-to-alias references to a fixpoint, so each alias maps to an
    // alias-free type. The iteration cap makes a cyclic alias terminate.
    let rounds = map.len() + 1;
    for _ in 0..rounds {
        let snapshot = map.clone();
        let mut changed = false;
        for alias in map.values_mut() {
            let mut ignored = HashSet::new();
            let context = ResolveContext {
                aliases: &snapshot,
                expand_aliases: true,
            };
            changed |= resolve_type(&mut alias.ty, &context, &mut ignored)?;
        }
        if !changed {
            break;
        }
    }

    Ok(map)
}

fn resolve_item(
    item: &mut Item,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    match item {
        Item::Function(f) => resolve_function(f, context, shapes)?,
        Item::Type(t) => {
            for v in &mut t.variants {
                for ft in &mut v.fields {
                    resolve_type(ft, context, shapes)?;
                }
            }
        }
        Item::Trait(t) => {
            for m in &mut t.methods {
                resolve_methodsig(m, context, shapes)?;
            }
        }
        Item::Impl(im) => {
            // The impl head is itself a written-type position: `impl Show
            // for Id` targets an alias, and `impl FromIterator(Id) for
            // Set(Id) where a: Bound(Id)` writes aliases in its trait/target
            // arguments and `where` clause.
            resolve_impl_target(&mut im.type_name, &mut im.target_args, context, shapes)?;
            for t in &mut im.trait_args {
                resolve_type(t, context, shapes)?;
            }
            resolve_bounds(&mut im.bounds, context, shapes)?;
            for m in &mut im.methods {
                resolve_function(m, context, shapes)?;
            }
        }
        Item::TypeAlias { ty, .. } => {
            resolve_type(ty, context, shapes)?;
        }
        Item::Const { value, .. } => resolve_in_expr(value, context, shapes)?,
        Item::Comptime(block) => resolve_in_block(block, context, shapes)?,
    }
    Ok(())
}

pub(crate) fn resolve_type_aliases(
    ty: &mut Type,
    map: &HashMap<String, Alias>,
) -> Result<bool, String> {
    let mut ignored = HashSet::new();
    let context = ResolveContext {
        aliases: map,
        expand_aliases: true,
    };
    resolve_type(ty, &context, &mut ignored)
}

/// Expand aliases in the written-type positions of one compiler-owned
/// expression using the definition module's alias environment. Tagged syntax is
/// resolved before the ordinary per-module alias-erasure pass, so it must not
/// carry a definition-site alias into the consumer module.
pub(crate) fn resolve_expr_aliases(expr: &mut Expr, module: &Module) -> Result<(), String> {
    let map = resolved_map(module)?;
    if !map.is_empty() {
        let mut ignored = HashSet::new();
        let context = ResolveContext {
            aliases: &map,
            expand_aliases: true,
        };
        resolve_in_expr_with_origin(expr, &context, false, &mut ignored)?;
    }
    Ok(())
}

/// Expand alias names appearing anywhere in a type. The `map` is already
/// fixpoint-resolved, so a single replacement yields an alias-free type. Returns
/// whether anything changed.
fn resolve_type(
    ty: &mut Type,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<bool, String> {
    resolve_type_with_origin(ty, context, true, shapes)
}

fn resolve_type_with_origin(
    ty: &mut Type,
    context: &ResolveContext<'_>,
    resolve_call_site_head: bool,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<bool, String> {
    let mut changed = match ty {
        Type::Qualified(_, inner) => {
            resolve_type_with_origin(inner, context, resolve_call_site_head, shapes)?
        }
        Type::Slice(elem) => {
            resolve_type_with_origin(elem, context, resolve_call_site_head, shapes)?
        }
        Type::Named(name, args) => {
            let mut changed = false;
            for a in args.iter_mut() {
                changed |= resolve_type_with_origin(a, context, resolve_call_site_head, shapes)?;
            }
            let alias_name = if resolve_call_site_head {
                crate::linker::call_site_type_target(name).unwrap_or(name)
            } else {
                name
            };
            if let Some(alias) = context
                .expand_aliases
                .then(|| context.aliases.get(alias_name))
                .flatten()
            {
                if alias.params.len() == args.len() {
                    let subst: HashMap<String, Type> = alias
                        .params
                        .iter()
                        .cloned()
                        .zip(args.iter().cloned())
                        .collect();
                    let mut target = alias.ty.clone();
                    substitute_alias_params(&mut target, &subst);
                    *ty = target;
                    return Ok(true);
                }
            }
            changed
        }
        Type::Tuple(ts) => {
            let mut changed = false;
            for t in ts {
                changed |= resolve_type_with_origin(t, context, resolve_call_site_head, shapes)?;
            }
            changed
        }
        Type::RecordCompose { base, fields } => {
            let composition_context = ResolveContext {
                aliases: context.aliases,
                expand_aliases: true,
            };
            let mut changed = resolve_type_with_origin(
                base,
                &composition_context,
                resolve_call_site_head,
                shapes,
            )?;
            for (_, field) in fields {
                changed |= resolve_type_with_origin(
                    field,
                    &composition_context,
                    resolve_call_site_head,
                    shapes,
                )?;
            }
            changed
        }
        Type::Fn(params, ret, _, _) => {
            let mut changed = false;
            for p in params {
                changed |= resolve_type_with_origin(p, context, resolve_call_site_head, shapes)?;
            }
            changed |= resolve_type_with_origin(ret, context, resolve_call_site_head, shapes)?;
            changed
        }
        // (RFC-0081) The head is a trait name — aliases bind TYPE names, so only
        // the trait arguments expand. (`type R = dyn Render` itself resolves via
        // the ordinary `Named` alias lookup at R's use sites.)
        Type::Dyn(_, args) => {
            let mut changed = false;
            for a in args {
                changed |= resolve_type_with_origin(a, context, resolve_call_site_head, shapes)?;
            }
            changed
        }
    };
    if matches!(ty, Type::RecordCompose { .. }) {
        normalize_record_compose(ty, shapes)?;
        changed = true;
    }
    Ok(changed)
}

fn normalize_record_compose(
    ty: &mut Type,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    let Type::RecordCompose { base, fields } = ty else {
        return Ok(());
    };
    let Type::Named(base_name, base_types) = base.as_ref() else {
        return Err(format!(
            "type spread requires an anonymous record shape; `{}` is not a structural record",
            crate::format::type_str(base)
        ));
    };
    let Some(base_fields) = anon_record_field_names(base_name) else {
        return Err(format!(
            "type spread requires an anonymous record shape; `{}` is not structural",
            crate::format::type_str(base)
        ));
    };
    if base_fields.len() != base_types.len() {
        return Err("malformed compiler-owned anonymous record shape in type spread".into());
    }

    let mut merged = base_fields
        .iter()
        .cloned()
        .zip(base_types.iter().cloned())
        .collect::<Vec<_>>();
    for (field, extension_ty) in fields.iter() {
        if let Some((_, base_ty)) = merged.iter().find(|(name, _)| name == field) {
            if base_ty != extension_ty {
                return Err(format!(
                    "field `{field}` has conflicting types in structural record composition: \
                     base provides `{}`, extension declares `{}`",
                    crate::format::type_str(base_ty),
                    crate::format::type_str(extension_ty)
                ));
            }
        } else {
            merged.push((field.clone(), extension_ty.clone()));
        }
    }
    merged.sort_by(|left, right| left.0.cmp(&right.0));
    let names = merged
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let types = merged.into_iter().map(|(_, ty)| ty).collect();
    shapes.insert(names.clone());
    *ty = Type::Named(crate::ast::anon_record_type_name(&names), types);
    Ok(())
}

fn collect_anon_shapes(ty: &Type, out: &mut HashSet<Vec<String>>) {
    match ty {
        Type::Named(name, args) => {
            if let Some(fields) = anon_record_field_names(name) {
                out.insert(fields);
            }
            for arg in args {
                collect_anon_shapes(arg, out);
            }
        }
        Type::Tuple(items) | Type::Dyn(_, items) => {
            for item in items {
                collect_anon_shapes(item, out);
            }
        }
        Type::RecordCompose { base, fields } => {
            collect_anon_shapes(base, out);
            for (_, field) in fields {
                collect_anon_shapes(field, out);
            }
        }
        Type::Fn(params, result, _, _) => {
            for param in params {
                collect_anon_shapes(param, out);
            }
            collect_anon_shapes(result, out);
        }
        Type::Slice(elem) => collect_anon_shapes(elem, out),
        Type::Qualified(_, inner) => collect_anon_shapes(inner, out),
    }
}

fn substitute_alias_params(ty: &mut Type, subst: &HashMap<String, Type>) -> bool {
    match ty {
        Type::Qualified(_, inner) => substitute_alias_params(inner, subst),
        Type::Slice(elem) => substitute_alias_params(elem, subst),
        Type::Named(name, args) => {
            if args.is_empty() {
                if let Some(target) = subst.get(name) {
                    *ty = target.clone();
                    return true;
                }
            }
            let mut changed = false;
            for a in args {
                changed |= substitute_alias_params(a, subst);
            }
            changed
        }
        Type::Tuple(ts) => {
            let mut changed = false;
            for t in ts {
                changed |= substitute_alias_params(t, subst);
            }
            changed
        }
        Type::RecordCompose { base, fields } => {
            let mut changed = substitute_alias_params(base, subst);
            for (_, field) in fields {
                changed |= substitute_alias_params(field, subst);
            }
            changed
        }
        Type::Fn(params, ret, _, _) => {
            let mut changed = false;
            for p in params {
                changed |= substitute_alias_params(p, subst);
            }
            changed |= substitute_alias_params(ret, subst);
            changed
        }
        Type::Dyn(_, args) => {
            let mut changed = false;
            for a in args {
                changed |= substitute_alias_params(a, subst);
            }
            changed
        }
    }
}

fn resolve_function(
    f: &mut Function,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    for p in &mut f.params {
        if let Some(t) = &mut p.ty {
            resolve_type(t, context, shapes)?;
        }
    }
    if let Some(t) = &mut f.ret {
        resolve_type(t, context, shapes)?;
    }
    resolve_bounds(&mut f.bounds, context, shapes)?;
    resolve_in_block(&mut f.body, context, shapes)
}

/// Resolve aliases in a `where`-clause's trait type-arguments (`where c:
/// FromIterator(Id)` → `… FromIterator(Int)`). The bound's variable and trait
/// names are never type aliases, so only the trait arguments are rewritten.
fn resolve_bounds(
    bounds: &mut [(String, String, Vec<Type>)],
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    for (_, _, trait_args) in bounds.iter_mut() {
        for t in trait_args {
            resolve_type(t, context, shapes)?;
        }
    }
    Ok(())
}

/// Resolve an alias used as an impl-head target (`impl Show for Id`). If the
/// target denotes a named type after alias substitution it is rewritten to that
/// type's head and arguments. An alias to a non-named target (tuple/function
/// type) is not a valid impl target, so it is left untouched for the checker to
/// report — this stays fail-closed.
fn resolve_impl_target(
    name: &mut String,
    args: &mut Vec<Type>,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    for a in args.iter_mut() {
        resolve_type(a, context, shapes)?;
    }
    if !context.expand_aliases {
        return Ok(());
    }
    let Some(alias) = context.aliases.get(name) else {
        return Ok(());
    };
    if alias.params.len() != args.len() {
        return Ok(());
    }
    let subst: HashMap<String, Type> = alias
        .params
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    let mut target = alias.ty.clone();
    substitute_alias_params(&mut target, &subst);
    if let Type::Named(target_name, target_args) = target {
        *name = target_name;
        *args = target_args;
    }
    Ok(())
}

fn resolve_methodsig(
    m: &mut MethodSig,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    for p in &mut m.params {
        if let Some(t) = &mut p.ty {
            resolve_type(t, context, shapes)?;
        }
    }
    if let Some(t) = &mut m.ret {
        resolve_type(t, context, shapes)?;
    }
    if let Some(b) = &mut m.default {
        resolve_in_block(b, context, shapes)?;
    }
    Ok(())
}

/// Walk a block, resolving aliases in every type written inside a body: `let`/`var`
/// ascriptions, `as`-cast targets, and lambda parameter/return annotations (the
/// last two reached through `resolve_in_expr`).
fn resolve_in_block(
    block: &mut Block,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    resolve_in_block_with_origin(block, context, true, shapes)
}

fn resolve_in_block_with_origin(
    block: &mut Block,
    context: &ResolveContext<'_>,
    resolve_call_site_head: bool,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    if let Some(region) = &mut block.region {
        if let Some(ty) = &mut region.ty {
            resolve_type_with_origin(ty, context, resolve_call_site_head, shapes)?;
        }
    }
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    resolve_type_with_origin(t, context, resolve_call_site_head, shapes)?;
                }
                resolve_in_expr_with_origin(value, context, resolve_call_site_head, shapes)?;
            }
            Stmt::LetPattern { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value) => {
                resolve_in_expr_with_origin(value, context, resolve_call_site_head, shapes)?
            }
            Stmt::Return(Some(e)) => {
                resolve_in_expr_with_origin(e, context, resolve_call_site_head, shapes)?
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn resolve_in_expr(
    expr: &mut Expr,
    context: &ResolveContext<'_>,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    resolve_in_expr_with_origin(expr, context, true, shapes)
}

fn resolve_in_expr_with_origin(
    e: &mut Expr,
    map: &ResolveContext<'_>,
    resolve_call_site_head: bool,
    shapes: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    match e {
        Expr::Lambda { params, body, ret, .. } => {
            for p in params.iter_mut() {
                if let Some(t) = &mut p.ty {
                    resolve_type_with_origin(t, map, resolve_call_site_head, shapes)?;
                }
            }
            if let Some(t) = ret {
                resolve_type_with_origin(t, map, resolve_call_site_head, shapes)?;
            }
            resolve_in_block_with_origin(body, map, resolve_call_site_head, shapes)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                resolve_in_expr_with_origin(x, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for a in args {
                resolve_in_expr_with_origin(a, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, a) in args {
                resolve_in_expr_with_origin(a, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            resolve_in_expr_with_origin(receiver, map, resolve_call_site_head, shapes)?;
            for (_, a) in args {
                resolve_in_expr_with_origin(a, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::MethodCall { receiver, args, .. } => {
            resolve_in_expr_with_origin(receiver, map, resolve_call_site_head, shapes)?;
            for a in args {
                resolve_in_expr_with_origin(a, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::Apply { func, args } => {
            resolve_in_expr_with_origin(func, map, resolve_call_site_head, shapes)?;
            for a in args {
                resolve_in_expr_with_origin(a, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::As { expr, ty } => {
            resolve_in_expr_with_origin(expr, map, resolve_call_site_head, shapes)?;
            resolve_type_with_origin(ty, map, resolve_call_site_head, shapes)?;
        }
        Expr::ExistentialPack { expr, ty, .. } => {
            resolve_in_expr_with_origin(expr, map, resolve_call_site_head, shapes)?;
            resolve_type_with_origin(ty, map, resolve_call_site_head, shapes)?;
        }
        Expr::ExistentialUpcast { expr, ty } => {
            resolve_in_expr_with_origin(expr, map, resolve_call_site_head, shapes)?;
            resolve_type_with_origin(ty, map, resolve_call_site_head, shapes)?;
        }
        Expr::ExistentialCall {
            receiver,
            args,
            ty,
            result,
            ..
        } => {
            resolve_in_expr_with_origin(receiver, map, resolve_call_site_head, shapes)?;
            for arg in args {
                resolve_in_expr_with_origin(arg, map, resolve_call_site_head, shapes)?;
            }
            resolve_type_with_origin(ty, map, resolve_call_site_head, shapes)?;
            resolve_type_with_origin(result, map, resolve_call_site_head, shapes)?;
        }
        Expr::Unary { expr, .. } | Expr::Try(expr) | Expr::Field { base: expr, .. } => {
            resolve_in_expr_with_origin(expr, map, resolve_call_site_head, shapes)?
        }
        Expr::RecordUpdate {
            name: _,
            base,
            fields,
        } => {
            resolve_in_expr_with_origin(base, map, resolve_call_site_head, shapes)?;
            for (_, v) in fields {
                resolve_in_expr_with_origin(v, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, v) in fields {
                resolve_in_expr_with_origin(v, map, resolve_call_site_head, shapes)?;
            }
            if let Some(s) = spread {
                resolve_in_expr_with_origin(s, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_in_expr_with_origin(lhs, map, resolve_call_site_head, shapes)?;
            resolve_in_expr_with_origin(rhs, map, resolve_call_site_head, shapes)?;
        }
        Expr::Range { lo, hi, .. } => {
            resolve_in_expr_with_origin(lo, map, resolve_call_site_head, shapes)?;
            resolve_in_expr_with_origin(hi, map, resolve_call_site_head, shapes)?;
        }
        Expr::Index { base, index } => {
            resolve_in_expr_with_origin(base, map, resolve_call_site_head, shapes)?;
            resolve_in_expr_with_origin(index, map, resolve_call_site_head, shapes)?;
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            resolve_in_expr_with_origin(scrutinee, map, resolve_call_site_head, shapes)?;
            resolve_in_block_with_origin(body, map, resolve_call_site_head, shapes)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            resolve_in_expr_with_origin(cond, map, resolve_call_site_head, shapes)?;
            resolve_in_block_with_origin(then_block, map, resolve_call_site_head, shapes)?;
            if let Some(b) = else_block {
                resolve_in_block_with_origin(b, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::While { cond, body } => {
            resolve_in_expr_with_origin(cond, map, resolve_call_site_head, shapes)?;
            resolve_in_block_with_origin(body, map, resolve_call_site_head, shapes)?;
        }
        Expr::For { iter, body, .. } => {
            resolve_in_expr_with_origin(iter, map, resolve_call_site_head, shapes)?;
            resolve_in_block_with_origin(body, map, resolve_call_site_head, shapes)?;
        }
        Expr::Match { scrutinee, arms } => {
            resolve_in_expr_with_origin(scrutinee, map, resolve_call_site_head, shapes)?;
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    resolve_in_expr_with_origin(g, map, resolve_call_site_head, shapes)?;
                }
                resolve_in_expr_with_origin(&mut arm.body, map, resolve_call_site_head, shapes)?;
            }
        }
        Expr::Block(b) => {
            resolve_in_block_with_origin(b, map, resolve_call_site_head, shapes)?;
        }
    }
    Ok(())
}

/// Map a bare builtin name that has since moved to a module-qualified stdlib
/// path (e.g. `push` -> `list.push`). Returns `None` for names that were never
/// moved. The type checker uses this to suggest the new spelling; the formatter
/// uses it to rewrite legacy calls to their canonical form.
pub fn moved_builtin(bare: &str) -> Option<&'static str> {
    Some(match bare {
        "push" => "list.push",
        "at" => intrinsics::LIST_AT,
        "length" => intrinsics::LIST_LENGTH,
        "concat" => intrinsics::LIST_CONCAT,
        "dict_new" => intrinsics::DICT_NEW,
        "insert" => "dict.insert",
        "get_or" => intrinsics::DICT_GET_OR,
        "has" => intrinsics::DICT_CONTAINS_KEY,
        "remove" => "dict.remove",
        "update" => "dict.update",
        "keys" => intrinsics::DICT_KEYS,
        "values" => intrinsics::DICT_VALUES,
        "pairs" => intrinsics::DICT_PAIRS,
        "size" => intrinsics::DICT_LENGTH,
        "split" => intrinsics::STRING_SPLIT,
        "trim" => intrinsics::STRING_TRIM,
        "contains" => intrinsics::STRING_CONTAINS,
        "starts_with" => intrinsics::STRING_STARTS_WITH,
        "ends_with" => intrinsics::STRING_ENDS_WITH,
        "replace" => intrinsics::STRING_REPLACE,
        "index_of" => "string.index_of",
        "substring" => intrinsics::STRING_SUBSTRING,
        "as_str" => intrinsics::STRING_AS_STR,
        "slice" => intrinsics::STRING_SLICE,
        "to_string" => intrinsics::STRING_TO_STRING,
        "len" => intrinsics::STRING_LEN,
        "string_length" => intrinsics::STRING_LENGTH,
        "char_count" => intrinsics::STRING_CHAR_COUNT,
        "string_chars" => intrinsics::STRING_CHARS,
        "to_chars" => intrinsics::STRING_CHARS,
        "to_upper" => intrinsics::STRING_TO_UPPER,
        "to_lower" => intrinsics::STRING_TO_LOWER,
        "string_to_int" => intrinsics::STRING_TO_INT,
        "int_to_float" => intrinsics::MATH_TO_FLOAT,
        "float_to_int" => intrinsics::MATH_TO_INT,
        "sqrt" => intrinsics::MATH_SQRT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_alias_in_signature_and_chains() {
        // `type Meters = Int`, `type Distance = Meters` — both expand to Int, and
        // no alias item survives.
        let src =
            "type Meters = Int\ntype Distance = Meters\nfn far(d: Distance) -> Meters:\n    d\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        assert!(!m
            .items
            .iter()
            .any(|it| matches!(it, Item::TypeAlias { .. })));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        assert_eq!(f.params[0].ty, Some(Type::Named("Int".into(), vec![])));
        assert_eq!(f.ret, Some(Type::Named("Int".into(), vec![])));
    }

    #[test]
    fn find_cycle_flags_cyclic_aliases() {
        let cyclic = crate::parser::parse_module("type A = B\ntype B = A\n").expect("parse");
        assert!(find_cycle(&cyclic).is_some());
        let ok = crate::parser::parse_module("type A = Int\ntype B = A\n").expect("parse");
        assert!(find_cycle(&ok).is_none());
    }

    #[test]
    fn expands_alias_in_body_written_types() {
        // `type Id = Int` written inside a body — a `let` ascription, a lambda
        // parameter and return type — must all expand to `Int`, not stay `Id`
        // (which the checker would reject as an unknown type).
        let src =
            "type Id = Int\nfn main():\n    let x: Id = 5\n    let f = fn(n: Id) -> Id: n\n    x\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        let int = Type::Named("Int".into(), vec![]);
        // `let x: Id = 5` — the ascription expands.
        match &f.body.stmts[0] {
            Stmt::Let { ty, .. } => assert_eq!(ty.as_ref(), Some(&int)),
            s => panic!("expected `let x: Id`, got {s:?}"),
        }
        // `let f = fn(n: Id) -> Id: n` — lambda parameter and return expand.
        match &f.body.stmts[1] {
            Stmt::Let {
                value: Expr::Lambda { params, ret, .. },
                ..
            } => {
                assert_eq!(params[0].ty.as_ref(), Some(&int));
                assert_eq!(ret.as_ref(), Some(&int));
            }
            s => panic!("expected `let f = fn ...`, got {s:?}"),
        }
    }

    #[test]
    fn expands_alias_in_as_cast_and_where_bound() {
        // `x as Id` (`Expr::As`) and a `where` trait argument (`FromIterator(Id)`)
        // are both written-type positions that must expand to the target.
        let src = "type Id = Int\nfn each(c: Id) -> Id where c: FromIterator(Id):\n    c as Id\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        let int = Type::Named("Int".into(), vec![]);
        // The `where c: FromIterator(Id)` bound's trait argument expands.
        assert_eq!(f.bounds[0].2, vec![int.clone()]);
        // `c as Id` — the cast target expands (last statement is the tail expr).
        match f.body.stmts.last().expect("stmt") {
            Stmt::Expr(Expr::As { ty, .. }) => assert_eq!(ty, &int),
            s => panic!("expected `c as Id`, got {s:?}"),
        }
    }

    #[test]
    fn expands_alias_in_impl_head() {
        // `impl Describe for Id` targets an alias; the head's `type_name` must
        // expand so the impl attaches to the concrete `Int`.
        let src = "type Id = Int\ntrait Describe:\n    fn describe(self) -> String\nimpl Describe for Id:\n    fn describe(self) -> String:\n        \"id\"\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let im = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Impl(im) => Some(im),
                _ => None,
            })
            .expect("impl");
        assert_eq!(im.type_name, "Int");
        assert!(im.target_args.is_empty());
    }

    #[test]
    fn expands_generic_alias_in_impl_head() {
        // Impl heads are written type positions too, so generic aliases must
        // substitute just like function parameters and return types do.
        let src = "type Row(a) = List(a)\ntrait Describe:\n    fn describe(self) -> String\nimpl Describe for Row(Int):\n    fn describe(self) -> String:\n        \"row\"\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let im = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Impl(im) => Some(im),
                _ => None,
            })
            .expect("impl");
        assert_eq!(im.type_name, "List");
        assert_eq!(im.target_args, vec![Type::Named("Int".into(), vec![])]);
    }

    #[test]
    fn expands_alias_inside_compound_types() {
        // `type Row = List(Int)` inside `List(Row)` becomes `List(List(Int))`.
        let src = "type Row = List(Int)\nfn grid(g: List(Row)) -> Int:\n    0\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        assert_eq!(
            f.params[0].ty,
            Some(Type::Named(
                "List".into(),
                vec![Type::Named(
                    "List".into(),
                    vec![Type::Named("Int".into(), vec![])]
                )]
            ))
        );
    }

    #[test]
    fn expands_generic_aliases_by_substitution() {
        // BUG-563: the parser accepted `type Pair(a) = ...` but uses such as
        // `Pair(Int)` used to survive alias resolution and then fail as an
        // unknown type. A generic alias is just transparent substitution.
        let src = "type Pair(a) = (a, a)\ntype Rows(a) = List(Pair(a))\nfn first(p: Pair(Int), rows: Rows(String)) -> Int:\n    p.0\n";
        let m = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        assert!(!m
            .items
            .iter()
            .any(|it| matches!(it, Item::TypeAlias { .. })));
        let f = m
            .items
            .iter()
            .find_map(|it| match it {
                Item::Function(f) => Some(f),
                _ => None,
            })
            .expect("function");
        assert_eq!(
            f.params[0].ty,
            Some(Type::Tuple(vec![
                Type::Named("Int".into(), vec![]),
                Type::Named("Int".into(), vec![])
            ]))
        );
        assert_eq!(
            f.params[1].ty,
            Some(Type::Named(
                "List".into(),
                vec![Type::Tuple(vec![
                    Type::Named("String".into(), vec![]),
                    Type::Named("String".into(), vec![]),
                ])],
            ))
        );
    }

    #[test]
    fn normalizes_record_composition_to_the_direct_exact_shape() {
        let src = r#"type Base = .{b: String, a: Int}
type Extended = .{..Base, c: Int}
fn same(left: Extended, right: .{c: Int, a: Int, b: String}) -> Bool:
    left == right
"#;
        let module = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("function");
        assert_eq!(function.params[0].ty, function.params[1].ty);
        assert_eq!(
            crate::format::type_str(function.params[0].ty.as_ref().expect("type")),
            ".{a: Int, b: String, c: Int}"
        );
    }

    #[test]
    fn early_composition_normalization_preserves_unrelated_alias_uses() {
        let src = r#"type Id = Int
type Base = .{a: Int}
type Extended = .{..Base, b: String}
fn keep_alias(value: Id, extended: Extended) -> Id:
    value
"#;
        let module =
            normalize_record_compositions(crate::parser::parse_module(src).expect("parse"))
                .expect("normalize compositions");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("function");

        assert_eq!(
            function.params[0].ty,
            Some(Type::Named("Id".into(), vec![]))
        );
        assert_eq!(function.ret, Some(Type::Named("Id".into(), vec![])));
        assert_eq!(
            function.params[1].ty,
            Some(Type::Named("Extended".into(), vec![]))
        );

        let extended = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias { name, ty, .. } if name == "Extended" => Some(ty),
                _ => None,
            })
            .expect("Extended alias");
        assert_eq!(crate::format::type_str(extended), ".{a: Int, b: String}");
    }

    #[test]
    fn normalizes_composition_beneath_an_ownership_qualifier() {
        let src = r#"type Base = .{a: Int}
fn inspect(value: frozen .{..Base, b: String}):
    ()
"#;
        let module =
            normalize_record_compositions(crate::parser::parse_module(src).expect("parse"))
                .expect("normalize qualified composition");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("function");
        assert_eq!(
            crate::format::type_str(function.params[0].ty.as_ref().expect("type")),
            "frozen .{a: Int, b: String}"
        );
    }

    #[test]
    fn normalizes_generic_record_composition_after_substitution() {
        let src = r#"type Value(a) = .{value: a}
type Located(a) = .{..Value(a), line: Int}
fn locate(value: Located(String)) -> .{line: Int, value: String}:
    value
"#;
        let module = resolve(crate::parser::parse_module(src).expect("parse")).expect("resolve");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("function");
        assert_eq!(function.params[0].ty, function.ret);
    }

    #[test]
    fn record_composition_collapses_identical_fields_and_rejects_conflicts() {
        let identical = crate::parser::parse_module(
            "type Base = .{a: Int}\ntype Same = .{..Base, a: Int}\nfn f(x: Same) -> .{a: Int}:\n    x\n",
        )
        .expect("parse");
        let identical = resolve(identical).expect("identical duplicate collapses");
        let function = identical
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("function");
        assert_eq!(function.params[0].ty, function.ret);

        let conflict =
            crate::parser::parse_module("type Base = .{a: Int}\ntype Bad = .{..Base, a: String}\n")
                .expect("parse");
        let error = resolve(conflict).expect_err("conflicting duplicate fails");
        assert!(error.contains("field `a` has conflicting types"), "{error}");
        assert!(error.contains("base provides `Int`"), "{error}");
        assert!(error.contains("extension declares `String`"), "{error}");
    }

    #[test]
    fn record_composition_rejects_non_record_bases_and_tracks_cycles() {
        let invalid = crate::parser::parse_module("type Bad = .{..Int, a: Int}\n").expect("parse");
        let error = resolve(invalid).expect_err("non-record base fails");
        assert!(
            error.contains("type spread requires an anonymous record shape"),
            "{error}"
        );

        let cycle =
            crate::parser::parse_module("type A = .{..B, a: Int}\ntype B = .{..A, b: Int}\n")
                .expect("parse");
        assert!(find_cycle(&cycle).is_some());
    }
}
