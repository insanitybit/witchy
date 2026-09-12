use wasmtime::{bail, Caller, Extern, Linker, Result};

use super::super::{memory_of, read_wstr, slice, VmState};

pub(super) fn require_console_token(
    store: impl wasmtime::AsContext,
    token: wasmtime::Rooted<wasmtime::ExternRef>,
) -> Result<()> {
    if !token.data(store.as_context())?.is_some_and(|data| data.is::<super::super::ConsoleCapability>()) {
        bail!("expected actual Console capability token");
    }
    Ok(())
}

pub(super) fn host_print_captured(
    caller: Caller<'_, VmState>,
    token: Option<wasmtime::Rooted<wasmtime::ExternRef>>,
    ptr: i32,
    len: i32,
) -> Result<()> {
    let token = token.ok_or_else(|| wasmtime::Error::msg("missing Console token"))?;
    require_console_token(&caller, token)?;
    if !caller.data().caps.print { bail!("Console output was not granted"); }
    host_print(caller, ptr, len)
}

/// Register the `__witchy_abort` diagnostic trap (RFC-0045). It is not a
/// capability — it grants no authority (it reads only the `(str_ptr)` string
/// the guest hands it, returns nothing, and its only effect is to terminate
/// execution with a diagnostic label, an ability the guest already has via
/// `unreachable`) — so the kernel links it unconditionally; codegen calls it
/// right before the trap so the compiled abort carries the interpreter's
/// exact message.
pub(in crate::runtime) fn link_abort(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "__witchy_abort", host_witchy_abort)?;
    linker.func_wrap("witchy", "__witchy_abort_at", host_witchy_abort_at)?;
    linker.func_wrap("witchy", "__witchy_list_bounds_at", host_witchy_list_bounds_at)?;
    Ok(())
}

/// Register the Console string print.
pub(in crate::runtime) fn link_fixture_console(linker: &mut Linker<VmState>) -> Result<()> {
    #[cfg(feature = "test-fixtures")]
    linker.func_wrap("witchy", "console_print_at", super::canonical_fixture::print_at)?;
    #[cfg(not(feature = "test-fixtures"))]
    linker.func_wrap("witchy", "console_print_at", |
        _caller: Caller<'_, VmState>,
        _token: Option<wasmtime::Rooted<wasmtime::ExternRef>>,
        _message_ptr: i32, _message_len: i32,
        _function_ptr: i32, _function_len: i32, _line: i32,
    | -> Result<()> {
        bail!("Console fixture support is unavailable in this runtime");
    })?;
    Ok(())
}

/// Register the Console string print.
pub(in crate::runtime) fn link_print(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "print", host_print)?;
    linker.func_wrap("witchy", "console_print", host_print_captured)?;
    Ok(())
}

#[cfg(test)]
mod canonical_console_tests {
    use super::*;
    use wasmtime::{ExternRef, Rooted};

    #[test]
    fn canonical_list_bounds_checks_unsigned_label_slice_and_signed_index() -> Result<()> {
        let mut runtime = super::super::super::Runtime::new()?;
        let engine = runtime.engine.clone();
        let empty = wasmtime::Module::new(&engine, "(module)")?;
        let vm = runtime.instantiate_module(crate::runtime::WorkerModule::Unadmitted(empty),
            super::super::super::Capabilities::none(), 1, None)?;
        let mut store = vm.store;
        let mut linker = Linker::new(&engine);
        link_abort(&mut linker)?;
        let module = wasmtime::Module::new(&engine, r#"(module
            (import "witchy" "__witchy_list_bounds_at" (func $bounds (param i64 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "main\ff")
            (func (export "call") (param i64 i32 i32 i32)
                local.get 0 local.get 1 local.get 2 local.get 3 i32.const 8 call $bounds))"#)?;
        let instance = linker.instantiate(&mut store, &module)?;
        let call = instance.get_typed_func::<(i64, i32, i32, i32), ()>(&mut store, "call")?;
        for (ptr, len) in [(65535, 2), (-1, 1), (0, -1)] {
            assert_eq!(call.call(&mut store, (0, 0, ptr, len)).unwrap_err().root_cause().to_string(), "abort slice out of bounds");
        }
        assert_eq!(call.call(&mut store, (0, 0, 4, 1)).unwrap_err().root_cause().to_string(), "abort slice is not UTF-8");
        for (index, length) in [(-1, 0), (4294967296, 1), (i64::MIN, -1)] {
            assert_eq!(call.call(&mut store, (index, length, 0, 4)).unwrap_err().root_cause().to_string(),
                format!("runtime error: `main`, line 8: list index {index} out of bounds (length {})", length as u32));
        }
        assert!(store.data().output.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn canonical_abort_checks_both_unsigned_slices_without_output() -> Result<()> {
        let mut runtime = super::super::super::Runtime::new()?;
        let engine = runtime.engine.clone();
        let empty = wasmtime::Module::new(&engine, "(module)")?;
        let vm = runtime.instantiate_module(crate::runtime::WorkerModule::Unadmitted(empty),
            super::super::super::Capabilities::none(), 1, None)?;
        let mut store = vm.store;
        let mut linker = Linker::new(&engine);
        link_abort(&mut linker)?;
        let module = wasmtime::Module::new(&engine, r#"(module
            (import "witchy" "__witchy_abort_at" (func $abort (param i32 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "boommain.explode\ff")
            (func (export "call") (param i32 i32 i32 i32)
                local.get 0 local.get 1 local.get 2 local.get 3 i32.const 4 call $abort))"#)?;
        let instance = linker.instantiate(&mut store, &module)?;
        let call = instance.get_typed_func::<(i32, i32, i32, i32), ()>(&mut store, "call")?;
        for arguments in [(65535, 2, 4, 12), (0, 4, 65535, 2), (-1, 1, 4, 12),
            (0, -1, 4, 12), (0, 4, -1, 1), (0, 4, 4, -1)] {
            let error = call.call(&mut store, arguments).expect_err("malformed abort slice");
            assert_eq!(error.root_cause().to_string(), "abort slice out of bounds");
        }
        for arguments in [(16, 1, 4, 12), (0, 4, 16, 1)] {
            let error = call.call(&mut store, arguments).expect_err("invalid UTF-8");
            assert_eq!(error.root_cause().to_string(), "abort slice is not UTF-8");
        }
        let error = call.call(&mut store, (0, 4, 4, 12)).expect_err("valid diagnostic is terminal");
        assert_eq!(error.root_cause().to_string(), "runtime error: `main.explode`, line 4: boom");
        assert!(store.data().output.lock().unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn canonical_console_rejects_null_forged_tokens_and_missing_grants() -> Result<()> {
        let mut runtime = super::super::super::Runtime::new()?;
        let engine = runtime.engine.clone();
        let empty = wasmtime::Module::new(&engine, "(module)")?;
        let vm = runtime.instantiate_module(crate::runtime::WorkerModule::Unadmitted(empty), super::super::super::Capabilities {
            print: true, quiet: true, ..super::super::super::Capabilities::none()
        }, 1, None)?;
        let mut store = vm.store;
        let mut linker = Linker::new(&engine);
        link_print(&mut linker)?;
        let module = wasmtime::Module::new(&engine, r#"(module
            (import "witchy" "console_print" (func $print (param externref i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "ok")
            (func (export "call") (param externref)
                local.get 0 i32.const 0 i32.const 2 call $print))"#)?;
        let instance = linker.instantiate(&mut store, &module)?;
        let call = instance.get_typed_func::<Option<Rooted<ExternRef>>, ()>(&mut store, "call")?;
        assert!(call.call(&mut store, None).is_err());
        let forged = ExternRef::new(&mut store, 0_i32)?;
        assert!(call.call(&mut store, Some(forged)).is_err());
        assert!(store.data().output.lock().unwrap().is_empty());
        let token = ExternRef::new(&mut store, super::super::super::ConsoleCapability)?;
        call.call(&mut store, Some(token))?;
        assert_eq!(*store.data().output.lock().unwrap(), ["ok"]);
        store.data_mut().caps.print = false;
        assert!(call.call(&mut store, Some(token)).is_err());
        assert_eq!(*store.data().output.lock().unwrap(), ["ok"]);
        Ok(())
    }
}

/// Register `Console[Read]` line input.
pub(in crate::runtime) fn link_console_read(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "console_read_len", host_console_read_len)?;
    Ok(())
}

/// Register the "print a computed result" facility for an Int- or
/// Float-returning `main`.
pub(in crate::runtime) fn link_print_values(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "print_int", host_print_int)?;
    linker.func_wrap("witchy", "print_float", host_print_float)?;
    Ok(())
}

/// (RFC-0045) `__witchy_abort(template, a, b, str_ptr)` — the always-linked,
/// authority-free abort channel. Renders the shared [`DiagTemplate`] (the SAME
/// wording the interpreter constructs) from the integer holes `a`/`b` and, when
/// the template has a string hole, the witchy string at `str_ptr`, then traps
/// with the interpreter's full location-prefixed `runtime error` text so the
/// compiled backend's surfaced message matches byte-for-byte. The compiler-owned
/// packed site global identifies the innermost aborting source statement. This
/// import never returns (codegen still emits `unreachable` after the call).
/// `str_ptr == 0` means "no string hole".
fn host_witchy_abort(
    mut caller: Caller<'_, VmState>,
    template: i32,
    a: i64,
    b: i64,
    str_ptr: i32,
) -> Result<()> {
    use witchy_syntax::diag::{runtime_error, DiagTemplate};
    let Some(tmpl) = DiagTemplate::from_id(template) else {
        bail!("runtime error: abort with unknown diagnostic template id {template}");
    };
    // Read the string hole only for a template that carries one, and only for a
    // non-null pointer (a defensive read: a crafted module could pass junk, but
    // `read_wstr` fails closed on an out-of-bounds slice).
    let site = guest_i64_global(&mut caller, "__witchy_diagnostic_site").unwrap_or(0);
    let (func_ptr, line) = witchy_syntax::diag::unpack_site(site);
    let (s, func) = if str_ptr != 0 || func_ptr != 0 {
        let mem = memory_of(&mut caller)?;
        let data = mem.data(&caller);
        (
            if str_ptr == 0 { String::new() } else { read_wstr(data, str_ptr).unwrap_or_default() },
            if func_ptr == 0 { String::new() } else { read_wstr(data, func_ptr as i32).unwrap_or_default() },
        )
    } else {
        (String::new(), String::new())
    };
    let core = tmpl.render(a, b, &s);
    bail!("{}", runtime_error(&func, line, &core));
}

fn guest_i64_global(caller: &mut Caller<'_, VmState>, name: &str) -> Option<i64> {
    let Some(Extern::Global(global)) = caller.get_export(name) else {
        return None;
    };
    global.get(&mut *caller).i64()
}

/// Canonical diagnostic ABI: raw byte slices, never implicit String headers or
/// a stale site global. All reads finish before rendering the terminal error.
fn diagnostic_text(data: &[u8], ptr: i32, len: i32) -> Result<&str> {
    let start = ptr as u32 as usize;
    let end = start.checked_add(len as u32 as usize)
        .ok_or_else(|| wasmtime::Error::msg("abort slice overflow"))?;
    let bytes = data.get(start..end).ok_or_else(|| wasmtime::Error::msg("abort slice out of bounds"))?;
    std::str::from_utf8(bytes).map_err(|_| wasmtime::Error::msg("abort slice is not UTF-8"))
}

fn host_witchy_list_bounds_at(
    mut caller: Caller<'_, VmState>, index: i64, length: i32,
    function_ptr: i32, function_len: i32, line: i32,
) -> Result<()> {
    let memory = memory_of(&mut caller)?;
    let function = diagnostic_text(memory.data(&caller), function_ptr, function_len)?;
    let core = witchy_syntax::diag::DiagTemplate::ListIndexOob.render(index, i64::from(length as u32), "");
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_console.is_some() {
        return Err(super::super::console_fixtures::diagnostic(function, line as u32, core));
    }
    bail!("{}", witchy_syntax::diag::runtime_error(function, line as u32, &core));
}

fn host_witchy_abort_at(
    mut caller: Caller<'_, VmState>, message_ptr: i32, message_len: i32,
    function_ptr: i32, function_len: i32, line: i32,
) -> Result<()> {
    let memory = memory_of(&mut caller)?;
    let data = memory.data(&caller);
    let message = diagnostic_text(data, message_ptr, message_len)?;
    let function = diagnostic_text(data, function_ptr, function_len)?;
    let core = witchy_syntax::diag::DiagTemplate::Fail.render(0, 0, message);
    #[cfg(feature = "test-fixtures")]
    if caller.data().fixture_console.is_some() {
        return Err(super::super::console_fixtures::diagnostic(function, line as u32, core));
    }
    bail!("{}", witchy_syntax::diag::runtime_error(function, line as u32, &core));
}

fn host_print(mut caller: Caller<'_, VmState>, ptr: i32, len: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let bytes = slice(data, ptr, len)?;
    let text = String::from_utf8_lossy(bytes);
    let id = caller.data().id;
    if caller.data().caps.live_output {
        use std::io::Write;
        // Long-running server: stream cleanly (no debug prefix) and flush, since
        // `main` never returns to drain the buffer and stdout is block-buffered
        // when redirected to a pipe/file (e.g. `coven-serve > log`).
        print!("{text}");
        let _ = std::io::stdout().flush();
    } else if !caller.data().caps.quiet {
        print!("[vm {id}] {text}");
    }
    caller
        .data()
        .output
        .lock()
        .unwrap()
        .push(text.trim_end_matches('\n').to_string());
    Ok(())
}

fn host_console_read_len(mut caller: Caller<'_, VmState>) -> Result<i32> {
    let mut line = match caller.data_mut().console_input.as_mut() {
        Some(lines) => lines.pop_front().unwrap_or_default(),
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|error| wasmtime::Error::msg(format!("failed to read Console input: {error}")))?;
            line
        }
    };
    while matches!(line.as_bytes().last(), Some(b'\r' | b'\n')) {
        line.pop();
    }
    let len = i32::try_from(line.len())
        .map_err(|_| wasmtime::Error::msg("Console input exceeds the guest ABI size limit"))?;
    caller.data_mut().pending = Some(line.into_bytes());
    Ok(len)
}

fn host_print_int(caller: Caller<'_, VmState>, n: i64) -> Result<()> {
    if caller.data().caps.live_output {
        use std::io::Write;
        println!("{n}");
        let _ = std::io::stdout().flush();
    } else if !caller.data().caps.quiet {
        println!("[vm {}] {n}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(n.to_string());
    Ok(())
}

fn host_print_float(caller: Caller<'_, VmState>, x: f64) -> Result<()> {
    // The same canonical float formatting the interpreter uses (Value::Float
    // Display via `render_float`), so the two backends agree on float output —
    // including the trailing `.0` on a whole-valued float.
    let s = witchy_syntax::fmt::render_float(x);
    if caller.data().caps.live_output {
        use std::io::Write;
        println!("{s}");
        let _ = std::io::stdout().flush();
    } else if !caller.data().caps.quiet {
        println!("[vm {}] {s}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(s);
    Ok(())
}
