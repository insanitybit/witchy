//! Wasmtime adapters for compiler services exposed to trusted Witchy programs.
//!
//! The adapters read guest strings, call the [`CompilerServices`]
//! implementation injected through [`VmState`], and stage the result for
//! `fill_pending`. The kernel depends on this interface — not on the
//! compiler's implementation modules — so post-compilation enforcement can
//! shed its parser/type/WIR implementation dependencies as the
//! implementation moves above the kernel.

use super::{memory_of, read_wstr, VmState};
use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::intrinsics;

/// The compiler services a trusted Witchy program may call.
pub(crate) trait CompilerServices: Send + Sync {
    /// The capability-footprint JSON of one source string.
    fn footprint(&self, src: &str) -> Result<String>;
    /// The footprint-diff JSON of two source strings.
    fn footprint_diff(&self, old_src: &str, new_src: &str) -> Result<String>;
    /// Markdown API docs for `src` under heading `name` (`witchy doc` output).
    fn doc(&self, name: &str, src: &str) -> Result<String>;
    /// The inspectable `compiler.try_doc` JSON result.
    fn doc_result_json(&self, name: &str, src: &str) -> Result<String>;
}

/// The default implementation: the same `compiler.*` natives the interpreter
/// exposes, resolved through the shared native registry at call time. The
/// intrinsic names double as the diagnostic labels, so the pre-injection
/// error messages are preserved byte for byte.
pub(crate) struct RegistryCompilerServices;

impl RegistryCompilerServices {
    fn call(name: &'static str, args: &[crate::value::NativeValue]) -> Result<String> {
        let f = crate::native::lookup(name)
            .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
        match f(args).map_err(|e| Error::msg(e.message))? {
            crate::value::NativeValue::Str(s) => Ok(s),
            _ => Err(Error::msg(format!("{name} did not return a String"))),
        }
    }
}

impl CompilerServices for RegistryCompilerServices {
    fn footprint(&self, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(intrinsics::COMPILER_FOOTPRINT, &[Value::Str(src.to_string())])
    }

    fn footprint_diff(&self, old_src: &str, new_src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DIFF,
            &[Value::Str(old_src.to_string()), Value::Str(new_src.to_string())],
        )
    }

    fn doc(&self, name: &str, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DOC,
            &[Value::Str(name.to_string()), Value::Str(src.to_string())],
        )
    }

    fn doc_result_json(&self, name: &str, src: &str) -> Result<String> {
        use crate::value::NativeValue as Value;
        Self::call(
            intrinsics::COMPILER_DOC_RESULT_JSON,
            &[Value::Str(name.to_string()), Value::Str(src.to_string())],
        )
    }
}

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
/// JSON of the guest's source string through the injected compiler services,
/// stage it for `fill_pending`, and report its byte length.
fn host_compiler_footprint_len(mut caller: Caller<'_, VmState>, src_ptr: i32) -> Result<i32> {
    let mem = memory_of(&mut caller)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let json = caller.data().compiler_services.clone().footprint(&src)?;
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
    let mem = memory_of(&mut caller)?;
    let old_src = read_wstr(mem.data(&caller), old_ptr)?;
    let new_src = read_wstr(mem.data(&caller), new_ptr)?;
    let json = caller.data().compiler_services.clone().footprint_diff(&old_src, &new_src)?;
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
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let md = caller.data().compiler_services.clone().doc(&name, &src)?;
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
    let mem = memory_of(&mut caller)?;
    let name = read_wstr(mem.data(&caller), name_ptr)?;
    let src = read_wstr(mem.data(&caller), src_ptr)?;
    let json = caller.data().compiler_services.clone().doc_result_json(&name, &src)?;
    let len = json.len() as i32;
    caller.data_mut().pending = Some(json.into_bytes());
    Ok(len)
}
