//! RFC-0101 source-first pipeline promotion evidence.
//!
//! These fixtures deliberately enter through the production checked resolver.
//! They exercise source-only constructs before lowering, then reuse the same
//! checked artifact for interpreter execution, compiled-Wasm execution, and
//! RFC-0080 origin inspection.

use witchy::ast;
use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, pipeline};

struct SourceOnlyCase {
    name: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const SOURCE_ONLY_CASES: &[SourceOnlyCase] = &[
    SourceOnlyCase {
        name: "generator impl method",
        source: r#"
import iter

type Counter:
    stop: Int

impl Counter:
    gen fn values(self) -> Iter(Int):
        var value = 0
        while value < self.stop:
            yield value
            value = value + 1

fn main(console: Console):
    let values: List(Int) = iter.collect(Counter(stop: 4).values())
    console.print("${values}")
"#,
        expected: &["[0, 1, 2, 3]"],
    },
    SourceOnlyCase {
        name: "async region value",
        source: r#"
async fn answer(seed: Int) -> Int:
    seed + 2

async fn main(console: Console):
    let value = answer(40).await
    let rendered: String = region:
        "answer ${value}"
    console.print(rendered)
"#,
        expected: &["answer 42"],
    },
    SourceOnlyCase {
        name: "comptime emitted generator and async bodies",
        source: r#"
import iter

comptime:
    emit("gen fn generated_values() -> Iter(Int):")
    emit("    yield 20")
    emit("    yield 22")
    emit("fn generated_region() -> Int:")
    emit("    region -> Int:")
    emit("        42")
    emit("async fn generated_answer() -> Int:")
    emit("    generated_region()")

async fn main(console: Console):
    let values: List(Int) = iter.collect(generated_values())
    console.print("${values}")
    let answer = generated_answer().await
    console.print("${answer}")
"#,
        expected: &["[20, 22]", "42"],
    },
];

fn checked(source: &str, context: &str) -> pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{context} must pass the source-first checker: {error}"))
}

fn compiled_output(module: &pipeline::CheckedModule, context: &str) -> Vec<String> {
    let wasm = codegen::compile_checked_module_binary(module).expect_lowered(context);
    let mut runtime = Runtime::batch().expect("create source-first runtime");
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
        .unwrap_or_else(|error| panic!("spawn {context}: {error}"));
    actor
        .run()
        .unwrap_or_else(|error| panic!("run {context}: {error}"));
    actor.output()
}

fn expected(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_string()).collect()
}

#[test]
fn source_only_semantics_survive_the_checked_interpreter_and_wasm_pipeline() {
    for case in SOURCE_ONLY_CASES {
        let module = checked(case.source, case.name);
        let interpreted = interpreter::run_checked_module(&module, ".", Vec::new())
            .unwrap_or_else(|error| panic!("interpret {}: {error}", case.name));
        let compiled = compiled_output(&module, case.name);
        let expected = expected(case.expected);

        assert_eq!(interpreted, expected, "{} interpreter result", case.name);
        assert_eq!(compiled, expected, "{} compiled-Wasm result", case.name);
    }
}

const ORIGIN_CANDIDATE: &str = r#"
import meta

comptime fn build() -> ItemSyntax:
    let leaf = quote expr:
        7
    let value = quote expr:
        ${leaf}
    quote item:
        pub fn generated_with_hole() -> Int:
            ${value}

comptime:
    emit_item(build())

fn main(console: Console):
    console.print("${generated_with_hole()}")
"#;

struct DiagnosticCase {
    name: &'static str,
    source: &'static str,
    required: &'static [&'static str],
}

const DIAGNOSTIC_CASES: &[DiagnosticCase] = &[
    DiagnosticCase {
        name: "tag definition and invocation",
        source: r#"
import meta

comptime fn broken(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    missing_helper()

fn main(console: Console):
    console.print("before")
    console.print("${broken"value"}")
"#,
        required: &[
            "tagged literal `broken` at invocation line 9",
            "defined in module `main` at line 4",
            "missing_helper",
        ],
    },
    DiagnosticCase {
        name: "nested tag hole ancestry",
        source: r#"
import meta

comptime fn inner(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_raw("(")

comptime fn passthrough(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_raw(list.at(holes, 0))

fn main(console: Console):
    console.print("${passthrough"${inner"nested"}"}")
"#,
        required: &[
            "tagged literal `inner`",
            "expansion trace:",
            "tagged literal `passthrough` at invocation line 11",
            "hole 1 at hole-local line 1, column 15",
        ],
    },
];

#[test]
fn rfc0080_diagnostic_and_origin_ancestry_survive_the_checked_pipeline() {
    let module = checked(ORIGIN_CANDIDATE, "RFC-0080 origin candidate");
    let (generated_index, _) = module
        .module()
        .items
        .iter()
        .enumerate()
        .find(|(_, item)| {
            matches!(item, ast::Item::Function(function)
                if function.name.ends_with("generated_with_hole"))
        })
        .expect("generated function survives checked lowering");
    let item_origin = module
        .origins()
        .origin_for_item(generated_index)
        .expect("generated item retains its RFC-0080 origin");

    assert_eq!(item_origin.origin.definition.start.line, 9);
    assert_eq!(item_origin.origin.invocation.start.line, 13);
    assert_eq!(item_origin.origin.hole_ancestry.len(), 2);
    assert_eq!(item_origin.origin.hole_ancestry[0].definition.start.line, 7);
    assert_eq!(item_origin.origin.hole_ancestry[0].invocation.start.line, 9);
    assert_eq!(item_origin.origin.hole_ancestry[1].definition.start.line, 5);
    assert_eq!(item_origin.origin.hole_ancestry[1].invocation.start.line, 7);

    let interpreted = interpreter::run_checked_module(&module, ".", Vec::new())
        .expect("interpret RFC-0080 origin candidate");
    let compiled = compiled_output(&module, "compile RFC-0080 origin candidate");
    assert_eq!(interpreted, ["7"]);
    assert_eq!(compiled, interpreted);

    for case in DIAGNOSTIC_CASES {
        let error = witchy::resolve_std_only_checked(case.source)
            .expect_err("diagnostic candidate must be rejected")
            .to_string();
        for required in case.required {
            assert!(
                error.contains(required),
                "{} must retain `{required}`: {error}",
                case.name,
            );
        }
    }
}
