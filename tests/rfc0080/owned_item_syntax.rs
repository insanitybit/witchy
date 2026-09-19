//! RFC-0080 compiler-owned item syntax: hole-free item quotes stay as AST
//! through comptime evaluation and preserve typed emission order.

use witchy::{ast, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn make_first() -> ItemSyntax:
    quote item:
        pub fn structural_first() -> Int:
            1

comptime:
    emit_item(make_first())
    emit_item(quote item:
        pub fn structural_middle() -> Int:
            2
    )
    emit_item(quote item:
        pub fn structural_last() -> Int:
            3
    )

fn main(console: Console):
    console.print("${structural_first()}")
    console.print("${structural_middle()}")
    console.print("${structural_last()}")
"#;

#[test]
fn compiler_owned_items_expand_in_order_and_run_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural item quotes");
    assert_eq!(
        parsed.compiler_item_syntax.len(),
        3,
        "all emitted items are compiler-owned quotes"
    );
    assert!(parsed.compiler_item_syntax.iter().all(|syntax| {
        matches!(&syntax.item, ast::Item::Function(function)
            if matches!(function.name.as_str(), "structural_first" | "structural_middle" | "structural_last"))
    }));

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand through the tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn structural_first()"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn structural_middle()"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn structural_last()"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_item"), "{expanded_source}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand compiler-owned and compatibility item syntax");
    assert!(
        linked.compiler_item_syntax.is_empty(),
        "compiler-owned payloads must not survive into the runtime module"
    );
    typeck::check(&linked).expect("typecheck expanded structural item program");

    let generated = linked
        .items
        .iter()
        .filter_map(|item| match item {
            ast::Item::Function(function)
                if function.name.ends_with("structural_first")
                    || function.name.ends_with("structural_middle")
                    || function.name.ends_with("structural_last") =>
            {
                Some(function.name.rsplit('.').next().unwrap_or(&function.name).to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        generated,
        ["structural_first", "structural_middle", "structural_last"],
        "typed item events must preserve their emission order"
    );

    let expected = vec!["1".to_string(), "2".to_string(), "3".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect("interpret expanded structural item program"),
        expected
    );

    super::assert_compiled_output(
        &linked,
        &expected,
        "compile expanded structural item program",
        128,
    );
}

#[test]
fn dynamic_item_source_is_retired() {
    let source = r#"
import meta

comptime fn build(name: String) -> ItemSyntax:
    meta.item("pub fn retired() -> Int:\n    1")

comptime:
    emit_item(build("retired"))

fn main(console: Console):
    console.print(retired())
"#;
    let parsed = parser::parse_module(source).expect("parse dynamic item producer");
    assert!(
        !parsed.items.iter().any(
            |item| matches!(item, ast::Item::Function(function) if function.name.ends_with("retired"))
        ),
        "the rejected meta.item call must not merge its later generated item"
    );
    let error = pipeline::link_with_origins(vec![("main".into(), parsed)], "main")
        .expect_err("dynamic item parser bridge is retired before an item can merge")
        .to_string();
    assert!(
        error.contains("module `meta`") && error.contains("function `item`"),
        "{error}"
    );
}
