//! RFC-0080 compiler-owned item syntax: hole-free item quotes stay as AST
//! through comptime evaluation and preserve typed emission order.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{ast, codegen, comptime, format, interpreter, parser, pipeline, typeck};

const SOURCE: &str = r#"
import meta

comptime fn make_first() -> ItemSyntax:
    quote item:
        pub fn structural_first() -> Int:
            1

comptime:
    emit_item(make_first())
    let source_middle = "pub fn source_middle() -> Int:\n    2"
    emit_item(meta.item(source_middle))
    emit_item(quote item:
        pub fn structural_last() -> Int:
            3
    )

fn main(console: Console):
    console.print("${structural_first()}")
    console.print("${source_middle()}")
    console.print("${structural_last()}")
"#;

#[test]
fn compiler_owned_items_expand_in_order_and_run_on_both_backends() {
    let parsed = parser::parse_module(SOURCE).expect("parse structural item quotes");
    assert_eq!(
        parsed.compiler_item_syntax.len(),
        2,
        "only the two hole-free item quotes belong to the compiler-owned table"
    );
    assert!(parsed.compiler_item_syntax.iter().all(|syntax| {
        matches!(&syntax.item, ast::Item::Function(function)
            if function.name == "structural_first" || function.name == "structural_last")
    }));

    let mut expanded_for_tooling = parsed.clone();
    comptime::expand_compile_time("main", &mut expanded_for_tooling, &[])
        .expect("expand through the tooling callback");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn structural_first()"), "{expanded_source}");
    assert!(expanded_source.contains("pub fn source_middle()"), "{expanded_source}");
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
                    || function.name.ends_with("source_middle")
                    || function.name.ends_with("structural_last") =>
            {
                Some(function.name.rsplit('.').next().unwrap_or(&function.name).to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        generated,
        ["structural_first", "source_middle", "structural_last"],
        "typed item events must preserve their emission order"
    );

    let expected = vec!["1".to_string(), "2".to_string(), "3".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect("interpret expanded structural item program"),
        expected
    );

    let wasm = codegen::compile_module_binary(&linked)
        .expect_lowered("compile expanded structural item program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            128,
        )
        .expect("spawn compiled structural item program");
    actor.run().expect("run compiled structural item program");
    assert_eq!(actor.output(), expected);
}

#[test]
fn dynamic_item_source_becomes_an_owned_module_fragment_with_imports() {
    let source = r#"
import meta

comptime fn build(name: String) -> ItemSyntax:
    let nl = "\n"
    let declaration = "import show" + nl + nl + "pub fn " + name + "() -> String:" + nl + "    show.render(7)"
    meta.item(declaration)

comptime:
    emit_item(build("dynamic_owned"))

fn main(console: Console):
    console.print(dynamic_owned())
"#;
    let parsed = parser::parse_module(source).expect("parse dynamic item producer");
    let linked = pipeline::link_with_origins(vec![("main".into(), parsed)], "main")
        .expect("dynamic item parses once and preserves its import fragment");
    typeck::check(linked.module()).expect("typecheck dynamic owned item");
    let generated = linked
        .module()
        .items
        .iter()
        .position(|item| matches!(item, ast::Item::Function(function) if function.name.ends_with("dynamic_owned")))
        .expect("dynamic generated function");
    assert!(linked.origins().origin_for_item(generated).is_some());

    let expected = vec!["7".to_string()];
    assert_eq!(
        interpreter::run_module(linked.module().clone(), ".", Vec::new()).expect("interpret"),
        expected
    );
    let wasm = codegen::compile_module_binary(linked.module()).expect_lowered("compile");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            128,
        )
        .expect("spawn");
    actor.run().expect("run compiled program");
    assert_eq!(actor.output(), expected);
}
