//! RFC-0080 structural substitution for hole-bearing type syntax.

use witchy::{interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let record = quote type:
        .{value: ${int}}
    let records = quote type:
        List(${record})
    emit_item(quote item:
        pub fn generated(values: ${records}) -> Int:
            values.at(0).value
    )

fn main(console: Console):
    console.print("${generated([.{value: 42}])}")
"#;

#[test]
fn type_holes_remain_structural_through_nested_quotes_and_items() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural type holes");
    assert!(
        parsed.compiler_type_syntax.len() >= 2,
        "expected type templates to be compiler-owned"
    );

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural type holes");
    typeck::check(&linked).expect("typecheck structural type holes");
    assert!(linked.compiler_type_syntax.is_empty());

    let expected = vec!["42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    super::assert_compiled_output(&linked, &expected, "compile structural holes", 64);
}
