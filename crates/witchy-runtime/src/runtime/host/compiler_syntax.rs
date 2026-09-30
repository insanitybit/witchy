//! Stateful compiler syntax adapter, installed only by the admitted recipe
//! runner. Guest strings never mint sessions, handles or host authority.
use super::super::{comptime::SessionState, memory_of, VmState};
use std::sync::Arc;
use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::{ast::{Block, Convention, Expr, Function, ImplDef, ImplOrigin, Item, MatchArm, Param, Stmt, Type, TypeDef, Variant}, compiler_session::{
    seeds::{measure_dynamic, EmittedExpression, EmittedItem, SyntaxCost, SyntaxSeed}, ComptimeSyntaxOrigin,
}};

pub(in super::super) fn register(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_syntax_fresh_into", fresh_into)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_expr_into", quote_into::<Expression>)?;
    linker.func_wrap("witchy", "compiler_syntax_emit_expr", emit::<Expression>)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_item_into", quote_into::<ItemCategory>)?;
    linker.func_wrap("witchy", "compiler_syntax_emit_item", emit::<ItemCategory>)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_type_into", quote_into::<TypeCategory>)?;
    linker.func_wrap("witchy", "compiler_syntax_record_type_into", record_type_into)?;
    linker.func_wrap("witchy", "compiler_syntax_param_into", param_into)?;
    linker.func_wrap("witchy", "compiler_syntax_function_expr_into", function_expr_into)?;
    linker.func_wrap("witchy", "compiler_syntax_inherent_impl_into", inherent_impl_into)?;
    linker.func_wrap("witchy", "compiler_syntax_expr_leaf_into", expression_leaf_into)?;
    linker.func_wrap("witchy", "compiler_syntax_type_expr_into", type_expr_into)?;
    linker.func_wrap("witchy", "compiler_syntax_type_named_into", type_named_into)?;
    linker.func_wrap("witchy", "compiler_syntax_expr_compose_into", expression_compose_into)?;
    linker.func_wrap("witchy", "compiler_syntax_statement_into", statement_into)?;
    linker.func_wrap("witchy", "compiler_syntax_block_into", block_into)?;
    linker.func_wrap("witchy", "compiler_syntax_function_block_into", function_block_into)?;
    linker.func_wrap("witchy", "compiler_syntax_pattern_into", pattern_into)?;
    linker.func_wrap("witchy", "compiler_syntax_match_arm_into", match_arm_into)?;
    Ok(())
}

fn pattern_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler pattern input budget exceeded")); }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::pattern_match_wire::decode_pattern(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let mut ancestry = Vec::new();
    let pattern = spec.build(|handle| {
        if !state.pattern_costs.contains_key(handle) { return Err("compiler pattern requires current-session child syntax".into()); }
        let child = state.syntax.patterns().get(handle).cloned()
            .ok_or("compiler pattern child has no payload")?;
        ancestry.extend(state.syntax.pattern_origins().get(handle)
            .ok_or("compiler pattern child has no origin")?.hole_ancestry.iter().cloned());
        Ok(child)
    }).map_err(Error::msg)?;
    let cost = measure_dynamic(&pattern).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("pattern").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() { return Err(Error::msg("compiler syntax arena budget exceeded")); }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("pattern").map_err(Error::msg)?;
    state.syntax.insert_pattern(handle.clone(), pattern);
    state.syntax.insert_pattern_origin(handle.clone(), ComptimeSyntaxOrigin { definition_line: 1, hole_ancestry: ancestry });
    state.pattern_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn match_arm_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler match arm input budget exceeded")); }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::pattern_match_wire::decode_arm(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    if !state.pattern_costs.contains_key(&spec.pattern) || !state.costs.contains_key(&spec.body) {
        return Err(Error::msg("compiler match arm requires current-session syntax"));
    }
    let pattern = state.syntax.patterns().get(&spec.pattern).cloned()
        .ok_or_else(|| Error::msg("compiler match arm pattern has no payload"))?;
    let body = state.syntax.expressions().get(&spec.body).cloned()
        .ok_or_else(|| Error::msg("compiler match arm body has no payload"))?;
    let mut ancestry = state.syntax.pattern_origins().get(&spec.pattern)
        .ok_or_else(|| Error::msg("compiler match arm pattern has no origin"))?.hole_ancestry.clone();
    ancestry.extend(state.syntax.expression_origins().get(&spec.body)
        .ok_or_else(|| Error::msg("compiler match arm body has no origin"))?.hole_ancestry.iter().cloned());
    let arm = MatchArm { line: 1, pattern, guard: None, body };
    let cost = measure_dynamic(&arm).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("match-arm").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() { return Err(Error::msg("compiler syntax arena budget exceeded")); }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("match-arm").map_err(Error::msg)?;
    state.syntax.insert_match_arm(handle.clone(), arm);
    state.syntax.insert_match_arm_origin(handle.clone(), ComptimeSyntaxOrigin { definition_line: 1, hole_ancestry: ancestry });
    state.match_arm_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn statement_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    use witchy_syntax::compiler_session::block_wire::StatementSpec;
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler statement input budget exceeded")); }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::block_wire::decode_statement(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let (statement, origin) = {
        let resolve_expr = |handle: &str| -> Result<(Expr, ComptimeSyntaxOrigin)> {
            if !state.costs.contains_key(handle) { return Err(Error::msg("compiler statement requires current-session expression syntax")); }
            let expr = state.syntax.expressions().get(handle).cloned()
                .ok_or_else(|| Error::msg("compiler statement expression has no payload"))?;
            let origin = state.syntax.expression_origins().get(handle).cloned()
                .ok_or_else(|| Error::msg("compiler statement expression has no origin"))?;
            Ok((expr, origin))
        };
        match spec {
            StatementSpec::Let { mutable, name, ty, value } => {
                let (value, origin) = resolve_expr(&value)?;
                let ty = ty.map(|handle| {
                    if !state.type_costs.contains_key(&handle) { return Err(Error::msg("compiler statement requires current-session type syntax")); }
                    state.syntax.types().get(&handle).cloned()
                        .ok_or_else(|| Error::msg("compiler statement type has no payload"))
                }).transpose()?;
                (Stmt::Let { name, ty, mutable, value }, origin)
            }
            StatementSpec::Assign { name, value } => {
                let (value, origin) = resolve_expr(&value)?;
                (Stmt::Assign { place_depth: 0, name, value }, origin)
            }
            StatementSpec::LetPattern { pattern, value } => {
                if !state.pattern_costs.contains_key(&pattern) {
                    return Err(Error::msg("compiler statement requires current-session pattern syntax"));
                }
                let pattern_value = state.syntax.patterns().get(&pattern).cloned()
                    .ok_or_else(|| Error::msg("compiler statement pattern has no payload"))?;
                let pattern_origin = state.syntax.pattern_origins().get(&pattern)
                    .ok_or_else(|| Error::msg("compiler statement pattern has no origin"))?;
                let (value, mut origin) = resolve_expr(&value)?;
                origin.hole_ancestry.extend(pattern_origin.hole_ancestry.iter().cloned());
                (Stmt::LetPattern { pattern: pattern_value, value }, origin)
            }
            StatementSpec::Expr(handle) => {
                let (expr, origin) = resolve_expr(&handle)?;
                (Stmt::Expr(expr), origin)
            }
            StatementSpec::Return(handle) => {
                let (expr, origin) = resolve_expr(&handle)?;
                (Stmt::Return(Some(expr)), origin)
            }
        }
    };
    let cost = measure_dynamic(&statement).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("statement").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() { return Err(Error::msg("compiler syntax arena budget exceeded")); }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("statement").map_err(Error::msg)?;
    state.syntax.insert_statement(handle.clone(), statement);
    state.syntax.insert_statement_origin(handle.clone(), origin);
    state.statement_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn block_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler block input budget exceeded")); }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::block_wire::decode_block(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let mut statements = Vec::with_capacity(spec.statements.len() + usize::from(spec.tail.is_some()));
    let mut lines = Vec::with_capacity(statements.capacity());
    let mut ancestors = Vec::new();
    for handle in spec.statements {
        if !state.statement_costs.contains_key(&handle) { return Err(Error::msg("compiler block requires current-session statement syntax")); }
        let stmt = state.syntax.statements().get(&handle).cloned()
            .ok_or_else(|| Error::msg("compiler block statement has no payload"))?;
        let origin = state.syntax.statement_origins().get(&handle)
            .ok_or_else(|| Error::msg("compiler block statement has no origin"))?;
        ancestors.extend(origin.hole_ancestry.iter().cloned());
        lines.push(origin.definition_line);
        statements.push(stmt);
    }
    if let Some(handle) = spec.tail {
        if !state.costs.contains_key(&handle) { return Err(Error::msg("compiler block requires current-session tail expression")); }
        let expr = state.syntax.expressions().get(&handle).cloned()
            .ok_or_else(|| Error::msg("compiler block tail has no payload"))?;
        let origin = state.syntax.expression_origins().get(&handle)
            .ok_or_else(|| Error::msg("compiler block tail has no origin"))?;
        ancestors.extend(origin.hole_ancestry.iter().cloned());
        lines.push(origin.definition_line);
        statements.push(Stmt::Expr(expr));
    }
    let block = Block { stmts: statements, lines, region: None };
    let cost = measure_dynamic(&block).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("block").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() { return Err(Error::msg("compiler syntax arena budget exceeded")); }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("block").map_err(Error::msg)?;
    state.syntax.insert_block(handle.clone(), block);
    state.syntax.insert_block_origin(handle.clone(), ComptimeSyntaxOrigin { definition_line: 1, hole_ancestry: ancestors });
    state.block_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn expression_compose_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler expression input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let composition = witchy_syntax::compiler_session::expr_compose_wire::decode(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let mut origins = Vec::new();
    let mut type_origins = Vec::new();
    let mut block_origins = Vec::new();
    let mut arm_origins = Vec::new();
    let mut pattern_origins = Vec::new();
    let expression = composition.build(|handle| {
        if !state.costs.contains_key(handle) {
            return Err("compiler expression requires current-session syntax".into());
        }
        origins.push(state.syntax.expression_origins().get(handle)
            .ok_or("compiler expression handle has no origin")?.clone());
        state.syntax.expressions().get(handle).cloned()
            .ok_or("compiler expression handle has no typed payload".into())
    }, |handle| {
        if !state.type_costs.contains_key(handle) {
            return Err("compiler record requires current-session type syntax".into());
        }
        type_origins.push(state.syntax.type_origins().get(handle)
            .ok_or("compiler record type handle has no origin")?.clone());
        state.syntax.types().get(handle).cloned()
            .ok_or("compiler record type handle has no typed payload".into())
    }, |handle| {
        if !state.block_costs.contains_key(handle) {
            return Err("compiler expression requires current-session block syntax".into());
        }
        block_origins.push(state.syntax.block_origins().get(handle)
            .ok_or("compiler expression block handle has no origin")?.clone());
        state.syntax.blocks().get(handle).cloned()
            .ok_or("compiler expression block handle has no typed payload".into())
    }, |handle| {
        if !state.match_arm_costs.contains_key(handle) {
            return Err("compiler expression requires current-session match arm syntax".into());
        }
        arm_origins.push(state.syntax.match_arm_origins().get(handle)
            .ok_or("compiler expression match arm handle has no origin")?.clone());
        state.syntax.match_arms().get(handle).cloned()
            .ok_or("compiler expression match arm handle has no typed payload".into())
    }, |handle| {
        if !state.pattern_costs.contains_key(handle) {
            return Err("compiler expression requires current-session pattern syntax".into());
        }
        pattern_origins.push(state.syntax.pattern_origins().get(handle)
            .ok_or("compiler expression pattern handle has no origin")?.clone());
        state.syntax.patterns().get(handle).cloned()
            .ok_or("compiler expression pattern handle has no typed payload".into())
    }).map_err(Error::msg)?;
    let cost = measure_dynamic(&expression).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("expression").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("expression").map_err(Error::msg)?;
    state.syntax.insert_expression(handle.clone(), expression);
    state.syntax.insert_expression_origin(handle.clone(), ComptimeSyntaxOrigin {
        definition_line: 1,
        hole_ancestry: origins.into_iter().chain(type_origins).chain(block_origins).chain(arm_origins).chain(pattern_origins)
            .flat_map(|origin| origin.hole_ancestry).collect(),
    });
    state.costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn inherent_impl_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler inherent impl input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::function_wire::decode_inherent_impl(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    if !state.type_costs.contains_key(&spec.target) {
        return Err(Error::msg("compiler inherent impl requires current-session target syntax"));
    }
    let target = state.syntax.types().get(&spec.target)
        .ok_or_else(|| Error::msg("compiler inherent impl target has no typed payload"))?;
    let Type::Named(type_name, target_args) = target else {
        return Err(Error::msg("compiler inherent impl target must name a concrete type"));
    };
    let mut origin = state.syntax.type_origins().get(&spec.target).cloned();
    let methods = spec.methods.iter().map(|handle| {
        if !state.item_costs.contains_key(handle) {
            return Err(Error::msg("compiler inherent impl requires current-session function items"));
        }
        if origin.is_none() { origin = state.item_origins.get(handle).cloned(); }
        match state.syntax.items().get(handle) {
            Some(Item::Function(function)) if !function.is_async && !function.is_gen => Ok(function.clone()),
            Some(Item::Function(_)) => Err(Error::msg("compiler inherent impl cannot attach async or generator methods")),
            _ => Err(Error::msg("compiler inherent impl method is not a function item")),
        }
    }).collect::<Result<Vec<_>>>()?;
    let item = Item::Impl(ImplDef {
        origin: ImplOrigin::Source, trait_name: None, trait_args: Vec::new(),
        type_name: type_name.clone(), target_args: target_args.clone(),
        bounds: Vec::new(), methods,
    });
    let cost = measure_dynamic(&item).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("item").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("item").map_err(Error::msg)?;
    state.syntax.insert_item(handle.clone(), item);
    state.item_origins.insert(handle.clone(), origin.unwrap_or(ComptimeSyntaxOrigin {
        definition_line: 1, hole_ancestry: Vec::new(),
    }));
    state.item_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn param_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler parameter input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::function_wire::decode_param(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    if !state.type_costs.contains_key(&spec.ty) {
        return Err(Error::msg("compiler parameter requires current-session type syntax"));
    }
    let ty = state.syntax.types().get(&spec.ty).cloned()
        .ok_or_else(|| Error::msg("compiler parameter type handle has no typed payload"))?;
    let convention = match spec.convention.as_str() {
        "default" => Convention::Let, "let" => Convention::Borrow,
        "var" => Convention::Var, "own" => Convention::Own,
        _ => unreachable!("decoder checked convention"),
    };
    let param = Param { name: spec.name, ty: Some(ty), convention, default: None };
    let cost = measure_dynamic(&param).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("parameter").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("parameter").map_err(Error::msg)?;
    state.syntax.insert_param(handle.clone(), param);
    state.param_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn function_expr_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler function input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::function_wire::decode_function(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    if spec.is_async { return Err(Error::msg("meta.function expression body cannot be async")); }
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let params = spec.params.iter().map(|handle| {
        if !state.param_costs.contains_key(handle) {
            return Err(Error::msg("compiler function requires current-session parameter syntax"));
        }
        state.syntax.params().get(handle).cloned()
            .ok_or_else(|| Error::msg("compiler function parameter has no typed payload"))
    }).collect::<Result<Vec<_>>>()?;
    let ret = spec.ret.as_ref().map(|handle| {
        if !state.type_costs.contains_key(handle) {
            return Err(Error::msg("compiler function requires current-session return type syntax"));
        }
        state.syntax.types().get(handle).cloned()
            .ok_or_else(|| Error::msg("compiler function return type has no typed payload"))
    }).transpose()?;
    if !state.costs.contains_key(&spec.body) {
        return Err(Error::msg("compiler function requires a current-session expression body"));
    }
    let body = state.syntax.expressions().get(&spec.body).cloned()
        .ok_or_else(|| Error::msg("compiler function body has no typed payload"))?;
    let origin = state.syntax.expression_origins().get(&spec.body).cloned()
        .ok_or_else(|| Error::msg("compiler function body has no origin"))?;
    let item = Item::Function(Function {
        line: 1, public: spec.public, pure: false, comptime_only: false,
        attributes: Vec::new(), name: spec.name, type_params: Vec::new(),
        params, ret, body: Block { stmts: vec![Stmt::Expr(body)], lines: vec![1], region: None },
        bounds: Vec::new(), is_gen: false, is_async: false,
    });
    let cost = measure_dynamic(&item).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("item").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("item").map_err(Error::msg)?;
    state.syntax.insert_item(handle.clone(), item);
    state.item_origins.insert(handle.clone(), origin);
    state.item_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn function_block_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler function input budget exceeded")); }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let spec = witchy_syntax::compiler_session::function_wire::decode_function(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let params = spec.params.iter().map(|handle| {
        if !state.param_costs.contains_key(handle) {
            return Err(Error::msg("compiler function requires current-session parameter syntax"));
        }
        state.syntax.params().get(handle).cloned()
            .ok_or_else(|| Error::msg("compiler function parameter has no typed payload"))
    }).collect::<Result<Vec<_>>>()?;
    if spec.is_async && params.iter().any(|param| param.convention == witchy_syntax::ast::Convention::Var) {
        return Err(Error::msg("meta.function_block cannot give an async function a var parameter"));
    }
    let ret = spec.ret.as_ref().map(|handle| {
        if !state.type_costs.contains_key(handle) {
            return Err(Error::msg("compiler function requires current-session return type syntax"));
        }
        state.syntax.types().get(handle).cloned()
            .ok_or_else(|| Error::msg("compiler function return type has no typed payload"))
    }).transpose()?;
    if !state.block_costs.contains_key(&spec.body) {
        return Err(Error::msg("compiler function requires a current-session block body"));
    }
    let body = state.syntax.blocks().get(&spec.body).cloned()
        .ok_or_else(|| Error::msg("compiler function block has no typed payload"))?;
    if body.region.is_some() { return Err(Error::msg("meta.function_block requires an ordinary block")); }
    let origin = state.syntax.block_origins().get(&spec.body).cloned()
        .ok_or_else(|| Error::msg("compiler function block has no origin"))?;
    let item = Item::Function(Function {
        line: 1, public: spec.public, pure: false, comptime_only: false,
        attributes: Vec::new(), name: spec.name, type_params: Vec::new(),
        params, ret, body, bounds: Vec::new(), is_gen: false, is_async: spec.is_async,
    });
    let cost = measure_dynamic(&item).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("item").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() { return Err(Error::msg("compiler syntax arena budget exceeded")); }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("item").map_err(Error::msg)?;
    state.syntax.insert_item(handle.clone(), item);
    state.item_origins.insert(handle.clone(), origin);
    state.item_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn type_expr_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("reflected type input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let ty = witchy_syntax::compiler_session::type_expr_wire::decode(&memory.data(&caller)[input])
        .map_err(Error::msg)?;
    let cost = measure_dynamic(&ty).map_err(Error::msg)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let required = state.syntax.syntax_handle_byte_len("type").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("type").map_err(Error::msg)?;
    state.syntax.insert_type(handle.clone(), ty);
    state.syntax.insert_type_origin(handle.clone(), ComptimeSyntaxOrigin {
        definition_line: 1, hole_ancestry: Vec::new(),
    });
    state.type_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn type_named_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler named type input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let wire = memory.data(&caller)[input].to_vec();
    let (call_site, name, handles) = decode_named_type_wire(&wire)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let mut args = Vec::with_capacity(handles.len());
    let mut ancestry = Vec::new();
    for handle in handles {
        if !state.type_costs.contains_key(handle) {
            return Err(Error::msg("compiler named type requires current-session child syntax"));
        }
        args.push(state.syntax.types().get(handle).cloned()
            .ok_or_else(|| Error::msg("compiler named type child has no typed payload"))?);
        ancestry.extend(state.syntax.type_origins().get(handle)
            .ok_or_else(|| Error::msg("compiler named type child has no origin"))?
            .hole_ancestry.iter().cloned());
    }
    let ty = if call_site {
        witchy_syntax::linker::call_site_type(name, args)
    } else {
        Type::Named(name.to_owned(), args)
    };
    let cost = measure_dynamic(&ty).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("type").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("type").map_err(Error::msg)?;
    state.syntax.insert_type(handle.clone(), ty);
    state.syntax.insert_type_origin(handle.clone(), ComptimeSyntaxOrigin {
        definition_line: 1, hole_ancestry: ancestry,
    });
    state.type_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn decode_named_type_wire(wire: &[u8]) -> Result<(bool, &str, Vec<&str>)> {
    fn line<'a>(wire: &mut &'a [u8]) -> Result<&'a str> {
        let end = wire.iter().position(|byte| *byte == b'\n')
            .ok_or_else(|| Error::msg("malformed compiler named type data"))?;
        let value = std::str::from_utf8(&wire[..end])
            .map_err(|_| Error::msg("compiler named type data is not UTF-8"))?;
        *wire = &wire[end + 1..];
        Ok(value)
    }
    fn framed<'a>(wire: &mut &'a [u8]) -> Result<&'a str> {
        let length = line(wire)?.parse::<usize>()
            .map_err(|_| Error::msg("invalid compiler named type field length"))?;
        if length > wire.len() { return Err(Error::msg("truncated compiler named type field")); }
        let value = std::str::from_utf8(&wire[..length])
            .map_err(|_| Error::msg("compiler named type field is not UTF-8"))?;
        *wire = &wire[length..];
        Ok(value)
    }
    let mut remaining = wire;
    let call_site = match line(&mut remaining)? {
        "D" => false,
        "C" => true,
        _ => return Err(Error::msg("invalid compiler named type origin")),
    };
    let name = framed(&mut remaining)?;
    if name.is_empty() || name.len() > 1024 || !name.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }) { return Err(Error::msg("invalid compiler named type name")); }
    let count = line(&mut remaining)?.parse::<usize>()
        .map_err(|_| Error::msg("invalid compiler named type argument count"))?;
    if count > 256 { return Err(Error::msg("compiler named type argument limit exceeded")); }
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count { handles.push(framed(&mut remaining)?); }
    if !remaining.is_empty() { return Err(Error::msg("trailing compiler named type data")); }
    Ok((call_site, name, handles))
}

// This is a length-framed data protocol, not Witchy source. Every type operand
// must be an instance already issued by this exact compiler session.
fn record_type_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), pointer, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    if input.len() as u64 > state.input_bytes {
        return Err(Error::msg("compiler record input budget exceeded"));
    }
    charge_fuel(&mut caller, input.len() as u64 + 1)?;
    let wire = memory.data(&caller)[input].to_vec();
    let (name, fields) = decode_record_wire(&wire)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let mut seen = std::collections::BTreeSet::new();
    let mut names = Vec::with_capacity(fields.len());
    let mut types = Vec::with_capacity(fields.len());
    let mut origin = None;
    for (field_name, handle) in fields {
        if !seen.insert(field_name.to_owned()) { return Err(Error::msg("duplicate compiler record field")); }
        if !state.type_costs.contains_key(handle) {
            return Err(Error::msg("compiler record requires current-session type syntax"));
        }
        let ty = state.syntax.types().get(handle)
            .ok_or_else(|| Error::msg("compiler record type handle has no typed payload"))?;
        origin.get_or_insert_with(|| state.syntax.type_origins().get(handle).expect("type and origin inserted together").clone());
        names.push(field_name.to_owned());
        types.push(ty.clone());
    }
    let item = Item::Type(TypeDef {
        name: name.to_owned(), must_consume: false, params: Vec::new(),
        variants: vec![Variant { name: name.to_owned(), line: 0,
            field_lines: vec![0; names.len()], field_names: names, fields: types }],
        derives: Vec::new(), sealed: false, is_capability: false, grantable: false,
        packed: false, partial_eq_derived: false, public_state_derived: false,
    });
    let cost = measure_dynamic(&item).map_err(Error::msg)?;
    let required = state.syntax.syntax_handle_byte_len("item").map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|work| work.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("item").map_err(Error::msg)?;
    state.syntax.insert_item(handle.clone(), item);
    state.item_origins.insert(handle.clone(), origin.unwrap_or(ComptimeSyntaxOrigin {
        definition_line: 1, hole_ancestry: Vec::new(),
    }));
    state.item_costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn decode_record_wire(wire: &[u8]) -> Result<(&str, Vec<(&str, &str)>)> {
    fn line<'a>(wire: &mut &'a [u8]) -> Result<&'a str> {
        let end = wire.iter().position(|byte| *byte == b'\n')
            .ok_or_else(|| Error::msg("malformed compiler record data"))?;
        let value = std::str::from_utf8(&wire[..end]).map_err(|_| Error::msg("compiler record data is not UTF-8"))?;
        *wire = &wire[end + 1..];
        Ok(value)
    }
    fn identifier(value: &str) -> bool {
        let mut bytes = value.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && value != "_" && !value.contains("__") && value.len() <= 1024
    }
    let mut remaining = wire;
    let name = line(&mut remaining)?;
    if !identifier(name) { return Err(Error::msg("invalid compiler record name")); }
    let count = line(&mut remaining)?.parse::<usize>()
        .map_err(|_| Error::msg("invalid compiler record field count"))?;
    if count > 256 { return Err(Error::msg("compiler record field limit exceeded")); }
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let field = line(&mut remaining)?;
        if !identifier(field) { return Err(Error::msg("invalid compiler record field name")); }
        let length = line(&mut remaining)?.parse::<usize>()
            .map_err(|_| Error::msg("invalid compiler record type handle length"))?;
        if length > remaining.len() { return Err(Error::msg("truncated compiler record type handle")); }
        let handle = std::str::from_utf8(&remaining[..length])
            .map_err(|_| Error::msg("compiler record type handle is not UTF-8"))?;
        remaining = &remaining[length..];
        fields.push((field, handle));
    }
    if !remaining.is_empty() { return Err(Error::msg("trailing compiler record data")); }
    Ok((name, fields))
}

fn expression_leaf_into(mut caller: Caller<'_, VmState>, kind_ptr: i32, kind_len: i32,
    payload_ptr: i32, payload_len: i32, out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let kind_range = range(memory.data_size(&caller), kind_ptr, kind_len)?;
    let payload_range = range(memory.data_size(&caller), payload_ptr, payload_len)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    let input_len = kind_range.len().checked_add(payload_range.len())
        .ok_or_else(|| Error::msg("compiler expression leaf input overflow"))?;
    if kind_range.len() > 16 || input_len as u64 > state.input_bytes {
        return Err(Error::msg("compiler expression leaf input budget exceeded"));
    }
    let required = state.syntax.syntax_handle_byte_len("expression").map_err(Error::msg)?;
    let cost = SyntaxCost { nodes: 4, bytes: payload_range.len() as u64 + 64 };
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    let work = (input_len as u64).checked_add(required as u64)
        .and_then(|n| n.checked_add(cost.work()?))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?;
    charge_fuel(&mut caller, work)?;
    let bytes = memory.data(&caller);
    let kind = std::str::from_utf8(&bytes[kind_range])
        .map_err(|_| Error::msg("compiler expression leaf kind is not UTF-8"))?;
    let payload = std::str::from_utf8(&bytes[payload_range])
        .map_err(|_| Error::msg("compiler expression leaf payload is not UTF-8"))?;
    let expression = match kind {
        "int" => Expr::Int(payload.parse().map_err(|_| Error::msg("meta.expr_int carried an invalid Int payload"))?),
        "duration_ms" => Expr::Duration(payload.parse().map_err(|_| Error::msg("meta.expr_duration_ms carried an invalid Int payload"))?),
        "float" => {
            let value: f64 = payload.parse().map_err(|_| Error::msg("meta.expr_float carried an invalid Float payload"))?;
            if !value.is_finite() { return Err(Error::msg("meta.expr_float requires a finite Float value")); }
            Expr::Float(value)
        }
        "bool" => Expr::Bool(payload.parse().map_err(|_| Error::msg("meta.expr_bool carried an invalid Bool payload"))?),
        "string" => Expr::Str(payload.to_owned()),
        "name" | "call-site-name" => {
            let mut chars = payload.chars();
            if !chars.next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                || payload == "_" || payload.contains("__") {
                return Err(Error::msg("meta expression name is not a valid identifier"));
            }
            if kind == "call-site-name" {
                witchy_syntax::linker::call_site_expr(payload)
            } else if payload.chars().next().is_some_and(char::is_uppercase) {
                Expr::Ctor { name: payload.to_owned(), args: Vec::new() }
            } else { Expr::Var(payload.to_owned()) }
        }
        _ => return Err(Error::msg("unknown compiler-owned expression leaf")),
    };
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let handle = state.syntax.next_syntax_handle("expression").map_err(Error::msg)?;
    state.syntax.insert_expression(handle.clone(), expression);
    state.syntax.insert_expression_origin(handle.clone(), ComptimeSyntaxOrigin {
        definition_line: 0, hole_ancestry: Vec::new(),
    });
    state.costs.insert(handle.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(handle.as_bytes());
    Ok(required as i32)
}

fn charge_fuel(caller: &mut Caller<'_, VmState>, work: u64) -> Result<()> {
    let remaining = caller.get_fuel()?.checked_sub(work)
        .ok_or_else(|| Error::msg("compiler syntax fuel exhausted"))?;
    caller.set_fuel(remaining)
}

fn read_handle(caller: &mut Caller<'_, VmState>, pointer: i32, length: i32) -> Result<String> {
    let memory = memory_of(caller)?;
    let input = range(memory.data_size(&*caller), pointer, length)?;
    let state = caller.data().compiler_syntax.as_ref().ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    // Reading an issued handle does not consume output quota again. Bound
    // each temporary input independently, then charge all copying to fuel.
    if input.len() as u64 > state.input_bytes { return Err(Error::msg("compiler syntax handle budget exceeded")); }
    charge_fuel(caller, input.len() as u64 + 1)?;
    Ok(std::str::from_utf8(&memory.data(&*caller)[input])
        .map_err(|_| Error::msg("compiler syntax handle is not UTF-8"))?.to_owned())
}

// A category selects only typed host-owned arenas. The budget, fuel, site and
// current-session checks are deliberately shared across every syntax kind.
trait Category {
    type Value: Clone;
    const NAME: &'static str;
    fn seed(state: &SessionState, handle: &str) -> Option<Arc<SyntaxSeed<Self::Value>>>;
    fn insert(state: &mut SessionState, handle: String, value: Self::Value,
        origin: ComptimeSyntaxOrigin, cost: SyntaxCost);
    fn cost(state: &SessionState, handle: &str) -> Option<SyntaxCost>;
    fn emit(state: &mut SessionState, handle: &str);
}

struct Expression;
impl Category for Expression {
    type Value = Expr;
    const NAME: &'static str = "expression";
    fn seed(state: &SessionState, handle: &str) -> Option<Arc<SyntaxSeed<Expr>>> {
        state.seeds.expression(handle).cloned()
    }
    fn insert(state: &mut SessionState, handle: String, value: Expr,
        origin: ComptimeSyntaxOrigin, cost: SyntaxCost) {
        state.syntax.insert_expression(handle.clone(), value);
        state.syntax.insert_expression_origin(handle.clone(), origin);
        state.costs.insert(handle, cost);
    }
    fn cost(state: &SessionState, handle: &str) -> Option<SyntaxCost> { state.costs.get(handle).copied() }
    fn emit(state: &mut SessionState, handle: &str) {
        let expression = state.syntax.expressions().get(handle).expect("typed instance and cost inserted together").clone();
        let origin = state.syntax.expression_origins().get(handle).expect("typed instance and origin inserted together").clone();
        state.expressions.push(EmittedExpression { expression, origin });
    }
}

struct ItemCategory;
impl Category for ItemCategory {
    type Value = Item;
    const NAME: &'static str = "item";
    fn seed(state: &SessionState, handle: &str) -> Option<Arc<SyntaxSeed<Item>>> {
        state.seeds.item(handle).cloned()
    }
    fn insert(state: &mut SessionState, handle: String, value: Item,
        origin: ComptimeSyntaxOrigin, cost: SyntaxCost) {
        state.syntax.insert_item(handle.clone(), value);
        state.item_origins.insert(handle.clone(), origin);
        state.item_costs.insert(handle, cost);
    }
    fn cost(state: &SessionState, handle: &str) -> Option<SyntaxCost> { state.item_costs.get(handle).copied() }
    fn emit(state: &mut SessionState, handle: &str) {
        let item = state.syntax.items().get(handle).expect("typed instance and cost inserted together").clone();
        let origin = state.item_origins.get(handle).expect("typed instance and origin inserted together").clone();
        state.items.push(EmittedItem { item, origin });
    }
}

struct TypeCategory;
impl Category for TypeCategory {
    type Value = Type;
    const NAME: &'static str = "type";
    fn seed(state: &SessionState, handle: &str) -> Option<Arc<SyntaxSeed<Type>>> {
        state.seeds.ty(handle).cloned()
    }
    fn insert(state: &mut SessionState, handle: String, value: Type,
        origin: ComptimeSyntaxOrigin, cost: SyntaxCost) {
        state.syntax.insert_type(handle.clone(), value);
        state.syntax.insert_type_origin(handle.clone(), origin);
        state.type_costs.insert(handle, cost);
    }
    fn cost(state: &SessionState, handle: &str) -> Option<SyntaxCost> { state.type_costs.get(handle).copied() }
    fn emit(_: &mut SessionState, _: &str) { unreachable!("types are operands, not emitted items") }
}

fn quote_into<C: Category>(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    site: i64, out: i32, capacity: i32) -> Result<i32> {
    let site = u32::try_from(site).ok().filter(|line| *line != 0)
        .ok_or_else(|| Error::msg("compiler quotation source line is out of range"))?;
    let handle = read_handle(&mut caller, pointer, length)?;
    let memory = memory_of(&mut caller)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let seed = C::seed(state, &handle)
        .ok_or_else(|| Error::msg(format!("compiler quotation has no checked {} seed", C::NAME)))?;
    if !seed.allows_site(site) { return Err(Error::msg("compiler quotation site is absent from checked source")); }
    let cost = seed.cost();
    let required = state.syntax.syntax_handle_byte_len(C::NAME).map_err(Error::msg)?;
    if cost.nodes > state.nodes.remaining() || cost.bytes > state.syntax_bytes.remaining()
        || required as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax arena budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().and_then(|n| n.checked_add(required as u64))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    if output.len() < required { return Ok(required as i32); }
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.syntax_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax arena budget exceeded"))?;
    state.bytes.charge(required as u64).map_err(|_| Error::msg("compiler syntax handle budget exceeded"))?;
    let instance = state.syntax.next_syntax_handle(C::NAME).map_err(Error::msg)?;
    C::insert(state, instance.clone(), seed.value().clone(),
        ComptimeSyntaxOrigin { definition_line: site, hole_ancestry: Vec::new() }, cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(instance.as_bytes());
    Ok(required as i32)
}

fn emit<C: Category>(mut caller: Caller<'_, VmState>, pointer: i32, length: i32) -> Result<()> {
    let handle = read_handle(&mut caller, pointer, length)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let cost = C::cost(state, &handle)
        .ok_or_else(|| Error::msg(format!("compiler emission requires a current-session {}", C::NAME)))?;
    if cost.nodes > state.emitted_nodes.remaining() || cost.bytes > state.emitted_bytes.remaining() {
        return Err(Error::msg("compiler syntax emission budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.emitted_nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax emission budget exceeded"))?;
    state.emitted_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax emission budget exceeded"))?;
    C::emit(state, &handle);
    Ok(())
}

fn range(size: usize, pointer: i32, length: i32) -> Result<std::ops::Range<usize>> {
    let start = pointer as u32 as usize;
    let end = start.checked_add(length as u32 as usize)
        .filter(|end| *end <= size).ok_or_else(|| Error::msg("compiler syntax guest buffer out of bounds"))?;
    Ok(start..end)
}

fn fresh_into(mut caller: Caller<'_, VmState>, hint: i32, length: i32, out: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    let input = range(memory.data_size(&caller), hint, length)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref()
        .ok_or_else(|| Error::msg("compiler syntax session unavailable"))?;
    // Bound host allocation before copying any untrusted guest input.
    if input.len() as u64 > state.bytes.remaining() || state.names.remaining() == 0 {
        return Err(Error::msg("compiler syntax output budget exceeded"));
    }
    let name_bytes = state.syntax.fresh_ident_byte_len(input.len()).map_err(Error::msg)?;
    if name_bytes as u64 > state.bytes.remaining() {
        return Err(Error::msg("compiler syntax output budget exceeded"));
    }
    let required = u32::try_from(name_bytes).map_err(|_| Error::msg("compiler syntax result exceeds wasm32"))?;
    let work = (input.len() as u64).checked_add(name_bytes as u64).and_then(|n| n.checked_add(1))
        .ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?;
    let remaining = caller.get_fuel()?.checked_sub(work)
        .ok_or_else(|| Error::msg("compiler syntax fuel exhausted"))?;
    caller.set_fuel(remaining)?;
    // The metering above precedes UTF-8 scanning, guest copying and formatting.
    let hint = std::str::from_utf8(&memory.data(&caller)[input])
        .map_err(|_| Error::msg("compiler syntax hint is not UTF-8"))?;
    if output.len() < name_bytes { return Ok(required as i32); }
    let hint = hint.to_owned();
    // No partial write, counter advance or budget debit on an undersized probe.
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.names.charge(1).map_err(|_| Error::msg("compiler syntax name budget exceeded"))?;
    state.bytes.charge(name_bytes as u64).map_err(|_| Error::msg("compiler syntax output budget exceeded"))?;
    let committed = state.syntax.next_fresh_ident(&hint).map_err(Error::msg)?;
    debug_assert_eq!(name_bytes, committed.len());
    memory.data_mut(&mut caller)[output.start..output.start + name_bytes].copy_from_slice(committed.as_bytes());
    caller.data_mut().compiler_syntax.as_mut().expect("checked session").issued.push(committed);
    Ok(required as i32)
}
