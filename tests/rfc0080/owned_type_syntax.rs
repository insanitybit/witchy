//! RFC-0080 compiler-owned type payloads through builders and quote holes.

use witchy::{ast, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn generated_item() -> meta.ItemSyntax:
    let record = quote type:
        .{value: Int}
    quote item:
        pub fn generated(x: ${record}) -> Int:
            x.value

comptime:
    emit_item(generated_item())

fn main(console: Console):
    console.print("${generated(.{value: 42})}")
"#;

#[test]
fn compiler_owned_type_holes_build_admitted_wasm_without_source_reparse() {
    use witchy::opt::{self, OptSet};
    use witchy::runtime::{Capabilities, Runtime};

    struct ResetOptimizationOverride;
    impl Drop for ResetOptimizationOverride {
        fn drop(&mut self) { opt::set_for_tests(None); }
    }

    // The generated identity function returns the literal argument 42.
    let source = r#"
import meta

comptime:
    let int = quote type:
        Int
    let item = quote item:
        pub fn generated(value: ${int}) -> Int:
            value
    emit_item(item)

fn main() -> Int:
    generated(42)
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("compiler-owned type hole type-checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compiler-owned type hole admits SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    actor.run().expect("admitted unoptimized Wasm execution");
    assert_eq!(actor.output(), vec!["42".to_string()]);
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn owned_types_survive_direct_holes_and_run_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned type program");
    assert_eq!(parsed.compiler_type_syntax.len(), 1);
    assert_eq!(parsed.compiler_item_syntax.len(), 1);
    assert!(matches!(
        &parsed.compiler_type_syntax[0].ty,
        ast::Type::Named(name, _) if name.starts_with("__anon")
    ));

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand compiler-owned type through tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn generated(x: .{value: Int}) -> Int:"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_type"), "{expanded_source}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link compiler-owned type program");
    typeck::check(&linked).expect("typecheck expanded type program");
    assert!(linked.compiler_type_syntax.is_empty());
    assert!(linked.compiler_item_syntax.is_empty());

    let expected = vec!["42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile expanded program", 64);
}
