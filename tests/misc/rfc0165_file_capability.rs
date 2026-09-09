//! RFC-0165 File capability: checked source, oracle, and admitted Wasm.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const SOURCE: &str = include_str!("../../examples/file_capability/src/file_capability.witchy");

#[test]
fn file_capability_example_matches_source_oracle_and_wasm() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("File example checks");
    let expected = vec!["config: hello".to_owned()];
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter runs the File example");
    assert_eq!(source_output, expected, "source interpreter expected File output");

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("File example admits production lowering");
    let entry = artifact.entry().expect("File example entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("File example validates for the canonical oracle");
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
        InterpreterConfig {
            fixture: witchy_wir::canonical_interpreter::OracleFixture {
                root: Some(std::env::current_dir().expect("test root")),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("canonical oracle runs the File example");
    assert_eq!(oracle.output, source_output, "oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "canonical oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                quiet: true,
                dir_root: Some(std::env::current_dir().expect("test root")),
                dir_read: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn admitted File example");
    vm.run().expect("admitted Wasm runs the File example");
    assert_eq!(vm.output(), source_output, "Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
