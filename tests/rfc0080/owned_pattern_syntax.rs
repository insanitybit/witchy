//! RFC-0080 compiler-owned pattern payloads through builders and quote holes.

use witchy::{ast, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn generated_item() -> meta.ItemSyntax:
    let pattern = quote pattern:
        [1 | 2, ..rest]
    quote item:
        pub fn generated(xs: List(Int)) -> Bool:
            match xs:
                ${pattern} -> true
                _ -> false

comptime:
    emit_item(generated_item())

fn main(console: Console):
    console.print("${generated([2, 3, 4])}")
    console.print("${generated([9])}")
"#;

#[test]
fn compiler_owned_anonymous_pattern_builds_admitted_wasm_without_source_reparse() {
    use witchy::opt::{self, OptSet};
    use witchy::runtime::{Capabilities, Runtime};

    struct ResetOptimizationOverride;
    impl Drop for ResetOptimizationOverride {
        fn drop(&mut self) { opt::set_for_tests(None); }
    }

    // `.Some(42)` matches the generated `.Some(42)` arm, yielding 42.
    let source = r#"
import meta

type Choice = .[Some(Int) | None]

comptime:
    let p = meta.pattern_anon_ctor(meta.ident("Some"), [meta.pattern_int(42)])
    let item = quote item:
        pub fn generated(value: Choice) -> Int:
            match value:
                ${p} -> 42
                _ -> 0
    emit_item(item)

fn main() -> Int:
    generated(.Some(42))
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("compiler-owned anonymous pattern type-checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compiler-owned anonymous pattern admits SafeWIR");
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
fn owned_patterns_survive_direct_holes_and_run_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned pattern program");
    assert_eq!(parsed.compiler_pattern_syntax.len(), 1);
    assert_eq!(parsed.compiler_item_syntax.len(), 1);
    assert!(matches!(
        parsed.compiler_pattern_syntax[0].pattern,
        ast::Pattern::List { .. }
    ));

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand compiler-owned pattern through tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("[1 | 2, ..rest] -> true"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_pattern"), "{expanded_source}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link compiler-owned pattern program");
    typeck::check(&linked).expect("typecheck expanded pattern program");
    assert!(linked.compiler_pattern_syntax.is_empty());
    assert!(linked.compiler_item_syntax.is_empty());

    let expected = vec!["true".to_string(), "false".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile expanded program", 64);
}
