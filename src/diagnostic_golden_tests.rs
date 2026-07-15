//! RFC-0072: verbatim diagnostic goldens over witchy's full error surface.
//!
//! Every error assertion elsewhere in the suite is a loose `.contains(...)`; a
//! message can silently degrade — drop its hint, lose its position — and stay
//! green. This file locks the ACTUAL text of each diagnostic class (parse,
//! layout, link, type, capability, `mode opt` enforcement, lowering-reject, and
//! runtime trap) with `insta` snapshots, the way rustc's `ui`/`.stderr` goldens
//! do. A message change is now a visible, reviewed diff in `src/snapshots/`.
//!
//! Scope is RFC-0072 **Phase 1** only: capture the surface as it is *today*,
//! truthfully, warts included. Where a captured message is genuinely wrong, it
//! is flagged `// KNOWN-BAD (BUG-NNN)` and the golden locks the current
//! (imperfect) behavior so its eventual fix shows up as a deliberate golden
//! update — Phase 2 (message polish) is out of scope here.
//!
//! Parity note: every runtime-trap golden captures BOTH the interpreter and the
//! compiled-WASM output in one snapshot, so a diverging pair is a parity failure
//! caught at the message level. Routed runtime traps must carry the same source
//! position and message on both backends (RFC-0045 strict); a bare Wasm trap is
//! a regression.

use crate::{codegen, interpreter, parser, pipeline, typeck};

// ---------------------------------------------------------------------------
// Pipeline plumbing — reuses the same stages the CLI and `example_tests.rs`
// drive (`parse_module` -> `pipeline::link` -> `typeck::check` ->
// `compile_module_binary` / `interpreter::run_module` / `run_wasm_bytes`).
// Each helper returns the diagnostic STRING for the first stage that rejects.
// Snapshots must be deterministic: sources carry no absolute paths, and no
// helper surfaces a timestamp, address, or filesystem path.
// ---------------------------------------------------------------------------

/// A parse error (or `<unexpectedly parsed>` if the source was accepted).
fn parse_diag(src: &str) -> String {
    match parser::parse_module(src) {
        Ok(_) => "<unexpectedly parsed>".to_string(),
        Err(e) => e.to_string(),
    }
}

/// A link error, driving parse then `pipeline::link` (which pulls in any
/// imported std module, exactly like the CLI's `link_file`).
fn link_diag(src: &str) -> String {
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => return format!("<parse error>: {e}"),
    };
    match pipeline::link(vec![("main".into(), module)], "main") {
        Ok(_) => "<unexpectedly linked>".to_string(),
        Err(e) => e.to_string(),
    }
}

/// A link error over several named modules (the multi-file case).
fn multi_link_diag(sources: &[(&str, &str)], entry: &str) -> String {
    let mut mods = Vec::new();
    for (n, s) in sources {
        match parser::parse_module(s) {
            Ok(m) => mods.push(((*n).to_string(), m)),
            Err(e) => return format!("<parse error in {n}>: {e}"),
        }
    }
    match pipeline::link(mods, entry) {
        Ok(_) => "<unexpectedly linked>".to_string(),
        Err(e) => e.to_string(),
    }
}

/// A type error, driving parse -> link -> `typeck::check`.
fn type_diag(src: &str) -> String {
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => return format!("<parse error>: {e}"),
    };
    let linked = match pipeline::link(vec![("main".into(), module)], "main") {
        Ok(m) => m,
        Err(e) => return format!("<link error>: {e}"),
    };
    match typeck::check(&linked) {
        Ok(()) => "<unexpectedly type-checked>".to_string(),
        Err(e) => e.to_string(),
    }
}

/// A `mode opt` enforcement error (the ownership-convention surface), driving
/// parse -> link -> `enforce_performance_modes` — the CLI's perf-mode gate.
fn mode_diag(src: &str) -> String {
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => return format!("<parse error>: {e}"),
    };
    let linked = match pipeline::link(vec![("main".into(), module)], "main") {
        Ok(m) => m,
        Err(e) => return format!("<link error>: {e}"),
    };
    match crate::enforce_performance_modes(&linked, "main") {
        Ok(()) => "<unexpectedly accepted>".to_string(),
        Err(e) => e,
    }
}

/// A lowering (codegen) rejection, driving the whole front end then
/// `compile_module_binary`; `Ok(None)` is the "does not lower" reject channel,
/// `Err` a hard `codegen error:` diagnostic.
fn lower_diag(src: &str) -> String {
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => return format!("<parse error>: {e}"),
    };
    let linked = match pipeline::link(vec![("main".into(), module)], "main") {
        Ok(m) => m,
        Err(e) => return format!("<link error>: {e}"),
    };
    if let Err(e) = typeck::check(&linked) {
        return format!("<type error>: {e}");
    }
    match codegen::compile_module_binary(&linked) {
        Ok(Some(_)) => "<unexpectedly compiled>".to_string(),
        Ok(None) => "<reject: does not lower to the compiled backend>".to_string(),
        Err(e) => e.to_string(),
    }
}

/// Run a program to a runtime trap on the interpreter, returning the trap
/// string. Panics on any earlier-stage failure (the source must be valid up to
/// the trap — these are runtime goldens).
fn interp_trap(src: &str) -> String {
    let module = parser::parse_module(src).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typecheck");
    match interpreter::run_module(linked, ".", Vec::new()) {
        Ok(out) => format!("<ran without trapping -> {out:?}>"),
        Err(e) => e.to_string(),
    }
}

/// Run the same program to a runtime trap on the compiled-WASM backend.
fn wasm_trap(src: &str) -> String {
    let module = parser::parse_module(src).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typecheck");
    let bytes = codegen::compile_module_binary(&linked)
        .expect("compile")
        .expect("the binary path lowers this program");
    match crate::run_wasm_bytes(&bytes) {
        Ok(out) => format!("<ran without trapping -> {out:?}>"),
        Err(e) => e,
    }
}

/// Snapshot the interpreter+compiled trap PAIR for one program (RFC-0072's
/// parity rule): both backends' trap text in one golden, so a divergence is a
/// message-level parity failure.
fn trap_pair(src: &str) -> String {
    let interp = interp_trap(src);
    let wasm = wasm_trap(src);
    assert_eq!(wasm, interp, "runtime diagnostic parity");
    format!("interp: {interp}\nwasm:   {wasm}")
}

// ===========================================================================
// Parse & layout diagnostics
// ===========================================================================
mod parse {
    use super::*;

    #[test]
    fn unclosed_paren() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n    console.print((1 + 2)\n"
        ));
    }

    #[test]
    fn unexpected_token() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n    let x = = 3\n"
        ));
    }

    #[test]
    fn retired_pipe_operator() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n    console.print(\"hi\" |> string.to_upper())\n"
        ));
    }

    #[test]
    fn braces_are_not_syntax() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console) {\n    console.print(\"hi\")\n}\n"
        ));
    }

    #[test]
    fn missing_header_colon() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console)\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn unterminated_string() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n    console.print(\"unterminated)\n"
        ));
    }

    #[test]
    fn interpolation_hole_parse_error() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n    console.print(\"pre ${value + } post\")\n"
        ));
    }

    #[test]
    fn unknown_performance_mode() {
        insta::assert_snapshot!(parse_diag(
            "mode turbo\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn top_level_item_expected() {
        insta::assert_snapshot!(parse_diag(
            "fn f() -> Int:\n    1\nimport list\n"
        ));
    }

    // --- layout / off-side rule ---

    #[test]
    fn tab_in_leading_indentation() {
        insta::assert_snapshot!(parse_diag(
            "fn main(console: Console):\n\tconsole.print(\"hi\")\n"
        ));
    }
}

// ===========================================================================
// Link diagnostics (unknown module / missing function / unknown type /
// module-qualified-reference-without-import, incl. did-you-mean hints)
// ===========================================================================
mod link {
    use super::*;

    #[test]
    fn unknown_imported_module() {
        insta::assert_snapshot!(link_diag(
            "import wibble\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn module_has_no_such_function() {
        insta::assert_snapshot!(link_diag(
            "import list\n\nfn main(console: Console):\n    console.print(\"${list.nonexistent([1])}\")\n"
        ));
    }

    #[test]
    fn unknown_type_qualify_hint() {
        insta::assert_snapshot!(link_diag(
            "type Wrapper:\n    inner: Wibble\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn module_qualified_reference_not_imported() {
        insta::assert_snapshot!(link_diag(
            "fn main(console: Console):\n    let f = iter.count\n    console.print(\"x\")\n"
        ));
    }

    #[test]
    fn multi_module_unknown_import() {
        insta::assert_snapshot!(multi_link_diag(
            &[(
                "main",
                "import helper\n\nfn main(console: Console):\n    console.print(\"${helper.missing()}\")\n",
            )],
            "main",
        ));
    }
}

// ===========================================================================
// Type diagnostics
// ===========================================================================
mod typecheck {
    use super::*;

    #[test]
    fn annotation_value_mismatch() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    let x: Int = \"hello\"\n    console.print(\"${x}\")\n"
        ));
    }

    #[test]
    fn arity_too_many_arguments() {
        insta::assert_snapshot!(type_diag(
            "fn add(a: Int, b: Int) -> Int:\n    a + b\n\nfn main(console: Console):\n    console.print(\"${add(1, 2, 3)}\")\n"
        ));
    }

    #[test]
    fn arity_too_few_arguments() {
        insta::assert_snapshot!(type_diag(
            "fn add(a: Int, b: Int) -> Int:\n    a + b\n\nfn main(console: Console):\n    console.print(\"${add(1)}\")\n"
        ));
    }

    #[test]
    fn inherent_method_arity_uses_surface_name() {
        insta::assert_snapshot!(type_diag(
            "type Point:\n    x: Int\n\nimpl Point:\n    fn scaled(self, factor: Int) -> Int:\n        self.x * factor\n\nfn main(console: Console):\n    let p = Point(3)\n    console.print(\"${p.scaled(2, 3)}\")\n"
        ));
    }

    #[test]
    fn static_method_arity_uses_surface_name() {
        insta::assert_snapshot!(type_diag(
            "type Score:\n    value: Int\n\nimpl Score:\n    fn zero() -> Score:\n        Score(0)\n\nfn main(console: Console):\n    console.print(\"${Score.zero(1)}\")\n"
        ));
    }

    #[test]
    fn trait_method_arity_uses_surface_name() {
        insta::assert_snapshot!(type_diag(
            "trait Greet:\n    fn hi(self) -> String\n\ntype Person:\n    name: String\n\nimpl Greet for Person:\n    fn hi(self) -> String:\n        self.name\n\nfn main(console: Console):\n    let p = Person(\"Ada\")\n    console.print(p.hi(1))\n"
        ));
    }

    #[test]
    fn generic_trait_dispatch_arity_uses_surface_name() {
        insta::assert_snapshot!(type_diag(
            "trait Lessable:\n    fn less(self, other: Self) -> Bool\n\nimpl Lessable for Int:\n    fn less(self, other: Int) -> Bool:\n        self < other\n\nfn smallest(a: a, b: a) -> a where a: Lessable:\n    if less(a, b, 1):\n        a\n    else:\n        b\n\nfn main(console: Console):\n    console.print(\"${smallest(1, 2)}\")\n"
        ));
    }

    #[test]
    fn unknown_function() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    console.print(\"${mystery(1)}\")\n"
        ));
    }

    #[test]
    fn unknown_function_did_you_mean() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${lenght(xs)}\")\n"
        ));
    }

    // (RFC-0072 phase 2) The two jargon-class messages the audit flagged: each
    // now names a fix instead of leaking a checker internal (d7a37976). Locked
    // so the guidance can't silently regress back into jargon.

    #[test]
    fn type_parameter_pinned_is_not_generic() {
        insta::assert_snapshot!(type_diag(
            "fn f(x: a) -> Int:\n    x + 1\n\nfn main(console: Console):\n    print(console, \"${f(1)}\")\n"
        ));
    }

    #[test]
    fn field_access_on_unresolved_type() {
        insta::assert_snapshot!(type_diag(
            "fn get(p: a) -> Int:\n    p.x\n\nfn main(console: Console):\n    print(console, \"${get(.{x: 7})}\")\n"
        ));
    }

    #[test]
    // RFC-0072 phase 2: even module-qualified-call import hints discovered during
    // method lowering carry the same enclosing function/line prefix as ordinary
    // type errors.
    fn module_qualified_call_not_imported() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    console.print(json.stringify(42))\n"
        ));
    }

    #[test]
    fn retired_global_builtin_now_module_qualified() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    console.print(\"${push([1], 2)}\")\n"
        ));
    }

    #[test]
    fn wrong_argument_type() {
        insta::assert_snapshot!(type_diag(
            "fn add(a: Int, b: Int) -> Int:\n    a + b\n\nfn main(console: Console):\n    console.print(\"${add(1, \"two\")}\")\n"
        ));
    }

    #[test]
    fn function_body_return_mismatch() {
        insta::assert_snapshot!(type_diag(
            "fn f() -> Int:\n    \"nope\"\n\nfn main(console: Console):\n    console.print(\"${f()}\")\n"
        ));
    }

    #[test]
    fn if_branches_disagree() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    let x = if true: 1 else: \"two\"\n    console.print(\"${x}\")\n"
        ));
    }

    #[test]
    fn if_condition_not_bool() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    if 3: console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn unbound_variable() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    console.print(\"${undefined_var}\")\n"
        ));
    }

    #[test]
    fn unbound_capability_without_parameter() {
        insta::assert_snapshot!(type_diag(
            "fn main():\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn record_has_no_such_field() {
        insta::assert_snapshot!(type_diag(
            "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let p = Point(x: 1, y: 2)\n    console.print(\"${p.z}\")\n"
        ));
    }

    #[test]
    fn duplicate_parameter_names() {
        insta::assert_snapshot!(type_diag(
            "fn f(a: Int, a: Int) -> Int:\n    a\n\nfn main(console: Console):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn trait_method_has_no_function_value() {
        insta::assert_snapshot!(type_diag(
            "import show\n\nfn main(console: Console):\n    let f = show\n    console.print(\"x\")\n"
        ));
    }

    #[test]
    fn use_after_move_own_parameter() {
        insta::assert_snapshot!(type_diag(
            "import list\n\nfn take(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    let xs = [1, 2, 3]\n    let a = take(xs)\n    let b = take(xs)\n    console.print(\"${a + b}\")\n"
        ));
    }

    #[test]
    fn interpolate_a_function_value() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f}\")\n"
        ));
    }

    #[test]
    fn float_does_not_implement_ord() {
        insta::assert_snapshot!(type_diag(
            "import list\n\nfn main(console: Console):\n    let xs = [3.0, 1.0]\n    console.print(\"${list.length(list.sort(xs))}\")\n"
        ));
    }

    #[test]
    fn ordering_requires_ord() {
        insta::assert_snapshot!(type_diag(
            "type Widget:\n    n: Int\n\nfn main(console: Console):\n    let a = Widget(n: 1)\n    let b = Widget(n: 2)\n    if a < b: console.print(\"lt\") else: console.print(\"ge\")\n"
        ));
    }

    // A plain (non-method) function invoked through method syntax. The receiver's
    // real type must be named — a record used to leak the `Bool(false)` rewrite
    // placeholder ("no method `describe` on `Bool`") — and the message names the
    // direct-call fix, the same on a record and on a built-in receiver.
    #[test]
    fn plain_function_as_method_on_record_names_the_type() {
        insta::assert_snapshot!(type_diag(
            "type Point:\n    x: Int\n    y: Int\n\nfn describe(p: Point) -> Int:\n    p.x\n\nfn main(console: Console):\n    let p = Point(1, 2)\n    console.print(\"${p.describe()}\")\n"
        ));
    }

    #[test]
    fn plain_function_as_method_on_builtin_receiver() {
        insta::assert_snapshot!(type_diag(
            "fn describe(s: String) -> Int:\n    string.length(s)\n\nfn main(console: Console):\n    let s = \"hi\"\n    console.print(\"${s.describe()}\")\n"
        ));
    }

    #[test]
    fn imported_module_function_as_method_on_builtin() {
        insta::assert_snapshot!(type_diag(
            "import func\n\nfn main(console: Console):\n    let s = \"hi\"\n    console.print(s.identity())\n"
        ));
    }

    // A same-named function that could NOT accept this receiver (`map` exists for
    // List/Option but the receiver is a user `Point`) must NOT get the confident
    // "`map` is a plain function, call it directly" message — it falls back to the
    // generic "no method" wording, since `map(point)` would not resolve either.
    #[test]
    fn unrelated_same_name_collision_uses_generic_message() {
        insta::assert_snapshot!(type_diag(
            "import list\n\ntype Point:\n    x: Int\n\nfn main(console: Console):\n    let p = Point(1)\n    console.print(\"${p.map(fn(v): v)}\")\n"
        ));
    }
}

// ===========================================================================
// Capability diagnostics
// ===========================================================================
mod capability {
    use super::*;

    #[test]
    fn main_takes_only_grantable_root_capabilities() {
        insta::assert_snapshot!(type_diag(
            "type Config:\n    Config(Int)\n\nfn main(console: Console, c: Config):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn main_rejects_a_build_capability() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console, out: BuildOut):\n    console.print(\"hi\")\n"
        ));
    }

    #[test]
    fn cap_gated_export_leads_with_grantable() {
        insta::assert_snapshot!(type_diag(
            "type Config:\n    Config(Int)\n\npub fn export_step(c: Config, input: String) -> String:\n    input\n"
        ));
    }

    #[test]
    fn dir_read_capability_cannot_write() {
        insta::assert_snapshot!(type_diag(
            "fn main(dir: Dir[Read]):\n    dir.write(\"x\", \"y\")\n"
        ));
    }

    #[test]
    fn bare_capability_operation_reports_method_only_fixit() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    print(console, \"hi\")\n"
        ));
    }

    #[test]
    fn console_print_requires_string() {
        insta::assert_snapshot!(type_diag(
            "fn main(console: Console):\n    console.print(42)\n"
        ));
    }

    #[test]
    fn file_read_capability_cannot_write() {
        insta::assert_snapshot!(type_diag(
            "fn main(file: File[Read]):\n    file.write(\"x\")\n"
        ));
    }

    #[test]
    fn net_connect_capability_cannot_listen() {
        insta::assert_snapshot!(type_diag(
            "fn main(net: Net[Connect]):\n    net.listen(\"127.0.0.1:0\")\n"
        ));
    }
}

// ===========================================================================
// `mode opt` enforcement (ownership-convention) diagnostics
// ===========================================================================
mod perf_mode {
    use super::*;

    #[test]
    fn ownership_convention_required_in_mode_opt() {
        insta::assert_snapshot!(mode_diag(
            "mode opt\n\nimport list\n\nfn tag(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    console.print(\"${tag([1, 2, 3])}\")\n"
        ));
    }

    #[test]
    fn no_copy_var_contract_explains_alias() {
        insta::assert_snapshot!(mode_diag(
            "mode opt\n\nimport dict\n\nfn main(console: Console):\n    var d = dict.new()\n    let _ = d.insert(\"a\", 1)\n    let snapshot = d\n    d.insert(\"a\", 2)\n    console.print(\"${snapshot.length()}\")\n"
        ));
    }

    #[test]
    fn no_copy_collection_family_covers_pop_and_remove() {
        insta::assert_snapshot!(mode_diag(
            "mode opt\n\nimport dict\nimport list\n\nfn main(console: Console):\n    var xs = [1, 2]\n    let xs_snapshot = xs\n    let _ = xs.pop()\n    var d = dict.new()\n    let _ = d.insert(\"a\", 1)\n    let d_snapshot = d\n    let _ = d.remove(\"a\")\n    console.print(\"${xs_snapshot.length() + d_snapshot.length()}\")\n"
        ));
    }

    #[test]
    fn no_copy_indirect_abi_limitation_is_explicit() {
        insta::assert_snapshot!(mode_diag(
            "mode opt\n\nfn take(var xs: unique List(Int)) -> Nil:\n    return\n\nfn main():\n    var xs = [1]\n    let f = take\n    f(xs)\n"
        ));
    }
}

// ===========================================================================
// Lowering-reject (codegen) diagnostics
// ===========================================================================
mod lowering {
    use super::*;

    #[test]
    fn dict_record_key_not_lowerable() {
        insta::assert_snapshot!(lower_diag(
            "import dict\n\ntype K:\n    a: Int\n    b: Int\n\nfn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, K(a: 1, b: 2), \"v\")\n    console.print(\"hi\")\n"
        ));
    }
}

// ===========================================================================
// Comptime-generated code diagnostics
// ===========================================================================
mod comptime {
    use super::*;

    #[test]
    // Historically BUG-341: a type error in comptime-emitted code reported a
    // phantom line past EOF. FIXED (commit 7375442) — the error now attributes to
    // the emitted function's own line 1. This golden locks the fixed behavior so a
    // regression to a past-EOF phantom line is caught.
    fn type_error_in_emitted_code() {
        insta::assert_snapshot!(type_diag(
            "comptime:\n    emit(\"fn broken() -> Int:\")\n    emit(\"    \\\"nope\\\"\")\n\nfn main(console: Console):\n    console.print(\"${broken()}\")\n"
        ));
    }
}

// ===========================================================================
// Runtime traps — interpreter + compiled-WASM PAIR (parity locked)
// ===========================================================================
mod runtime {
    use super::*;

    #[test]
    fn list_index_out_of_bounds() {
        insta::assert_snapshot!(trap_pair(
            "import list\n\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 9)}\")\n"
        ));
    }

    #[test]
    fn index_operator_out_of_bounds() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${xs[9]}\")\n"
        ));
    }

    #[test]
    fn parse_int_of_junk() {
        insta::assert_snapshot!(trap_pair(
            "import string\n\nfn main(console: Console):\n    console.print(\"${string.to_int(\"notanumber\")}\")\n"
        ));
    }

    #[test]
    fn user_fail() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    fail(\"something broke\")\n"
        ));
    }

    #[test]
    fn integer_division_by_zero() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    let z = 0\n    console.print(\"${10 / z}\")\n"
        ));
    }

    #[test]
    fn nested_function_uses_innermost_source_site() {
        insta::assert_snapshot!(trap_pair(
            "fn explode() -> Int:\n    let z = 0\n    10 / z\n\nfn main(console: Console):\n    console.print(\"${explode()}\")\n"
        ));
    }

    #[test]
    fn successful_nested_call_restores_caller_source_site() {
        insta::assert_snapshot!(trap_pair(
            "import list\n\nfn probe() -> Int:\n    let inner = [7]\n    let _ = list.at(inner, 0)\n    9\n\nfn main(console: Console):\n    let outer = [1]\n    console.print(\"${list.at(outer, probe())}\")\n"
        ));
    }

    #[test]
    fn escaping_lambda_uses_lexical_source_owner() {
        insta::assert_snapshot!(trap_pair(
            "fn make() -> fn() -> Int:\n    fn(): 10 / 0\n\nfn main(console: Console):\n    let explode = make()\n    console.print(\"${explode()}\")\n"
        ));
    }

    #[test]
    fn successful_closure_call_restores_caller_source_site() {
        insta::assert_snapshot!(trap_pair(
            "import list\n\nfn make_probe() -> fn() -> Int:\n    fn(): list.at([7], 0)\n\nfn main(console: Console):\n    let outer = [1]\n    let probe = make_probe()\n    console.print(\"${list.at(outer, probe())}\")\n"
        ));
    }

    #[test]
    fn integer_division_overflow() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    let min = (0 - 9223372036854775807) - 1\n    console.print(\"${min / (0 - 1)}\")\n"
        ));
    }

    #[test]
    fn integer_modulo_by_zero() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    let z = 0\n    console.print(\"${10 % z}\")\n"
        ));
    }

    #[test]
    fn nan_to_int() {
        insta::assert_snapshot!(trap_pair(
            "import math\n\nfn main(console: Console):\n    console.print(\"${math.to_int(0.0 / 0.0)}\")\n"
        ));
    }

    #[test]
    fn nan_comparison_order() {
        insta::assert_snapshot!(trap_pair(
            "fn main(console: Console):\n    let a = 0.0 / 0.0\n    let b = 1.0\n    if a < b: console.print(\"lt\") else: console.print(\"ge\")\n"
        ));
    }

    #[test]
    fn required_secret_not_granted() {
        // A `SecretStore` is granted, but the named secret `signing` is not — the
        // eager require-site trap (BUG-394). Interpreter-only capture: the trap is
        // driven through `run_module_exit_secrets` (the compiled twin needs a
        // `SecretGrant` wiring the differential harness covers elsewhere).
        let src = "import secretstore\n\nfn main(console: Console, secrets: SecretStore):\n    let s = secretstore.require(secrets, \"signing\")\n    console.print(\"got it\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let diag = match interpreter::run_module_exit_secrets(
            linked,
            ".",
            Vec::new(),
            Vec::new(),
            None,
            Vec::new(),
        ) {
            Ok(out) => format!("<ran without trapping -> {out:?}>"),
            Err(e) => e.to_string(),
        };
        insta::assert_snapshot!(diag);
    }
}
