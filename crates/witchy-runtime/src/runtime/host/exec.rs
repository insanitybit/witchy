//! Bounded-slice Exec adapter. Core admission/catalog wiring is separate:
//! these imports confer no grant beyond the existing exact Exec/Dir tokens.
use std::sync::Arc;
use wasmtime::{Caller, Error, ExternRef, Linker, Result, Rooted};
use super::{dir_authority_ref, dir_require_read, exec_authority_ref, memory_of, run_exec, VmState};

/// Immutable process result, not an Exec grant or a caller-chosen token ID.
struct ExecResponse(Arc<[u8]>);

pub(super) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "exec_run_at", run_at)?;
    linker.func_wrap("witchy", "exec_response_size", response_size)?;
    linker.func_wrap("witchy", "exec_response_into", response_into)?;
    Ok(())
}

fn range(ptr: i32, len: i32, limit: usize) -> Result<std::ops::Range<usize>> {
    let start = ptr as u32 as usize;
    let end = start.checked_add(len as u32 as usize)
        .filter(|end| *end <= limit)
        .ok_or_else(|| Error::msg("Exec slice lies outside guest memory"))?;
    Ok(start..end)
}

fn text(data: &[u8], ptr: i32, len: i32) -> Result<String> {
    let bytes = &data[range(ptr, len, data.len())?];
    std::str::from_utf8(bytes).map(str::to_owned)
        .map_err(|_| Error::msg("Exec input slice is not valid UTF-8"))
}

fn run_at(
    mut caller: Caller<'_, VmState>,
    exec: Option<Rooted<ExternRef>>,
    dir: Option<Rooted<ExternRef>>,
    path_ptr: i32, path_len: i32,
    args_ptr: i32, args_len: i32,
    stdin_ptr: i32, stdin_len: i32,
) -> Result<Option<Rooted<ExternRef>>> {
    // Authenticate both authorities before touching guest memory. Copy every
    // input before the one effect, using the std Exec ABI's NUL-joined argv.
    let exec = exec_authority_ref(&caller, exec)?;
    let dir = dir_authority_ref(&caller, dir)?;
    dir_require_read(&dir)?;
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let path = text(data, path_ptr, path_len)?;
    let args = text(data, args_ptr, args_len)?;
    let stdin = text(data, stdin_ptr, stdin_len)?;
    let bytes = run_exec(&exec, &dir, &path, &args, &stdin)?;
    i32::try_from(bytes.len()).map_err(|_| Error::msg("Exec response exceeds guest ABI size limit"))?;
    ExternRef::new(&mut caller, ExecResponse(bytes.into())).map(Some)
}

fn response(caller: &Caller<'_, VmState>, value: Option<Rooted<ExternRef>>) -> Result<Arc<[u8]>> {
    let value = value.ok_or_else(|| Error::msg("Exec response is null"))?;
    value.data(caller)?.and_then(|data| data.downcast_ref::<ExecResponse>())
        .map(|response| Arc::clone(&response.0))
        .ok_or_else(|| Error::msg("Exec response has wrong host kind"))
}

fn response_size(caller: Caller<'_, VmState>, value: Option<Rooted<ExternRef>>) -> Result<i32> {
    Ok(response(&caller, value)?.len() as i32)
}

fn response_into(
    mut caller: Caller<'_, VmState>, value: Option<Rooted<ExternRef>>,
    out_ptr: i32, capacity: i32,
) -> Result<i32> {
    let bytes = response(&caller, value)?;
    let memory = memory_of(&mut caller)?;
    let output = range(out_ptr, capacity, memory.data_size(&caller))?;
    // Validate the entire declared capacity even when too short. No partial
    // write, no pending global, and no process invocation on either query.
    if output.len() >= bytes.len() {
        memory.write(&mut caller, output.start, &bytes)?;
    }
    Ok(bytes.len() as i32)
}

#[cfg(all(feature = "self-checks", unix))]
#[doc(hidden)]
#[path = "exec_tests.rs"]
pub mod tests;
