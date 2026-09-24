//! RFC-0130 acceptance rows 4 and 6: source diagnostics and syntax surfaces.
//!
//! These are deliberately surface-only proofs. They do not adjudicate the
//! resumable-frame, one-time-effect, or complexity rows.

use witchy::ast::{Expr, Item, Stmt};
use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, format, interpreter, parser};

fn compiled_output(module: &witchy::pipeline::CheckedModule) -> Vec<String> {
    let wasm = codegen::compile_checked_module_binary(module)
        .expect_lowered("compile reflected generator syntax");
    let mut runtime = Runtime::batch().expect("create RFC-0130 surface runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn reflected generator syntax");
    actor.run().expect("run reflected generator syntax");
    actor.output()
}

#[test]
fn rfc0130_row_6_formatter_reflection_docs_and_editor_surfaces_preserve_generators() {
    let rough = "import iter\n\npub gen fn values(n:Int)->Iter(Int):\n yield n\n";
    let formatted = format::reformat(rough).expect("generator syntax must format");
    assert!(
        formatted.contains("pub gen fn values(n: Int) -> Iter(Int):")
            && formatted.contains("    yield n"),
        "formatter dropped generator syntax: {formatted}",
    );
    assert_eq!(
        format::reformat(&formatted).as_deref(),
        Some(formatted.as_str()),
        "generator formatting must be idempotent",
    );

    let parsed = parser::parse_module(&formatted).expect("formatted generator must parse");
    let function = parsed
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "values" => Some(function),
            _ => None,
        })
        .expect("formatted generator function survives AST reflection");
    assert!(function.is_gen, "reflected function lost its generator qualifier");
    assert!(
        matches!(function.body.stmts.as_slice(), [Stmt::Yield(Expr::Var(name))] if name == "n"),
        "reflected function lost its yield statement: {:?}",
        function.body.stmts,
    );

    let rendered = witchy_syntax::doc::render("numbers", &formatted)
        .expect("generator docs must render");
    assert!(
        rendered.contains("#### `gen fn values(n: Int) -> Iter(Int)`"),
        "generated docs dropped the generator qualifier: {rendered}",
    );

    let highlights = include_str!("../../editors/zed/languages/witchy/highlights.scm");
    assert!(
        highlights.contains("\"fn\" \"gen\"")
            && highlights.contains("\"return\" \"yield\"")
            && highlights.contains("(function_definition name: (identifier) @function)"),
        "Zed's pinned editor query does not preserve generator declarations",
    );
    let editor_config = include_str!("../../editors/zed/languages/witchy/config.toml");
    assert!(editor_config.contains("grammar = \"witchy\""));

    let reflected_source = r#"
import iter
import meta

comptime fn generator_item() -> ItemSyntax:
    quote item:
        gen fn generated_values() -> Iter(Int):
            yield 20
            yield 22

comptime:
    emit_item(generator_item())

fn main(console: Console):
    let values: List(Int) = iter.collect(generated_values())
    console.print("${values}")
"#;
    let checked = witchy::resolve_std_only_checked(reflected_source)
        .expect("quoted generator item must survive compile-time reflection");
    let expected = vec!["[20, 22]".to_string()];
    assert_eq!(
        interpreter::run_checked_module(&checked, ".", Vec::new())
            .expect("interpret reflected generator syntax"),
        expected,
    );
    assert_eq!(compiled_output(&checked), expected);
}
