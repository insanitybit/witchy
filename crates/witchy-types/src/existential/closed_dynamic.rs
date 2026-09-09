//! Canonical closed Dynamic storage. No backend existential box or cast is
//! required: checked witness payloads become ordinary owned sum variants.
use super::*;
use witchy_syntax::ast::{Convention, MatchArm, Pattern, Variant};
use witchy_syntax::ast::visit::{visit_block, visit_block_mut, visit_expr_mut};

const PAYLOAD: &str = "$canonical.DynamicPayload";

fn variant(id: u32) -> String { format!("$canonical.DynamicPayload_{id}") }
fn arm(pattern: Pattern, body: Expr) -> MatchArm {
    MatchArm { line: u32::MAX, pattern, guard: None, body }
}
fn ctor(name: &str, args: Vec<Expr>) -> Expr { Expr::Ctor { name: name.into(), args } }
fn pattern(name: &str, args: Vec<Pattern>) -> Pattern { Pattern::Ctor { name: name.into(), args } }

/// Only the checked-source pipeline calls this, with the plan it just built
/// from this typed allocation and its retained authenticated loader catalog.
/// This function does not mint source authority or accept a caller's receipt.
pub(crate) fn prepare(
    typed: TypedModule,
    catalog: &WitnessCatalog,
    runtime_catalog: &RuntimeDeclarationCatalog,
    plan: &RuntimeTypePlan,
) -> Result<TypedModule, String> {
    // Authenticate the complete supplied plan before even the descriptor-only
    // early return. In particular, matching descriptors cannot substitute for
    // the retained catalog's checked methods and nominal trait relations.
    let rebuilt = build_dynamic_runtime_plan(&typed, catalog, runtime_catalog)?;
    if plan != &rebuilt {
        return Err("closed Dynamic runtime plan does not match its checked source and catalogs".into());
    }
    let scope = crate::pipeline::canonical_scope(typed.module());
    let mut constructs_dynamic = false;
    for item in &typed.module().items {
        if let Item::Function(function) = item && scope.contains(&function.name) {
            visit_block::<String>(&function.body, &mut |expr| {
                constructs_dynamic |= matches!(expr, Expr::Ctor { name, .. } if name == "dynamic.Dynamic");
                Ok(())
            })?;
        }
    }
    // Descriptor-only programs retain the metadata-only path. No arbitrary
    // existential program acquires this representation merely by importing it.
    if !constructs_dynamic { return Ok(typed); }
    let mut payload_definition = typed.module().items.iter().find_map(|item| match item {
        Item::Type(definition) if definition.name == "dynamic.Dynamic" => Some(definition.clone()),
        _ => None,
    }).ok_or("closed Dynamic preparation lost its checked declaration")?;
    let existential = payload_definition.variants.first().and_then(|variant| variant.fields.get(1))
        .cloned().ok_or("closed Dynamic preparation lost its checked payload type")?;
    if !matches!(existential, Type::Dyn(..)) {
        return Err("closed Dynamic preparation requires its checked existential payload".into());
    }
    if typed.module().items.iter().any(|item| matches!(item, Item::Type(definition) if definition.name == PAYLOAD)) {
        return Err("closed Dynamic payload declaration collision".into());
    }
    let typed = crate::record_projection::lower_explicit_projections(typed)?;
    let (module, _, result) = typed.rewrite_into_module(|table, module| {
        rewrite_dynamic_module(module, table, Some(runtime_catalog), plan)
    });
    result?;
    let typed = crate::typeck::annotate_checked(module).map_err(|error| error.to_string())?;

    // Discover adapters by checked witness identity, then include their actual
    // transitive bodies. Never authorize a helper from a mangled-name pattern.
    let mut scope = (*crate::pipeline::canonical_scope(typed.module())).clone();
    let witnesses = loop {
        let mut requests = Vec::new();
        let mut upcasts = Vec::new();
        let mut next = scope.clone();
        for item in &typed.module().items {
            if let Item::Function(function) = item && scope.contains(&function.name) {
                visit_block::<String>(&function.body, &mut |expr| {
                    if let Some(request) = pack_request(typed.table(), expr)? { requests.push(request); }
                    if let Some(request) = upcast_request(typed.table(), expr)? { upcasts.push(request); }
                    if let Expr::Call { name, .. } | Expr::Var(name) = expr {
                        next.insert(name.clone());
                    }
                    Ok(())
                })?;
            }
        }
        let witnesses = witness::build_from_catalog_with_upcasts(catalog, requests, upcasts)?;
        for witness in &witnesses.witnesses {
            if witness.existential != existential {
                return Err("closed Dynamic preparation does not authorize another existential payload".into());
            }
            runtime_catalog.capability_free_type_identity(&witness.concrete, typed.module())
                .map_err(|error| error.to_string())?;
            for slot in &witness.slots { next.insert(slot.adapter.clone()); }
        }
        if next == scope { break witnesses; }
        scope = next;
    };
    if witnesses.witnesses.is_empty() {
        return Err("closed Dynamic construction has no checked concrete payloads".into());
    }
    let mut descriptors = BTreeMap::new();
    for witness in &witnesses.witnesses {
        let identity = runtime_catalog.capability_free_type_identity(&witness.concrete, typed.module())
            .map_err(|error| error.to_string())?;
        let descriptor = plan.id(&identity).ok_or("closed Dynamic payload is absent from authenticated plan")?;
        descriptors.insert(witness.id, descriptor.index());
    }
    let (mut module, _, result) = typed.rewrite_into_module(|table, module| {
        for item in &mut module.items {
            if let Item::Function(function) = item && scope.contains(&function.name) {
                rewrite_block(&mut function.body, table, &witnesses)?;
            }
        }
        Ok::<_, String>(())
    });
    result?;
    // Prune only after consuming address-keyed facts. Final ordinals, scope,
    // layouts and witnesses are all derived after this prepared module recheck.
    module.items.retain(|item| match item {
        Item::Function(function) => scope.contains(&function.name),
        Item::Comptime(_) => false,
        _ => true,
    });
    payload_definition.name = PAYLOAD.into();
    payload_definition.params.clear();
    payload_definition.derives.clear();
    payload_definition.variants = witnesses.witnesses.iter().map(|witness| Variant {
        name: variant(descriptors[&witness.id]), line: 0,
        fields: vec![witness.concrete.clone()], field_names: vec![], field_lines: vec![],
    }).collect();
    module.items.push(Item::Type(payload_definition));
    for item in &mut module.items {
        match item {
            Item::Type(definition) => for variant in &mut definition.variants {
                for field in &mut variant.fields { replace_type(field, &existential); }
            },
            Item::Function(function) => {
                for parameter in &mut function.params {
                    if let Some(ty) = &mut parameter.ty { replace_type(ty, &existential); }
                }
                if let Some(ty) = &mut function.ret { replace_type(ty, &existential); }
                replace_block_types(&mut function.body, &existential);
                visit_block_mut(&mut function.body, &mut |expr| {
                    close_expr(expr, &existential, &witnesses, &descriptors)
                })?;
            }
            Item::Const { value, .. } => visit_expr_mut(value, &mut |expr| {
                close_expr(expr, &existential, &witnesses, &descriptors)
            })?,
            Item::TypeAlias { ty, .. } => replace_type(ty, &existential),
            _ => return Err("closed Dynamic preparation requires an ordinary executable module".into()),
        }
    }
    let typed = crate::typeck::annotate_checked(module).map_err(|error| error.to_string())?;
    let facts = crate::access::checked_facts(typed.module(), typed.table()).map_err(|error| error.to_string())?;
    for method in plan.methods() {
        let signature = facts.declaration(&method.function).ok_or("prepared dynamic method lost checked signature")?;
        let identity = runtime_catalog.checked_callable_identity_with_authority(signature, typed.module())
            .map_err(|error| error.to_string())?;
        if identity != method.callable_identity {
            return Err("prepared dynamic method changed authenticated callable identity".into());
        }
    }
    Ok(typed)
}

fn close_expr(expr: &mut Expr, existential: &Type, witnesses: &WitnessPlan, descriptors: &BTreeMap<u32, u32>) -> Result<(), String> {
    match expr {
        Expr::ExistentialPack { ty, witness, expr: payload } if ty == existential => {
            let id = descriptors.get(witness).ok_or("closed Dynamic pack lost its witness")?;
            *expr = ctor(&variant(*id), vec![*payload.clone()]);
        }
        Expr::ExistentialCall { receiver, args, ty, owner_trait, method, slot, .. } if ty == existential => {
            let mut arms = Vec::new();
            for witness in &witnesses.witnesses {
                let (index, checked_slot) = witness.slot(owner_trait, method)
                    .ok_or("closed Dynamic dispatch lost its checked method slot")?;
                if index != *slot || checked_slot.conventions.iter().any(|convention| *convention == Convention::Var) {
                    return Err("closed Dynamic dispatch cannot change slot identity or write-back convention".into());
                }
                let value = "$closed_dynamic_receiver";
                let mut arguments = vec![Expr::Var(value.into())];
                arguments.extend(args.iter().cloned());
                arms.push(arm(pattern(&variant(descriptors[&witness.id]), vec![Pattern::Var(value.into())]),
                    Expr::Call { name: checked_slot.adapter.clone(), args: arguments }));
            }
            *expr = Expr::Match { scrutinee: receiver.clone(), arms };
        }
        Expr::Call { name, args } if name == intrinsics::DYNAMIC_TRY_DECODE_TYPED => {
            let [value, Expr::Int(expected)] = args.as_slice() else {
                return Err("closed Dynamic decode requires its authenticated constant descriptor".into());
            };
            let mut arms = Vec::new();
            if let Some((_, id)) = descriptors.iter().find(|(_, id)| i64::from(**id) == *expected) {
                arms.push(arm(pattern("dynamic.Dynamic", vec![
                    pattern("dynamic.RuntimeType", vec![Pattern::Int(*expected), Pattern::Wildcard]),
                    pattern(&variant(*id), vec![Pattern::Var("$closed_dynamic_decoded".into())]),
                ]), ctor("Some", vec![Expr::Var("$closed_dynamic_decoded".into())])));
            }
            arms.push(arm(Pattern::Wildcard, ctor("None", vec![])));
            *expr = Expr::Match { scrutinee: Box::new(value.clone()), arms };
        }
        Expr::ExistentialPack { .. } | Expr::ExistentialCall { .. } | Expr::ExistentialUpcast { .. } => {
            return Err("closed Dynamic preparation left an unsupported existential operation".into());
        }
        _ => {}
    }
    Ok(())
}

fn replace_type(ty: &mut Type, existential: &Type) {
    if ty == existential { *ty = Type::Named(PAYLOAD.into(), vec![]); return; }
    match ty {
        Type::Named(_, types) | Type::Dyn(_, types) | Type::Tuple(types) => {
            for ty in types { replace_type(ty, existential); }
        }
        Type::Qualified(_, ty) | Type::Slice(ty) => replace_type(ty, existential),
        Type::Fn(params, result, _, _) => {
            for ty in params { replace_type(ty, existential); }
            replace_type(result, existential);
        }
        Type::RecordCompose { base, fields } => {
            replace_type(base, existential);
            for (_, ty) in fields { replace_type(ty, existential); }
        }
    }
}

fn replace_block_types(block: &mut Block, existential: &Type) {
    fn local_types(block: &mut Block, existential: &Type) {
        for statement in &mut block.stmts {
            if let Stmt::Let { ty: Some(ty), .. } = statement { replace_type(ty, existential); }
        }
    }
    local_types(block, existential);
    let _: Result<(), ()> = visit_block_mut(block, &mut |expr| {
        match expr {
            Expr::As { ty, .. } => replace_type(ty, existential),
            Expr::Lambda { params, ret, body, .. } => {
                for parameter in params { if let Some(ty) = &mut parameter.ty { replace_type(ty, existential); } }
                if let Some(ty) = ret { replace_type(ty, existential); }
                local_types(body, existential);
            }
            Expr::Block(body) | Expr::While { body, .. } | Expr::For { body, .. }
            | Expr::WhileLet { body, .. } => local_types(body, existential),
            Expr::If { then_block, else_block, .. } => {
                local_types(then_block, existential);
                if let Some(body) = else_block { local_types(body, existential); }
            }
            _ => {}
        }
        Ok(())
    });
}
