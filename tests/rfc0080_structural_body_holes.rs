//! RFC-0080 structural substitution for hole-bearing statement and block syntax.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn local_stmt() -> meta.StmtSyntax:
    let int = meta.type_named(meta.ident("Int"), [])
    let seed = meta.expr_int(40)
    quote stmt:
        let x: ${int} = ${seed}

comptime fn direct_body() -> meta.BlockSyntax:
    let int = meta.type_named(meta.ident("Int"), [])
    let binding = meta.pattern_var(meta.ident("value"))
    let seed = meta.expr_int(40)
    let tail = quote expr:
        value + 2
    quote block:
        let x: ${int} = ${seed}
        let ${binding} = x
        ${tail}

comptime:
    let int = quote type:
        Int
    let composed = meta.block([local_stmt()], Some(meta.expr_name(meta.ident("x"))))
    emit_item(meta.function_block(true, meta.ident("composed"), [], Some(int), composed))
    emit_item(meta.function_block(true, meta.ident("direct"), [], Some(int), direct_body()))

fn main(console: Console):
    console.print("${composed()}")
    console.print("${direct()}")
"#;

#[test]
fn body_holes_remain_structural_until_projected_through_body_builders() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural body holes");
    assert_eq!(parsed.compiler_stmt_syntax.len(), 1);
    assert_eq!(parsed.compiler_block_syntax.len(), 1);

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand structural body holes");
    typeck::check(&linked).expect("typecheck structural body holes");
    assert!(linked.compiler_stmt_syntax.is_empty());
    assert!(linked.compiler_block_syntax.is_empty());

    let expected = vec!["40".to_string(), "42".to_string()];
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
