//! Wasmtime adapters for compiler services exposed to trusted Witchy programs.

use super::{memory_of, read_wstr, VmState};
use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::intrinsics;

pub(super) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "compiler_footprint_len", host_compiler_footprint_len)?;
    linker.func_wrap("witchy", "compiler_diff_len", host_compiler_diff_len)?;
    linker.func_wrap("witchy", "compiler_doc_len", host_compiler_doc_len)?;
    linker.func_wrap(
        "witchy",
        "compiler_doc_result_json_len",
        host_compiler_doc_result_json_len,
    )?;
    Ok(())
}

/// `compiler_footprint_len(src_ptr) -> Int`: compute the capability-footprint
/// JSON of the guest's source string through the shared native registry, stage
/// it for `fill_pending`, and report its byte length.
fn host_compiler_footprint_len(mut caller: Caller<'_, VmState>, src_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup(intrinsics::COMPILER_FOOTPRINT)
        .ok_or_else(|| Error::msg("compiler.footprint is not registered"))?;
    let json = match f(&[Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.footprint did not return a String")),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_diff_len(old_ptr, new_ptr) -> Int`: compute the footprint-diff
/// JSON of two guest source strings, stage it for `fill_pending`, and report
/// its byte length.
fn host_compiler_diff_len(
    mut caller: Caller<'_, VmState>,
    old_ptr: i32,
    new_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let old_src = read_wstr(mem.data(&caller), old_ptr)?;
    let new_src = read_wstr(mem.data(&caller), new_ptr)?;
    let f = crate::native::lookup(intrinsics::COMPILER_DIFF)
        .ok_or_else(|| Error::msg("compiler.diff is not registered"))?;
    let json = match f(&[Value::Str(old_src), Value::Str(new_src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.diff did not return a String")),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}

/// `compiler_doc_len(name_ptr, src_ptr) -> Int`: render the guest's source string to
/// Markdown API docs (the `compiler.doc` native — `witchy doc` output) under heading
/// `name`, stage it for `fill_pending`, and report its byte length.
fn host_compiler_doc_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup(intrinsics::COMPILER_DOC)
        .ok_or_else(|| Error::msg("compiler.doc is not registered"))?;
    let md = match f(&[Value::Str(name), Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("compiler.doc did not return a String")),
    };
    let len = md.len() as i32;
    caller.data_mut().pending = Some(md.into_bytes());
    Ok(len)
}

/// `compiler_doc_result_json_len(name_ptr, src_ptr) -> Int`: render the
/// inspectable `compiler.try_doc` JSON result, stage it for `fill_pending`, and
/// report its byte length.
fn host_compiler_doc_result_json_len(
    mut caller: Caller<'_, VmState>,
    name_ptr: i32,
    src_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let f = crate::native::lookup(intrinsics::COMPILER_DOC_RESULT_JSON)
        .ok_or_else(|| Error::msg(format!("{} is not registered", intrinsics::COMPILER_DOC_RESULT_JSON)))?;
    let json = match f(&[Value::Str(name), Value::Str(src)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg(format!("{} did not return a String", intrinsics::COMPILER_DOC_RESULT_JSON))),
    };
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}
