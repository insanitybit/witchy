//! RFC-0080 hygiene seam: compiler-owned fresh identifiers are deterministic,
//! source-unspellable, and distinct across calls and expansion evaluators.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{ast, codegen, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime:
    let int = quote type:
        Int
    let left = meta.fresh("tmp")
    let right = meta.fresh("tmp")
    let body = quote expr:
        ${meta.expr_name(left)} + ${meta.expr_name(right)}
    emit_item(meta.function(true, meta.ident("sum_fresh"), [meta.param(left, int), meta.param(right, int)], Some(int), body))

comptime:
    let int = quote type:
        Int
    let value = meta.fresh("tmp")
    emit_item(meta.function(true, meta.ident("identity_fresh"), [meta.param(value, int)], Some(int), meta.expr_name(value)))

comptime fn fresh_tag(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    let _unused = meta.fresh("tag_local")
    quote expr:
        7

fn main(console: Console):
    console.print("${sum_fresh(5, 7)}")
    console.print("${identity_fresh(9)}")
    console.print("${fresh_tag"a" + fresh_tag"b"}")
"#;

fn linked(source: &str) -> ast::Module {
    let parsed = parser::parse_module(source).expect("parse RFC-0080 fresh-name program");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link RFC-0080 fresh-name program");
    typeck::check(&linked).expect("typecheck RFC-0080 fresh-name program");
    linked
}

fn fresh_parameter_names(module: &ast::Module) -> Vec<String> {
    let mut names = Vec::new();
    for item in &module.items {
        let ast::Item::Function(function) = item else {
            continue;
        };
        if function.name.ends_with("sum_fresh") || function.name.ends_with("identity_fresh") {
            names.extend(function.params.iter().map(|param| param.name.clone()));
        }
    }
    names
}

#[test]
fn fresh_identifiers_are_deterministic_distinct_and_backend_neutral() {
    let first = linked(SOURCE);
    let names = fresh_parameter_names(&first);
    assert_eq!(names.len(), 3, "expected both generated functions: {names:?}");
    assert!(
        names.iter().all(|name| name.starts_with("__witchy_fresh_")),
        "fresh names must live in the source-reserved compiler namespace: {names:?}"
    );
    assert_ne!(names[0], names[1], "two calls in one evaluator must be distinct");
    assert_ne!(names[0], names[2], "separate comptime blocks must have distinct scopes");
    assert_eq!(
        names,
        fresh_parameter_names(&linked(SOURCE)),
        "fresh-name allocation must be reproducible for identical source"
    );

    let expected = vec!["12".to_string(), "9".to_string(), "14".to_string()];
    assert_eq!(
        interpreter::run_module(first.clone(), ".", Vec::new()).expect("interpret expanded program"),
        expected
    );

    let wasm = codegen::compile_module_binary(&first).expect_lowered("compile expanded program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            128,
        )
        .expect("spawn compiled expanded program");
    actor.run().expect("run compiled expanded program");
    assert_eq!(actor.output(), expected);
}

#[test]
fn fresh_rejects_an_invalid_identifier_hint() {
    let source = r#"
import meta

comptime:
    let _name = meta.fresh("bad-name")

fn main(console: Console):
    console.print("unreachable")
"#;
    let parsed = parser::parse_module(source).expect("invalid hint program still parses");
    let error = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect_err("meta.fresh must validate its human-readable hint")
        .message;
    assert!(error.contains("meta.fresh") && error.contains("bad-name"), "{error}");
}
