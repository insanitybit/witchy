use wasmtime::{Caller, Error, Linker, Result};
use witchy_syntax::intrinsics;

use super::super::{memory_of, read_wbytes, read_wstr, VmState};

/// Register the pending-buffer staging imports. `fill_pending` /
/// `write_pending_list` only write out data already staged by a granted size
/// call — no authority of their own, so the kernel links them unconditionally.
/// `args_size` stages the host-chosen argv (pure input, not authority), so it
/// is always available too.
pub(in crate::runtime) fn link_staging(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "fill_pending", host_fill_pending)?;
    linker.func_wrap("witchy", "write_pending_list", host_write_pending_list)?;
    linker.func_wrap("witchy", "args_size", host_args_size)?;
    Ok(())
}

/// Register the authority-free computed transfers: user-capability field
/// staging, regex match spans, float -> string formatting (done in the host so
/// it is byte-identical to the interpreter's `Display` — no float formatter in
/// WAT), code-point strings, and the hex/base64 transforms bridged to the same
/// native registry the interpreter uses (byte-for-byte parity, no byte-level
/// work in WAT).
pub(in crate::runtime) fn link_pure(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "user_cap_field_len", host_user_cap_field_len)?;
    linker.func_wrap("witchy", "regex_match_spans_len", host_regex_match_spans_len)?;
    linker.func_wrap("witchy", "float_to_str", host_float_to_str)?;
    linker.func_wrap("witchy", "string_from_code", host_string_from_code)?;
    linker.func_wrap("witchy", "encoding", host_encoding)?;
    Ok(())
}

/// Format an f64 with Rust's `Display` (matching the interpreter's float
/// `to_string`), write the bytes at `out_ptr`, and return the byte length. The
/// guest reserves a generous buffer; an f64's decimal form never exceeds it.
fn host_float_to_str(mut caller: Caller<'_, VmState>, x: f64, out_ptr: i32) -> Result<i32> {
    let s = witchy_syntax::fmt::render_float(x);
    let bytes = s.into_bytes();
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing float string into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

/// `string_from_code(codepoint, out_ptr) -> byte length`: encode a Unicode
/// scalar value as UTF-8 into the guest buffer, via the shared native registry
/// (the SAME `char::from_u32` the interpreter uses). An out-of-range or
/// surrogate value becomes U+FFFD, never an error.
fn host_string_from_code(mut caller: Caller<'_, VmState>, cp: i64, out_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let f = crate::native::lookup(intrinsics::STRING_FROM_CODE)
        .ok_or_else(|| Error::msg("string.from_code is not registered"))?;
    let s = match f(&[Value::Int(cp)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg("string.from_code did not return a String")),
    };
    let bytes = s.into_bytes();
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing string.from_code result into guest memory: {e}")))?;
    Ok(bytes.len() as i32)
}

/// `user_cap_field_len(param, field) -> Int` (RFC-0038): stage the (param, field)
/// policy string of a bare grantable-capability grant for `fill_pending`, and
/// report its byte length. Out of range is a launch/codegen mismatch — trap (the
/// compiled analog of the interpreter's under-grant error), so both backends
/// refuse a missing grant identically rather than diverging.
fn host_user_cap_field_len(mut caller: Caller<'_, VmState>, param: i32, field: i32) -> Result<i32> {
    let s = caller
        .data()
        .user_cap_fields
        .get(param as usize)
        .and_then(|fs| fs.get(field as usize))
        .cloned()
        .ok_or_else(|| Error::msg("a grantable-capability field is missing from the [user_caps] grant"))?;
    let len = s.len() as i32;
    caller.data_mut().pending = Some(s.into_bytes());
    Ok(len)
}

/// `regex_match_spans_len(pat_ptr, text_ptr) -> Int`: run the regex crate (the
/// same `regex.match_spans` native the interpreter uses) over two guest strings,
/// stage the encoded match spans for `fill_pending`, and report their byte
/// length. An invalid pattern traps — identical to the interpreter's error.
fn host_regex_match_spans_len(
    mut caller: Caller<'_, VmState>,
    pat_ptr: i32,
    text_ptr: i32,
) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let mem = memory_of(&mut caller)?;
    let pattern = read_wstr(mem.data(&caller), pat_ptr)?;
    let text = read_wstr(mem.data(&caller), text_ptr)?;
    let f = crate::native::lookup(intrinsics::REGEX_MATCH_SPANS)
        .ok_or_else(|| Error::msg(format!("{} is not registered", intrinsics::REGEX_MATCH_SPANS)))?;
    let spans = match f(&[Value::Str(pattern), Value::Str(text)]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s,
        _ => return Err(Error::msg(format!(
            "{} did not return a String",
            intrinsics::REGEX_MATCH_SPANS
        ))),
    };
    let len = spans.len() as i32;
    caller.data_mut().pending = Some(spans.into_bytes());
    Ok(len)
}

/// `encoding.*(op, in_header_ptr, out_data_ptr) -> byte length`: read a flat
/// String/Bytes buffer, run the selected hex/base64 transform through the shared
/// native registry (the same implementation the interpreter uses), write the
/// result flat buffer at `out_data_ptr`, and return its byte length. The guest
/// reserves a sufficient buffer (`2*len + slack`) beforehand.
fn host_encoding(mut caller: Caller<'_, VmState>, op: i32, in_ptr: i32, out_ptr: i32) -> Result<i32> {
    use crate::value::NativeValue as Value;
    let spec = intrinsics::lookup_wir_host_selector("encoding", op)
        .ok_or_else(|| Error::msg(format!("unknown encoding op {op}")))?;
    let input = spec.wir_host_call.expect("selector lookup returns a host call").input;
    let name = spec.name;
    let mem = memory_of(&mut caller)?;
    let arg = match input {
        intrinsics::WirHostInput::String => Value::Str(read_wstr(mem.data(&caller), in_ptr)?),
        intrinsics::WirHostInput::Bytes => Value::Bytes(read_wbytes(mem.data(&caller), in_ptr)?),
        intrinsics::WirHostInput::LossyUtf8Bytes => {
            Value::Str(String::from_utf8_lossy(&read_wbytes(mem.data(&caller), in_ptr)?).into_owned())
        }
    };
    let f = crate::native::lookup(name)
        .ok_or_else(|| Error::msg(format!("{name} is not registered")))?;
    let out = match f(&[arg]).map_err(|e| Error::msg(e.message))? {
        Value::Str(s) => s.into_bytes(),
        Value::Bytes(bytes) => bytes,
        _ => return Err(Error::msg(format!("{name} did not return a flat buffer"))),
    };
    mem.write(&mut caller, out_ptr as usize, &out)
        .map_err(|e| Error::msg(format!("writing {name} result into guest memory: {e}")))?;
    Ok(out.len() as i32)
}

/// `fill_pending(out_ptr)`: write the bytes staged by the matching size call.
fn host_fill_pending(mut caller: Caller<'_, VmState>, out_ptr: i32) -> Result<()> {
    let bytes = caller
        .data_mut()
        .pending
        .take()
        .ok_or_else(|| Error::msg("fill_pending called with nothing staged"))?;
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, out_ptr as usize, &bytes)
        .map_err(|e| Error::msg(format!("writing staged data into guest memory: {e}")))
}

/// `args_size() -> bytes`: stage the host-provided argv and report the byte
/// size of its `List(String)` structure (laid out by `write_pending_list`).
fn host_args_size(mut caller: Caller<'_, VmState>) -> Result<i32> {
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_host.is_some() {
        let args = match super::fixture::invoke(&mut caller, witchy_test_host::HostRequest::Argv)? {
            witchy_test_host::HostResponse::Strings(args) => args,
            response => {
                return Err(Error::msg(format!(
                    "fixture argv returned unexpected response {response:?}"
                )));
            }
        };
        let size = 4 + 8 * args.len() + args.iter().map(|arg| 4 + arg.len()).sum::<usize>();
        caller.data_mut().pending_list = Some(args);
        return Ok(size as i32);
    }
    let args = caller.data().caps.args.clone();
    let size = 4 + 8 * args.len() + args.iter().map(|a| 4 + a.len()).sum::<usize>();
    caller.data_mut().pending_list = Some(args);
    Ok(size as i32)
}

/// `write_pending_list(base_ptr)`: lay the staged string list out at `base_ptr`
/// in the guest's own list format — `[count][count x i64 slots][string
/// objects...]`, each slot holding the absolute guest pointer of its
/// `[len][bytes]` string. Authority-free: it only writes already-staged data.
fn host_write_pending_list(mut caller: Caller<'_, VmState>, base_ptr: i32) -> Result<()> {
    let names = caller
        .data_mut()
        .pending_list
        .take()
        .ok_or_else(|| Error::msg("write_pending_list called with nothing staged"))?;
    let n = names.len();
    let mut buf = Vec::with_capacity(4 + 8 * n + names.iter().map(|s| 4 + s.len()).sum::<usize>());
    buf.extend_from_slice(&(n as i32).to_le_bytes());
    let strings_start = base_ptr as i64 + 4 + 8 * n as i64;
    let mut offset = 0i64;
    for name in &names {
        let ptr = strings_start + offset;
        buf.extend_from_slice(&ptr.to_le_bytes());
        offset += 4 + name.len() as i64;
    }
    for name in &names {
        buf.extend_from_slice(&(name.len() as i32).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
    }
    let mem = memory_of(&mut caller)?;
    mem.write(&mut caller, base_ptr as usize, &buf)
        .map_err(|e| Error::msg(format!("writing directory listing into guest memory: {e}")))
}
