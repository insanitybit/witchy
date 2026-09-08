//! RFC-0165 recursive nominal source admission and execution parity.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const SOURCE: &str = r#"
type Tree:
    Leaf(Int)
    Branch(Int, Tree, Tree)

fn sum(tree: Tree) -> Int:
    match tree:
        Leaf(value) -> value
        Branch(value, left, right) -> value + sum(left) + sum(right)

fn main(console: Console):
    let child = Leaf(2)
    let tree = Branch(1, child, child)
    console.print("${sum(tree)}")
"#;

#[test]
fn checked_recursive_sum_matches_source_oracle_and_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("recursive source checks");
    let expected = vec!["5"];
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter runs recursive sum");
    assert_eq!(source_output, expected, "source interpreter expected output");

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recursive nominal source must admit production lowering");
    let entry = artifact.entry().expect("recursive source entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("recursive source validates for oracle");
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
        InterpreterConfig { max_steps: 20_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs recursive sum");
    assert_eq!(oracle.output, source_output, "oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "canonical oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn admitted recursive source");
    vm.run().expect("admitted Wasm runs recursive sum");
    assert_eq!(vm.output(), source_output, "Wasm must match source interpreter");
    assert_eq!(
        vm.canonical_heap_leaks()
            .expect("heap audit")
            .expect("canonical artifact"),
        0,
        "admitted Wasm must not leak"
    );
}
