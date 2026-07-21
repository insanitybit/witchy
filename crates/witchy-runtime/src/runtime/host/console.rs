use wasmtime::{bail, Caller, Extern, Linker, Result};

use super::super::{memory_of, read_wstr, slice, VmState};

/// Register the `__witchy_abort` diagnostic trap (RFC-0045). It is not a
/// capability — it grants no authority (it reads only the `(str_ptr)` string
/// the guest hands it, returns nothing, and its only effect is to terminate
/// execution with a diagnostic label, an ability the guest already has via
/// `unreachable`) — so the kernel links it unconditionally; codegen calls it
/// right before the trap so the compiled abort carries the interpreter's
/// exact message.
pub(in crate::runtime) fn link_abort(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "__witchy_abort", host_witchy_abort)?;
    Ok(())
}

/// Register the Console string print.
pub(in crate::runtime) fn link_print(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "print", host_print)?;
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

fn host_print(mut caller: Caller<'_, VmState>, ptr: i32, len: i32) -> Result<()> {
    let mem = memory_of(&mut caller)?;
    let data = mem.data(&caller);
    let bytes = slice(data, ptr, len)?;
    let text = String::from_utf8_lossy(bytes);
    let id = caller.data().id;
    if !caller.data().caps.quiet {
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

fn host_print_int(caller: Caller<'_, VmState>, n: i64) -> Result<()> {
    if !caller.data().caps.quiet {
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
    if !caller.data().caps.quiet {
        println!("[vm {}] {s}", caller.data().id);
    }
    caller.data().output.lock().unwrap().push(s);
    Ok(())
}
