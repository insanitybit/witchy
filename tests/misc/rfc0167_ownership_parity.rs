//! RFC-0167 ownership rows with independent expectations, the canonical
//! SafeWIR interpreter oracle, and admitted unoptimized Wasm.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

struct PositiveCase {
    row: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const POSITIVE_CASES: &[PositiveCase] = &[
    PositiveCase {
        row: "D",
        source: r#"
fn consume(own text: String) -> String:
    text

fn main(console: Console):
    let original = "hello"
    let snapshot = original
    let result = consume(original)
    console.print("${snapshot}:${result}")
"#,
        expected: &["hello:hello"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let replacement = replace(values)
    console.print("${values[0]}:${replacement[0]}")
"#,
        expected: &["2:2"],
    },
    PositiveCase {
        row: "F",
        source: r#"
type Snapshot:
    values: List(Int)

fn replace(var snapshot: frozen Snapshot) -> frozen Snapshot:
    snapshot = Snapshot([2])
    snapshot

fn main(console: Console):
    var snapshot: frozen Snapshot = Snapshot([1])
    let replacement = replace(snapshot)
    console.print("${snapshot.values[0]}:${replacement.values[0]}")
"#,
        expected: &["2:2"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn carry(own value: frozen a) -> frozen a:
    value

fn main(console: Console):
    let value: frozen List(Int) = [7]
    let result = carry(value)
    console.print("${result[0]}")
"#,
        expected: &["7"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [3]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let action = replace
    let result = action(values)
    console.print("${values[0]}:${result[0]}")
"#,
        expected: &["3:3"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn update(var values: unique List(Int)):
    values[0] = 9

fn main(console: Console):
    var values: unique List(Int) = [1, 2]
    update(values)
    console.print("${values[0]}")
"#,
        expected: &["9"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn relay(own values: unique List(Int)) -> unique List(Int):
    values

fn main(console: Console):
    let values: unique List(Int) = [4, 5]
    let result = relay(values)
    console.print("${result}")
"#,
        expected: &["[4, 5]"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn relay(own value: unique Int) -> unique Int:
    value

fn main(console: Console):
    let value: unique Int = 41
    let result = relay(value)
    console.print("${result + 1}")
"#,
        expected: &["42"],
    },
];

fn run_positive(case: &PositiveCase, mode: &str) {
    let source = if mode == "normal" {
        case.source.to_owned()
    } else {
        format!("mode opt\n\n{}", case.source)
    };
    let checked = witchy::resolve_std_only_checked(&source)
        .unwrap_or_else(|error| panic!("row {} {mode} source checks: {error}", case.row));
    let expected: Vec<String> = case.expected.iter().map(|line| (*line).to_owned()).collect();

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("row {} {mode} unoptimized canonical lowering", case.row));
    let entry = artifact.entry().expect("canonical ownership entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .unwrap_or_else(|error| panic!("row {} {mode} canonical validation: {error}", case.row));
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        arguments,
        Default::default(),
    )
    .unwrap_or_else(|error| panic!("row {} {mode} canonical interpreter: {error}", case.row));
    assert_eq!(oracle.output, expected, "row {} {mode} canonical result", case.row);
    assert_eq!(oracle.leaked_objects, 0, "row {} {mode} canonical heap audit", case.row);

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .unwrap_or_else(|error| panic!("row {} {mode} admitted Wasm: {error}", case.row));
    vm.run()
        .unwrap_or_else(|error| panic!("row {} {mode} Wasm execution: {error}", case.row));
    assert_eq!(vm.output(), expected, "row {} {mode} admitted Wasm result", case.row);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn supported_ownership_rows_match_canonical_oracle_and_unoptimized_wasm() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for case in POSITIVE_CASES {
        run_positive(case, "normal");
        run_positive(case, "opt");
    }
}

#[test]
fn frozen_deep_mutation_is_rejected_before_lowering() {
    let direct_result = r#"
mode opt

fn pass(own values: frozen List(Int)) -> frozen List(Int):
    values

fn main():
    let values: frozen List(Int) = [1]
    var result = pass(values)
    result[0] = 2
"#;
    let indirect_result = r#"
mode opt

fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn main():
    var values: frozen List(Int) = [1]
    let action = replace
    var result = action(values)
    result[0] = 3
"#;
    let root_mutation = r#"
mode opt

fn bad(var values: frozen List(Int)):
    values[0] = 2

fn main():
    ()
"#;
    for (case, source, root) in [
        ("root mutation", root_mutation, "values"),
        ("direct frozen result", direct_result, "result"),
        ("indirect frozen result", indirect_result, "result"),
    ] {
        let error = witchy::resolve_std_only_checked(source)
            .expect_err("row F must reject deep mutation through frozen storage")
            .to_string();
        assert!(
            error.contains(&format!("cannot mutate through frozen value `{root}`")),
            "row F {case} diagnostic must identify the frozen place: {error}"
        );
    }
}

