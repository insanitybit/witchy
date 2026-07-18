//! RFC-0080 compiler-owned statement and block payloads.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

type Number = Int

comptime fn local_stmt() -> meta.StmtSyntax:
    quote stmt:
        let x = 40

comptime fn direct_body() -> meta.BlockSyntax:
    quote block:
        let x = 40
        x + 2

fn selected() -> Number:
    42

fn touched() -> Nil:
    ()

comptime fn call_site_body() -> meta.BlockSyntax:
    let selected = meta.expr_name(meta.call_site("selected"))
    let call = meta.expr_call(selected, [])
    meta.block([], Some(call))

comptime fn statement_body() -> meta.BlockSyntax:
    let touched = meta.expr_name(meta.call_site("touched"))
    let touch = meta.stmt_expr(meta.expr_call(touched, []))
    let binding = meta.fresh("selected_value")
    let number = meta.type_named(meta.call_site("Number"), [])
    let selected = meta.expr_name(meta.call_site("selected"))
    let bind = meta.stmt_let(false, binding, Some(number), meta.expr_call(selected, []))
    let result = meta.stmt_return(meta.expr_name(binding))
    meta.block([touch, bind, result], Some(meta.expr_int(0)))

comptime:
    let int = quote type:
        Int
    let composed = meta.block([local_stmt()], Some(meta.expr_int(7)))
    emit_item(meta.function_block(true, meta.ident("composed"), [], Some(int), composed))
    emit_item(meta.function_block(true, meta.ident("direct"), [], Some(int), direct_body()))
    emit_item(meta.function_block(true, meta.ident("generated_selected"), [], Some(int), call_site_body()))
    emit_item(meta.function_block(true, meta.ident("statement_generated"), [], Some(int), statement_body()))
    let input = meta.ident("input")
    let number = meta.type_named(meta.call_site("Number"), [])
    let numbers = meta.type_named(meta.ident("List"), [number])
    let composite = meta.type_tuple([numbers, number])
    let identity_body = meta.block([], Some(meta.expr_name(input)))
    let identity_param = meta.param(input, composite)
    emit_item(meta.function_block(true, meta.ident("generated_identity"), [identity_param], Some(composite), identity_body))
    let callback = meta.ident("callback")
    let callback_type = meta.type_fn([number], number)
    let callback_param = meta.param(callback, callback_type)
    let callback_body = meta.block([], Some(meta.expr_call(meta.expr_name(callback), [meta.expr_int(42)])))
    emit_item(meta.function_block(true, meta.ident("generated_apply"), [callback_param], Some(number), callback_body))
    let frozen_input = meta.ident("frozen_input")
    let frozen_number = meta.type_frozen(number)
    let frozen_param = meta.param(frozen_input, frozen_number)
    let frozen_body = meta.block([], Some(meta.expr_name(frozen_input)))
    emit_item(meta.function_block(true, meta.ident("generated_frozen_identity"), [frozen_param], Some(number), frozen_body))

fn main(console: Console):
    console.print("${composed()}")
    console.print("${direct()}")
    console.print("${generated_selected()}")
    console.print("${statement_generated()}")
    console.print("${generated_identity(([42], 42))}")
    console.print("${generated_apply(fn(value: Number): value)}")
    console.print("${generated_frozen_identity(42)}")
"#;

#[test]
fn owned_body_syntax_survives_function_builders_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned body program");
    assert_eq!(parsed.compiler_stmt_syntax.len(), 1);
    assert_eq!(parsed.compiler_block_syntax.len(), 1);

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand compiler-owned bodies through tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn composed() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn direct() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn generated_selected() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn statement_generated() -> Int:"), "{expanded_source}");
    assert!(
        expanded_source.contains("List(@call_site_type:Number)"),
        "{expanded_source}"
    );
    assert!(expanded_source.contains("fn(@call_site_type:Number)"), "{expanded_source}");
    assert!(
        expanded_source.contains("frozen @call_site_type:Number"),
        "{expanded_source}"
    );
    assert!(!expanded_source.contains("@quote_stmt"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_block"), "{expanded_source}");
    let expanded_debug = format!("{expanded_for_tooling:?}");
    assert!(expanded_debug.contains("@call_site:"), "{expanded_debug}");

    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("expand and link compiler-owned body program");
    typeck::check(&linked).expect("typecheck expanded body program");
    assert!(linked.compiler_stmt_syntax.is_empty());
    assert!(linked.compiler_block_syntax.is_empty());

    let linked_debug = format!("{linked:?}");
    assert!(!linked_debug.contains("@call_site:"), "{linked_debug}");
    assert!(!linked_debug.contains("@call_site_type:"), "{linked_debug}");

    let expected = vec![
        "7".to_string(),
        "42".to_string(),
        "42".to_string(),
        "42".to_string(),
        "([42], 42)".to_string(),
        "42".to_string(),
        "42".to_string(),
    ];
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
