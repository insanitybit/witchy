//! Internal toolchain erasure fixtures use the ordinary source-bearing loader
//! and admission APIs. They do not make the private bridge callable by users.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn canonical_channel_scheduler_three_legs() {
    let source = include_str!("../../examples/channels/src/channels.witchy");
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let expected = ["got 1", "got 2", "got 3", "got 4", "channel drained"]
        .map(str::to_owned).to_vec();
    assert_eq!(
        witchy::interpreter::run_checked_module(&checked, ".", vec![]).unwrap(),
        expected,
    );
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the checked channel scheduler admits");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let oracle = interpret_program(&validated, function, arguments, Default::default()).unwrap();
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Default::default() },
        256,
    ).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}
