use witchy::{codegen, interpreter, runtime::{Capabilities, Runtime}};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn drop_worklist_preserves_nested_and_shared_owned_values() {
    let source = "fn main(console: Console):\n    let inner = [\"a\", \"bb\"]\n    let outer = [inner, inner]\n    console.print(outer[0][1])\n    console.print(inner[0])\n    let after = [\"done\"]\n    console.print(after[0])\n";
    let checked = witchy::resolve_std_only_checked(source).expect("checked source");
    let expected = vec!["bb", "a", "done"];
    assert_eq!(interpreter::run_module(checked.module().clone(), ".", Vec::new()).unwrap(), expected);
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("worklist program admits");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let oracle = interpret_program(&validated, function, tokens, InterpreterConfig::default()).unwrap();
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}
