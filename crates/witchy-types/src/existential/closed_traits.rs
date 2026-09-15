//! Closed trait values use ordinary sums and catalog-checked method adapters.
//! This is source preparation; it does not mint backend or import authority.
use super::*;
use witchy_syntax::ast::{Convention, MatchArm, Pattern, TypeDef, Variant};
use witchy_syntax::ast::visit::{visit_block, visit_block_mut, visit_expr_mut};

fn variant(id: u32) -> String { format!("$canonical.TraitValue_{id}") }
fn ctor(id: u32, value: Expr) -> Expr {
    Expr::Ctor { name: variant(id), args: vec![value] }
}
fn arm(id: u32, body: Expr) -> MatchArm {
    MatchArm {
        line: u32::MAX,
        pattern: Pattern::Ctor {
            name: variant(id), args: vec![Pattern::Var("$closed_trait_payload".into())],
        },
        guard: None, body,
    }
}

pub(crate) fn prepare(typed: TypedModule, catalog: &WitnessCatalog) -> Result<(TypedModule, bool), String> {
    let mut scope = (*crate::pipeline::canonical_scope(typed.module())).clone();
    let functions = typed.module().items.iter().filter_map(|item| match item {
        Item::Function(function) => Some(function.name.clone()),
        _ => None,
    }).collect::<BTreeSet<_>>();
    // Adapter roots come from the retained checked declarations. Include their
    // real bodies and dependencies, not an allowlist of generated spellings.
    let witnesses = loop {
        let mut requests = Vec::new();
        let mut upcasts = Vec::new();
        let mut next = scope.clone();
        for item in &typed.module().items {
            if let Item::Function(function) = item && scope.contains(&function.name)
                && !crate::pipeline::function_has_free_signature_variables(function)
            {
                visit_block::<String>(&function.body, &mut |expr| {
                    if let Some(request) = pack_request(typed.table(), expr)? { requests.push(request); }
                    if let Some(request) = upcast_request(typed.table(), expr)? { upcasts.push(request); }
                    if let Expr::Call { name, .. } | Expr::Var(name) = expr
                        && functions.contains(name)
                    {
                        next.insert(name.clone());
                    }
                    Ok(())
                })?;
            }
        }
        let plan = witness::build_from_catalog_with_upcasts(catalog, requests, upcasts)?;
        if plan.witnesses.is_empty() {
            // An open parameter or forwarding body provides no closed payload
            // evidence. Preserve it for the existing canonical layout refusal.
            return Ok((typed, false));
        }
        for witness in &plan.witnesses {
            for slot in &witness.slots {
                if !functions.contains(&slot.adapter) {
                    return Err(format!("closed trait witness has no checked adapter body `{}`", slot.adapter));
                }
                next.insert(slot.adapter.clone());
            }
        }
        if next == scope { break plan; }
        scope = next;
    };
    let mut groups: Vec<(Type, String)> = Vec::new();
    for witness in &witnesses.witnesses {
        if !groups.iter().any(|(ty, _)| ty == &witness.existential) {
            groups.push((witness.existential.clone(), format!("$canonical.TraitPayload_{}", groups.len())));
        }
    }
    for item in &typed.module().items {
        if let Item::Type(definition) = item
            && (groups.iter().any(|(_, name)| name == &definition.name)
                || definition.variants.iter().any(|v| witnesses.witnesses.iter().any(|w| v.name == variant(w.id))))
        {
            return Err("closed trait payload declaration collision".into());
        }
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
    module.items.retain(|item| match item {
        Item::Function(function) => scope.contains(&function.name),
        Item::Comptime(_) => false,
        _ => true,
    });
    // Refresh expression types after introducing explicit packs and upcasts;
    // an upcast consumes its checked source type, never a guessed source tag.
    let typed = crate::typeck::annotate_checked(module).map_err(|error| error.to_string())?;
    let (mut module, _, result) = typed.rewrite_into_module(|table, module| {
        for item in &mut module.items {
            if let Item::Function(function) = item {
                visit_block_mut(&mut function.body, &mut |expr| close_expr(expr, table, &witnesses))?;
            }
        }
        Ok::<_, String>(())
    });
    result?;
    for (existential, name) in &groups {
        module.items.push(Item::Type(TypeDef {
            name: name.clone(), must_consume: false, params: vec![], derives: vec![],
            sealed: false, is_capability: false, grantable: false, packed: false,
            partial_eq_derived: false, public_state_derived: false,
            variants: witnesses.witnesses.iter().filter(|w| &w.existential == existential).map(|w| Variant {
                name: variant(w.id), line: 0, fields: vec![w.concrete.clone()],
                field_names: vec![], field_lines: vec![],
            }).collect(),
        }));
    }
    for item in &mut module.items {
        match item {
            Item::Type(definition) => for variant in &mut definition.variants {
                for field in &mut variant.fields { replace_type(field, &groups); }
            },
            Item::Function(function) => {
                for parameter in &mut function.params {
                    if let Some(ty) = &mut parameter.ty { replace_type(ty, &groups); }
                }
                if let Some(ty) = &mut function.ret { replace_type(ty, &groups); }
                replace_block_types(&mut function.body, &groups);
            }
            Item::TypeAlias { ty, .. } => replace_type(ty, &groups),
            _ => {}
        }
    }
    let typed = crate::typeck::annotate_checked(module).map_err(|error| error.to_string())?;
    Ok((typed, true))
}

fn close_expr(expr: &mut Expr, table: &TypeTable, plan: &WitnessPlan) -> Result<(), String> {
    match expr {
        Expr::ExistentialPack { expr: payload, ty, witness } => {
            let witness = plan.by_id(*witness).filter(|w| &w.existential == ty)
                .ok_or("closed trait pack lost its checked witness")?;
            visit_expr_mut(payload, &mut |expr| close_expr(expr, table, plan))?;
            *expr = ctor(witness.id, *payload.clone());
        }
        Expr::ExistentialCall { receiver, args, ty, owner_trait, method, slot, params, result, conventions } => {
            if conventions.contains(&Convention::Var) {
                return Err("closed trait dispatch does not support write-back slots".into());
            }
            visit_expr_mut(receiver, &mut |expr| close_expr(expr, table, plan))?;
            for argument in args.iter_mut() {
                visit_expr_mut(argument, &mut |expr| close_expr(expr, table, plan))?;
            }
            let mut arms = Vec::new();
            for witness in plan.witnesses.iter().filter(|w| &w.existential == ty.unqualified()) {
                let (index, checked) = witness.slot(owner_trait, method)
                    .ok_or("closed trait dispatch lost its checked slot")?;
                let checked_conventions = std::iter::once(checked.receiver)
                    .chain(checked.conventions.iter().copied()).collect::<Vec<_>>();
                if index != *slot || checked.params != *params || checked.result != *result
                    || checked_conventions != *conventions || args.len() != params.len()
                {
                    return Err("closed trait dispatch changed its checked slot contract".into());
                }
                let mut arguments = vec![Expr::Var("$closed_trait_payload".into())];
                arguments.extend(args.iter().cloned());
                arms.push(arm(witness.id, Expr::Call { name: checked.adapter.clone(), args: arguments }));
            }
            if arms.is_empty() { return Err("closed trait dispatch has no concrete witnesses".into()); }
            *expr = Expr::Match { scrutinee: receiver.clone(), arms };
        }
        Expr::ExistentialUpcast { expr: value, ty } => {
            let source = table.type_of(value).and_then(ty_to_ast)
                .ok_or("closed trait upcast lost its checked source type")?;
            visit_expr_mut(value, &mut |expr| close_expr(expr, table, plan))?;
            let mut arms = Vec::new();
            for witness in plan.witnesses.iter().filter(|w| &w.existential == source.unqualified()) {
                let target = plan.upcast(witness.id, ty).and_then(|id| plan.by_id(id))
                    .filter(|target| target.concrete == witness.concrete)
                    .ok_or("closed trait upcast lost its checked same-payload conversion")?;
                arms.push(arm(witness.id, ctor(target.id, Expr::Var("$closed_trait_payload".into()))));
            }
            if arms.is_empty() { return Err("closed trait upcast has no concrete witnesses".into()); }
            *expr = Expr::Match { scrutinee: value.clone(), arms };
        }
        _ => {}
    }
    Ok(())
}

fn replace_type(ty: &mut Type, groups: &[(Type, String)]) {
    if let Some((_, name)) = groups.iter().find(|(existential, _)| existential == ty) {
        *ty = Type::Named(name.clone(), vec![]);
        return;
    }
    match ty {
        Type::Named(_, types) | Type::Dyn(_, types) | Type::Tuple(types) => {
            for ty in types { replace_type(ty, groups); }
        }
        Type::Qualified(_, inner) | Type::Slice(inner) => replace_type(inner, groups),
        Type::Fn(parameters, result, _, _) => {
            for parameter in parameters { replace_type(parameter, groups); }
            replace_type(result, groups);
        }
        Type::RecordCompose { base, fields } => {
            replace_type(base, groups);
            for (_, field) in fields { replace_type(field, groups); }
        }
    }
}

fn replace_block_types(block: &mut Block, groups: &[(Type, String)]) {
    fn locals(block: &mut Block, groups: &[(Type, String)]) {
        for statement in &mut block.stmts {
            if let Stmt::Let { ty: Some(ty), .. } = statement { replace_type(ty, groups); }
        }
    }
    locals(block, groups);
    let _: Result<(), ()> = visit_block_mut(block, &mut |expr| {
        match expr {
            Expr::As { ty, .. } => replace_type(ty, groups),
            Expr::Lambda { params, ret, body, .. } => {
                for parameter in params { if let Some(ty) = &mut parameter.ty { replace_type(ty, groups); } }
                if let Some(ty) = ret { replace_type(ty, groups); }
                locals(body, groups);
            }
            Expr::Block(body) | Expr::While { body, .. } | Expr::For { body, .. }
            | Expr::WhileLet { body, .. } => locals(body, groups),
            Expr::If { then_block, else_block, .. } => {
                locals(then_block, groups);
                if let Some(body) = else_block { locals(body, groups); }
            }
            _ => {}
        }
        Ok(())
    });
}
