//! RFC-0167 typed string and binary expression builders.

use witchy::{ast::{BinOp, Expr, Item, Stmt}, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

fn malicious() -> String:
    "executed"

comptime:
    let int = meta.type_named(meta.ident("Int"), [])
    let string = meta.type_named(meta.ident("String"), [])
    let literal = meta.expr_string("\" + malicious() + \" \\ \${malicious}")
    let value = meta.ident("value")
    let answer = meta.expr_binary("+", meta.expr_name(value), meta.expr_int(2))
    emit_item(meta.function(true, meta.ident("literal"), [], Some(string), literal))
    emit_item(meta.function(true, meta.ident("answer"), [meta.param(value, int)], Some(int), answer))

fn main(console: Console):
    console.print(literal())
    console.print("${answer(40)}")
"#;

fn generated_expr<'a>(module: &'a witchy::ast::Module, name: &str) -> &'a Expr {
    let name = format!("main.{name}");
    let function = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == name => Some(function),
        _ => None,
    }).unwrap_or_else(|| panic!("missing generated function `{name}`"));
    match function.body.stmts.last() {
        Some(Stmt::Expr(expr)) => expr,
        other => panic!("expected expression body for `{name}`, got {other:?}"),
    }
}

#[test]
fn string_and_binary_builders_transfer_owned_ast_without_source_injection() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural string and binary builders");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural string and binary builders");

    assert!(matches!(
        generated_expr(&linked, "literal"),
        Expr::Str(value) if value == "\" + malicious() + \" \\ ${malicious}"
    ));
    assert!(matches!(
        generated_expr(&linked, "answer"),
        Expr::Binary { op: BinOp::Add, lhs, rhs }
            if matches!(lhs.as_ref(), Expr::Var(name) if name == "value")
                && matches!(rhs.as_ref(), Expr::Int(2))
    ));
    typeck::check(&linked).expect("typecheck structural string and binary builders");

    let expected = vec!["\" + malicious() + \" \\ ${malicious}".to_string(), "42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );
    super::assert_compiled_output(&linked, &expected, "compile", 64);
}

#[test]
fn binary_builder_rejects_non_whitelisted_operators_before_any_source_is_formed() {
    let source = r#"
import meta

comptime:
    let _ = meta.expr_binary("; malicious()", meta.expr_int(1), meta.expr_int(2))
"#;
    let parsed = parser::parse_module(source).expect("parse invalid binary builder");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("binary builder must reject unrecognized operators")
        .to_string();
    assert!(error.contains("meta.expr_binary: unsupported operator `; malicious()`"), "{error}");
}
