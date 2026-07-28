//! RFC-0080 compiler-owned pattern payloads with source-backed builder compatibility.

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
