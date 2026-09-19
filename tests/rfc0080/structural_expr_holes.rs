//! RFC-0080 structural substitution for hole-bearing expression syntax.

use witchy::{interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import list
import meta

comptime fn generated_body() -> meta.ExprSyntax:
    let record = quote expr:
        .{value: 40}
    quote expr:
        ${record}.value + 2

comptime fn add_one(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_binary("+", meta.expr_tag_hole(holes.at(0)), meta.expr_int(1))

comptime:
    let body = generated_body()
    emit_item(quote item:
        pub fn generated() -> Int:
            ${body}
    )

fn main(console: Console):
    let n = 41
    console.print("${generated()}")
    console.print("${add_one"value ${n}"}")
"#;

#[test]
fn expression_holes_remain_structural_through_items_and_tags() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural expression holes");
    assert_eq!(
        parsed.compiler_expr_syntax.len(),
        2,
        "only the two quote expressions belong to the parser-owned syntax table; add_one is a direct structural builder"
    );

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural expression holes");
    typeck::check(&linked).expect("typecheck structural expression holes");
    assert!(linked.compiler_expr_syntax.is_empty());

    let expected = vec!["42".to_string(), "42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    super::assert_compiled_output(&linked, &expected, "compile structural holes", 64);
}
