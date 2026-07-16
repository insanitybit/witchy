//! RFC-0080 definition-site resolution for compiler-owned typed tag output.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{ast, codegen, interpreter, parser, pipeline, typeck};

const TAG_LIBRARY: &str = r#"
import meta

fn hidden() -> Int:
    40

comptime fn answer(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        hidden() + 2

comptime fn lexical(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    quote expr:
        (fn(hidden: Int): hidden + 1)(41)

"#;

const CONSUMER: &str = r#"
import tag_library

fn hidden() -> Int:
    0

fn main(console: Console):
    let hidden = fn() -> Int:
        1
    console.print("${answer"ignored"}")
    console.print("${lexical"ignored"}")
"#;

fn linked() -> ast::Module {
    let library = parser::parse_module(TAG_LIBRARY).expect("parse typed tag library");
    let consumer = parser::parse_module(CONSUMER).expect("parse typed tag consumer");
    let linked = pipeline::link(
        vec![("tag_library".into(), library), ("main".into(), consumer)],
        "main",
    )
    .expect("link definition-site typed tag program");
    typeck::check(&linked).expect("typecheck definition-site typed tag program");
    linked
}

#[test]
fn typed_tag_names_resolve_at_definition_site_on_both_backends() {
    let linked = linked();
    let expected = vec!["42".to_string(), "42".to_string()];

    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile typed tag program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled typed tag program");
    assert_eq!(actor.output(), expected);
}

#[test]
fn definition_site_markers_are_consumed_before_typechecking() {
    let linked = linked();
    let rendered = format!("{linked:?}");
    assert!(!rendered.contains("@definition_site:"), "{rendered}");
    assert!(rendered.contains("tag_library.hidden"), "{rendered}");
}
