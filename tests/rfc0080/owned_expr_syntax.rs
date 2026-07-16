//! RFC-0080 compiler-owned expression payloads with compatibility projection.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

fn identity(x: Int) -> Int:
    x

comptime fn generated_item() -> meta.ItemSyntax:
    let callee = quote expr:
        identity
    let argument = quote expr:
        42
    let call = meta.expr_call(callee, [argument])
    quote item:
        pub fn generated() -> Int:
            ${call}

comptime:
    emit_item(generated_item())

fn main(console: Console):
    console.print("${generated()}")
"#;

#[test]
fn owned_expressions_survive_direct_flow_and_project_for_builders() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned expressions");
    assert_eq!(parsed.compiler_expr_syntax.len(), 2);
    assert_eq!(parsed.compiler_item_syntax.len(), 1);

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand compiler-owned expressions through tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn generated() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("identity(42)"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_expr"), "{expanded_source}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link compiler-owned expressions");
    typeck::check(&linked).expect("typecheck expanded expression program");
    assert!(linked.compiler_expr_syntax.is_empty());
    assert!(linked.compiler_type_syntax.is_empty());
    assert!(linked.compiler_item_syntax.is_empty());

    let expected = vec!["42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile expanded program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled program");
    assert_eq!(actor.output(), expected);
}
