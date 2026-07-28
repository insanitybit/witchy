//! RFC-0080 compiler-owned item templates with typed AST holes.

use witchy::{ast, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn generated_item() -> meta.ItemSyntax:
    let ty = quote type:
        Int
    let binding = quote pattern:
        value
    let seed = quote expr:
        .{value: 40}.value
    let tail = quote expr:
        2
    quote item:
        pub fn generated(x: ${ty}) -> ${ty}:
            let ${binding} = ${seed}
            x + value + ${tail}

comptime:
    emit_item(generated_item())

fn main(console: Console):
    console.print("${generated(1)}")
"#;

#[test]
fn mixed_item_holes_expand_as_ast_and_run_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural item-hole program");
    assert_eq!(parsed.compiler_item_syntax.len(), 1, "quote owns one item template");
    assert_eq!(
        parsed.compiler_type_syntax.len(),
        1,
        "the hole-free type quote retains compiler-owned AST"
    );
    assert_eq!(
        parsed.compiler_pattern_syntax.len(),
        1,
        "the hole-free pattern quote retains compiler-owned AST"
    );
    assert_eq!(
        parsed.compiler_expr_syntax.len(),
        2,
        "both hole-free expression quotes retain compiler-owned AST"
    );
    let template = &parsed.compiler_item_syntax[0];
    let ast::Item::Function(generated) = &template.item else {
        panic!("expected a compiler-owned function template");
    };
    assert_eq!(generated.name, "generated");
    assert!(
        format::module(&parsed, &[]).contains("meta.item_join_syntax("),
        "the formatter must expose only the public typed syntax surface"
    );

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand through the tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn generated(x: Int) -> Int:"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_item"), "{expanded_source}");
    assert!(!expanded_source.contains("__witchy_quote"), "{expanded_source}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link structural item-hole program");
    typeck::check(&linked).expect("typecheck structurally expanded item");
    assert!(linked.compiler_item_syntax.is_empty(), "runtime module drops syntax payloads");
    assert!(linked.compiler_type_syntax.is_empty(), "runtime module drops type payloads");
    assert!(linked.compiler_pattern_syntax.is_empty(), "runtime module drops pattern payloads");
    assert!(linked.items.iter().any(
        |item| matches!(item, ast::Item::Function(function) if function.name.ends_with("generated"))
    ));

    let expected = vec!["43".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile expanded item", 64);
}
