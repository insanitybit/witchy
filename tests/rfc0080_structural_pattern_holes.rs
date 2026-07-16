//! RFC-0080 structural substitution for hole-bearing pattern syntax.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime:
    let one = meta.pattern_int(1)
    let selected = quote pattern:
        ${one} | 2
    emit_item(quote item:
        pub fn generated(value: Int) -> Int:
            match value:
                ${selected} -> 42
                _ -> 0
    )

fn main(console: Console):
    console.print("${generated(2)}")
"#;

#[test]
fn pattern_holes_remain_structural_through_nested_quotes_and_items() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural pattern holes");
    assert_eq!(
        parsed.compiler_pattern_syntax.len(),
        1,
        "expected one compiler-owned pattern template"
    );

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural pattern holes");
    typeck::check(&linked).expect("typecheck structural pattern holes");
    assert!(linked.compiler_pattern_syntax.is_empty());

    let expected = vec!["42".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret"),
        expected,
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile structural holes");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn");
    actor.run().expect("run compiled structural holes");
    assert_eq!(actor.output(), expected);
}
