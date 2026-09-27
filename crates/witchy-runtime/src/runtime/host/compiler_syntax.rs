//! Stateful compiler syntax adapter, installed only by the admitted recipe
//! runner. Guest strings never mint sessions, handles or host authority.
use super::super::{memory_of, VmState};
use wasmtime::{Caller, Error, Linker, Result};

pub(in super::super) fn register(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_syntax_fresh_into", fresh_into)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_expr_into", quote_expr_into)?;
    linker.func_wrap("witchy", "compiler_syntax_emit_expr", emit_expr)?;
    Ok(())
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

fn quote_expr_into(mut caller: Caller<'_, VmState>, pointer: i32, length: i32,
    site: i64, out: i32, capacity: i32) -> Result<i32> {
    use witchy_syntax::compiler_session::ComptimeSyntaxOrigin;
    let site = u32::try_from(site).ok().filter(|line| *line != 0)
        .ok_or_else(|| Error::msg("compiler quotation source line is out of range"))?;
    let handle = read_handle(&mut caller, pointer, length)?;
    let memory = memory_of(&mut caller)?;
    let output = range(memory.data_size(&caller), out, capacity)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let seed = state.seeds.expression(&handle).cloned()
        .ok_or_else(|| Error::msg("compiler quotation has no checked expression seed"))?;
    if !seed.allows_site(site) { return Err(Error::msg("compiler quotation site is absent from checked source")); }
    let cost = seed.cost();
    let required = state.syntax.syntax_handle_byte_len("expression").map_err(Error::msg)?;
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
    let instance = state.syntax.next_syntax_handle("expression").map_err(Error::msg)?;
    state.syntax.insert_expression(instance.clone(), seed.expression().clone());
    state.syntax.insert_expression_origin(instance.clone(), ComptimeSyntaxOrigin { definition_line: site, hole_ancestry: Vec::new() });
    state.costs.insert(instance.clone(), cost);
    memory.data_mut(&mut caller)[output.start..output.start + required].copy_from_slice(instance.as_bytes());
    Ok(required as i32)
}

fn emit_expr(mut caller: Caller<'_, VmState>, pointer: i32, length: i32) -> Result<()> {
    use witchy_syntax::compiler_session::seeds::EmittedExpression;
    let handle = read_handle(&mut caller, pointer, length)?;
    let state = caller.data().compiler_syntax.as_ref().expect("checked session");
    let cost = *state.costs.get(&handle).ok_or_else(|| Error::msg("compiler emission requires a current-session expression"))?;
    if cost.nodes > state.emitted_nodes.remaining() || cost.bytes > state.emitted_bytes.remaining() {
        return Err(Error::msg("compiler syntax emission budget exceeded"));
    }
    charge_fuel(&mut caller, cost.work().ok_or_else(|| Error::msg("compiler syntax fuel overflow"))?)?;
    let state = caller.data_mut().compiler_syntax.as_mut().expect("checked session");
    state.emitted_nodes.charge(cost.nodes).map_err(|_| Error::msg("compiler syntax emission budget exceeded"))?;
    state.emitted_bytes.charge(cost.bytes).map_err(|_| Error::msg("compiler syntax emission budget exceeded"))?;
    let expression = state.syntax.expressions().get(&handle).expect("typed instance and cost inserted together").clone();
    let origin = state.syntax.expression_origins().get(&handle).expect("typed instance and origin inserted together").clone();
    state.expressions.push(EmittedExpression { expression, origin });
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
