//! RFC-0165 checked-source nullable fixed-field vertical slice.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const SOURCE: &str = r#"
type Choice:
    Empty
    Text(String)

type Holder:
    Holder(Option(String))

fn option_label(value: Option(String)) -> String:
    match value:
        Some(text) -> text
        None -> "none"

fn choice_label(value: Choice) -> String:
    match value:
        Empty -> "empty"
        Text(text) -> text

fn keep_tuple(value: (Int, String)) -> Int:
    0

fn holder_label(value: Holder) -> String:
    match value:
        Holder(inner) -> option_label(inner)

fn choose(flag: Bool) -> Option(String):
    if flag:
        Some("joined")
    else:
        None

fn main(console: Console):
    let _ = keep_tuple((0, "tuple"))
    console.print(option_label(None))
    console.print(option_label(Some("option")))
    console.print(choice_label(Empty))
    console.print(choice_label(Text("choice")))
    console.print(holder_label(Holder(None)))
    console.print(holder_label(Holder(Some("nested"))))
    console.print(option_label(choose(false)))
    console.print(option_label(choose(true)))
"#;

#[test]
fn real_checked_nullable_source_matches_source_oracle_and_wasm() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("nullable source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter runs nullable source");
    let expected = vec!["none", "option", "empty", "choice", "none", "nested", "none", "joined"];
    assert_eq!(source_output, expected, "source interpreter expected output");

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nullable source must admit production lowering");
    let entry = artifact.entry().expect("nullable source entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("nullable source validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(
        &validated,
        function,
        arguments,
        InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs nullable source");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "canonical oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted nullable source");
    vm.run().expect("admitted Wasm runs nullable source");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
