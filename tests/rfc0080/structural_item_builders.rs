//! RFC-0080 compiler-owned impl and module builders.

use witchy::{interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

trait Answer:
    fn answer(self) -> Int

type Token:
    Token

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let answer = meta.type_named(meta.ident("Answer"), [])
    let token = meta.type_named(meta.ident("Token"), [])
    let receiver = meta.ident("self")
    let method = meta.function(false, meta.ident("answer"), [meta.param(receiver, token)], Some(int), meta.expr_int(42))
    emit_item(meta.impl_block(answer, token, [method]))

    let generated = meta.function(true, meta.ident("generated"), [], Some(int), meta.expr_int(7))
    let generated_module = meta.module([generated])
    for item in meta.module_items(generated_module):
        emit_item(item)

fn main(console: Console):
    console.print("${Token.answer()}")
    console.print("${generated()}")
"#;

#[test]
fn impl_and_module_builders_remain_structural_through_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural item builders");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural item builders");
    typeck::check(&linked).expect("typecheck structural item builders");

    let expected = vec!["42".to_string(), "7".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    super::assert_compiled_output(&linked, &expected, "compile", 128);
}

#[test]
fn module_syntax_is_compile_time_only() {
    let source = r#"
import meta

fn leak(value: meta.ModuleSyntax) -> meta.ModuleSyntax:
    value
"#;
    let parsed = parser::parse_module(source).expect("parse runtime ModuleSyntax leak");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link runtime ModuleSyntax leak");
    let error = typeck::check(&linked)
        .expect_err("ModuleSyntax must not cross into runtime code")
        .to_string();
    assert!(error.contains("meta.ModuleSyntax") && error.contains("compile-time-only"), "{error}");
}
