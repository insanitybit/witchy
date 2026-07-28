//! RFC-0080 compiler-owned type payloads with source-backed builder compatibility.

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
