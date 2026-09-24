//! RFC-0080 compiler-owned statement and block payloads.

use witchy::{comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

type Number = Int

comptime fn local_stmt() -> meta.StmtSyntax:
    quote stmt:
        let x = 40

comptime fn direct_body() -> meta.BlockSyntax:
    quote block:
        let x = 40
        x + 2

fn selected() -> Number:
    42

fn touched() -> ():
    ()

comptime fn call_site_body() -> meta.BlockSyntax:
    let selected = meta.expr_name(meta.call_site("selected"))
    let call = meta.expr_call(selected, [])
    meta.block([], Some(call))

comptime fn statement_body() -> meta.BlockSyntax:
    let touched = meta.expr_name(meta.call_site("touched"))
    let touch = meta.stmt_expr(meta.expr_call(touched, []))
    let binding = meta.fresh("selected_value")
    let number = meta.type_named(meta.call_site("Number"), [])
    let selected = meta.expr_name(meta.call_site("selected"))
    let bind = meta.stmt_let(false, binding, Some(number), meta.expr_call(selected, []))
    let result = meta.stmt_return(meta.expr_name(binding))
    meta.block([touch, bind, result], Some(meta.expr_int(0)))

comptime:
    let int = quote type:
        Int
    let composed = meta.block([local_stmt()], Some(meta.expr_int(7)))
    emit_item(meta.function_block(true, meta.ident("composed"), [], Some(int), composed))
    emit_item(meta.function_block(true, meta.ident("direct"), [], Some(int), direct_body()))
    emit_item(meta.function_block(true, meta.ident("generated_selected"), [], Some(int), call_site_body()))
    emit_item(meta.function_block(true, meta.ident("statement_generated"), [], Some(int), statement_body()))
    let input = meta.ident("input")
    let number = meta.type_named(meta.call_site("Number"), [])
    let numbers = meta.type_named(meta.ident("List"), [number])
    let composite = meta.type_tuple([numbers, number])
    let identity_body = meta.block([], Some(meta.expr_name(input)))
    let identity_param = meta.param(input, composite)
    emit_item(meta.function_block(true, meta.ident("generated_identity"), [identity_param], Some(composite), identity_body))
    let callback = meta.ident("callback")
    let callback_type = meta.type_fn([number], number)
    let callback_param = meta.param(callback, callback_type)
    let callback_body = meta.block([], Some(meta.expr_call(meta.expr_name(callback), [meta.expr_int(42)])))
    emit_item(meta.function_block(true, meta.ident("generated_apply"), [callback_param], Some(number), callback_body))
    let frozen_input = meta.ident("frozen_input")
    let frozen_number = meta.type_frozen(number)
    let frozen_param = meta.param(frozen_input, frozen_number)
    let frozen_body = meta.block([], Some(meta.expr_name(frozen_input)))
    emit_item(meta.function_block(true, meta.ident("generated_frozen_identity"), [frozen_param], Some(number), frozen_body))
    let reflected_number = meta.TNamed("Number", [])
    let reflected_list = meta.TNamed("List", [reflected_number])
    let reflected_callback = meta.TFn([reflected_number], reflected_number, ["let"], false, false)
    let reflected_frozen = meta.TQualified("frozen", reflected_number)
    let reflected_input = meta.type_expr(meta.TTuple([reflected_list, reflected_callback, reflected_frozen]))
    let reflected_param = meta.param(meta.ident("reflected"), reflected_input)
    let reflected_body = meta.block([], Some(meta.expr_int(42)))
    emit_item(meta.function_block(true, meta.ident("generated_reflected"), [reflected_param], Some(number), reflected_body))
    let read = meta.type_named(meta.ident("Read"), [])
    let directory = meta.type_capability(meta.ident("Dir"), [read])
    let directory_param = meta.param(meta.ident("directory"), directory)
    emit_item(meta.function_block(true, meta.ident("generated_capability"), [directory_param], Some(number), reflected_body))

fn main(console: Console):
    console.print("${composed()}")
    console.print("${direct()}")
    console.print("${generated_selected()}")
    console.print("${statement_generated()}")
    console.print("${generated_identity(([42], 42))}")
    console.print("${generated_apply(fn(value: Number): value)}")
    console.print("${generated_frozen_identity(42)}")
    console.print("${generated_reflected(([42], fn(value: Number): value, 42))}")
"#;

#[test]
fn compiler_owned_statements_and_blocks_build_admitted_wasm_without_source_reparse() {
    use witchy::opt::{self, OptSet};
    use witchy::runtime::{Capabilities, Runtime};

    struct ResetOptimizationOverride;
    impl Drop for ResetOptimizationOverride {
        fn drop(&mut self) { opt::set_for_tests(None); }
    }

    // Each generated function returns 42: the structural statement binds 40,
    // then adds 2; the quoted block computes 20 * 2 + 2. Their sum is 84.
    let source = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let x = meta.ident("x")
    let bind = meta.stmt_let(false, x, Some(int), meta.expr_int(40))
    let sum = meta.expr_binary("+", meta.expr_name(x), meta.expr_int(2))
    let body = meta.block([bind], Some(sum))
    emit_item(meta.function_block(true, meta.ident("generated_stmt"), [], Some(int), body))
    let quoted = quote block:
        let y = 20
        y * 2 + 2
    emit_item(meta.function_block(true, meta.ident("generated_quote"), [], Some(int), quoted))

fn main() -> Int:
    generated_stmt() + generated_quote()
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("compiler-owned statements and blocks type-check");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compiler-owned statements and blocks admit SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["84".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn compiler_owned_parameter_builds_admitted_wasm_without_source_reparse() {
    use witchy::opt::{self, OptSet};
    struct ResetOptimizationOverride;
    impl Drop for ResetOptimizationOverride {
        fn drop(&mut self) { opt::set_for_tests(None); }
    }

    // The generated function returns its one Int argument, so the result is 42.
    let source = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let value = meta.ident("value")
    let parameter = meta.param(value, int)
    let body = meta.block([], Some(meta.expr_name(value)))
    emit_item(meta.function_block(true, meta.ident("generated"), [parameter], Some(int), body))

fn main() -> Int:
    generated(42)
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("typed parameter checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compiler-owned parameter admits SafeWIR");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities { print_int: true, quiet: true, ..Default::default() },
        128,
    ).expect("admitted Wasm instance");
    actor.run().expect("admitted Wasm execution");
    assert_eq!(actor.output(), vec!["42".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn owned_body_syntax_survives_function_builders_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned body program");
    assert_eq!(parsed.compiler_stmt_syntax.len(), 1);
    assert_eq!(parsed.compiler_block_syntax.len(), 1);

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand compiler-owned bodies through tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn composed() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn direct() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn generated_selected() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn statement_generated() -> Int:"), "{expanded_source}");
    assert!(
        expanded_source.contains("List(@call_site_type:Number)"),
        "{expanded_source}"
    );
    assert!(expanded_source.contains("fn(@call_site_type:Number)"), "{expanded_source}");
    assert!(
        expanded_source.contains("frozen @call_site_type:Number"),
        "{expanded_source}"
    );
    assert!(expanded_source.contains("pub fn generated_reflected("), "{expanded_source}");
    assert!(expanded_source.contains("fn(Number) -> Number"), "{expanded_source}");
    assert!(
        expanded_source.contains("pub fn generated_capability(directory: Dir[Read])"),
        "{expanded_source}"
    );
    assert!(!expanded_source.contains("@quote_stmt"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_block"), "{expanded_source}");
    let expanded_debug = format!("{expanded_for_tooling:?}");
    assert!(expanded_debug.contains("@call_site:"), "{expanded_debug}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link compiler-owned body program");
    typeck::check(&linked).expect("typecheck expanded body program");
    assert!(linked.compiler_stmt_syntax.is_empty());
    assert!(linked.compiler_block_syntax.is_empty());

    let linked_debug = format!("{linked:?}");
    assert!(!linked_debug.contains("@call_site:"), "{linked_debug}");
    assert!(!linked_debug.contains("@call_site_type:"), "{linked_debug}");

    let expected = vec![
        "7".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "([42], 42)".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
    ];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile expanded program", 64);
}

#[test]
fn reflected_type_builder_rejects_unknown_qualifiers() {
    let source = r#"
import meta

comptime:
    let _ = meta.type_expr(meta.TQualified("shared", meta.TNamed("Int", [])))
"#;
    let parsed = parser::parse_module(source).expect("parse invalid reflected type");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("unknown reflected qualifier must fail during expansion")
        .to_string();
    assert!(
        error.contains("meta.type_expr") && error.contains("unknown qualifier `shared`"),
        "{error}"
    );
}

#[test]
fn rfc0167_typed_builders_reject_retired_view_and_local_unique() {
    for builder in [
        "meta.type_expr(meta.TNamed(\"View\", [meta.TNamed(\"String\", []), meta.TNamed(\"'a\", [])]))",
        "meta.type_expr(meta.TNamed(\"List\", [meta.TNamed(\"View\", [meta.TNamed(\"String\", []), meta.TNamed(\"'a\", [])])]))",
        "meta.type_named(meta.ident(\"View\"), [meta.type_expr(meta.TNamed(\"String\", [])), meta.type_expr(meta.TNamed(\"'a\", []))])",
        "meta.type_named(meta.call_site(\"View\"), [meta.type_expr(meta.TNamed(\"String\", [])), meta.type_expr(meta.TNamed(\"'a\", []))])",
        "meta.type_expr(meta.TQualified(\"local unique\", meta.TNamed(\"String\", [])))",
        "__meta_type_qualified(\"local unique\", meta.type_expr(meta.TNamed(\"String\", [])))",
    ] {
        let source = format!("import meta\n\ncomptime:\n    let _ = {builder}\n");
        let parsed = parser::parse_module(&source).expect("parse builder call");
        let error = pipeline::link(vec![("main".into(), parsed)], "main")
            .expect_err("retired type shape must fail during expansion").to_string();
        let expected = if builder.starts_with("__meta_") { "compiler-private intrinsic" }
            else if builder.contains("local unique") { "unknown qualifier `local unique`" }
            else { "`View(T, 'a)` is retired; use `&T` (or `&[T]` for a slice view)" };
        assert!(error.contains(expected), "{builder}: {error}");
    }
}

#[test]
fn rfc0167_typed_builders_reject_emitted_nominal_view_lifetimes() {
    let source = r#"import meta

type View(a):
    value: a

comptime:
    let lifetime = meta.type_expr(meta.TNamed("'a", []))
    let view = meta.type_named(meta.call_site("View"), [lifetime])
    let value = meta.ident("value")
    let parameter = meta.param(value, view)
    let body = meta.block([], Some(meta.expr_int(0)))
    emit_item(meta.function_block(true, meta.ident("bad"), [parameter], Some(meta.type_named(meta.ident("Int"), [])), body))
"#;
    let parsed = parser::parse_module(source).expect("parse nominal builders");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("typed builder constructs the nominal syntax value");
    let error = typeck::check(&linked)
        .expect_err("ordinary type checking must reject the emitted lifetime argument")
        .to_string();
    assert!(
        error.contains("callable `bad` uses lifetime argument `'a`, but no parameter binds that lifetime"),
        "{error}"
    );
}
