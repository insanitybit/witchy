//! Stateful compiler syntax adapter, installed only by the admitted recipe
//! runner. Guest strings never mint sessions, handles or host authority.
use super::super::{memory_of, VmState};
use wasmtime::{Caller, Error, Linker, Result};

pub(in super::super) fn register(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_syntax_fresh_into", fresh_into)?;
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
