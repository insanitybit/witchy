//! Stateful compiler syntax adapter, installed only by the admitted recipe
//! runner. Guest strings never mint sessions, handles or host authority.
use super::super::{comptime::SessionState, memory_of, VmState};
use std::sync::Arc;
use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::{ast::{Expr, Item, Type}, compiler_session::{
    seeds::{EmittedExpression, EmittedItem, SyntaxCost, SyntaxSeed}, ComptimeSyntaxOrigin,
}};

pub(in super::super) fn register(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_syntax_fresh_into", fresh_into)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_expr_into", quote_into::<Expression>)?;
    linker.func_wrap("witchy", "compiler_syntax_emit_expr", emit::<Expression>)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_item_into", quote_into::<ItemCategory>)?;
    linker.func_wrap("witchy", "compiler_syntax_emit_item", emit::<ItemCategory>)?;
    linker.func_wrap("witchy", "compiler_syntax_quote_type_into", quote_into::<TypeCategory>)?;
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
