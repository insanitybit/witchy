//! RFC-0167 removes string-to-syntax generation in one cut. Quotation and the
//! structural `meta.*` builders are the only replacement surface.

use witchy::{comptime, interpreter, parser, pipeline, typeck};

#[test]
fn retired_raw_codegen_names_reject_before_generated_items_merge() {
    for name in [
        "item",
        "item_join_syntax",
        "expr_raw",
        "expr_join_syntax",
        "type_join",
        "pattern_join",
        "stmt_raw",
        "stmt_join_syntax",
        "block_raw",
        "block_join_syntax",
    ] {
        let source = format!(
            "import meta\n\ncomptime:\n    meta.{name}(\"1\")\n    emit_item(quote item:\n        fn must_not_merge() -> Int:\n            0\n    )\n\nfn main() -> Int:\n    0\n"
        );
        let mut parsed = parser::parse_module(&source).expect("retired spelling remains ordinary source");
        let error = comptime::expand_compile_time("main", &mut parsed, &[])
            .expect_err("retired code-generation API must not acquire a parse fallback");
        assert!(error.contains("module `meta`") && error.contains(name), "{name}: {error}");
        assert!(
            !parsed.items.iter().any(|item| {
                matches!(item, witchy::ast::Item::Function(function) if function.name == "must_not_merge")
            }),
            "{name}: rejected comptime must not merge later generated items"
        );
    }
}

#[test]
fn structural_quote_and_tag_hole_replace_raw_expression_splicing() {
    let module = parser::parse_module(
        r#"
import meta

comptime fn add_one(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_binary("+", meta.expr_tag_hole(holes.at(0)), meta.expr_int(1))

comptime:
    emit_item(quote item:
        fn quoted_answer() -> Int:
            40 + 2
    )

fn main(console: Console):
    console.print("${add_one"value ${41}"}")
    console.print("${quoted_answer()}")
"#,
    )
    .expect("parse structural replacements");
    let linked = pipeline::link(vec![("main".into(), module)], "main")
        .expect("link structural replacements");
    typeck::check(&linked).expect("typecheck structural replacements");
    assert_eq!(
        interpreter::run_module(linked, ".", Vec::new()).expect("interpret structural replacements"),
        vec!["42".to_string(), "42".to_string()],
    );
}
