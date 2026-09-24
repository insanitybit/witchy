//! Pending bounded regex-spans coverage probes. Keep all three implementations
//! independently adjudicated by literal outputs, including character offsets.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn regex_invalid_pattern_aborts_all_three_implementations() {
    let source = r#"
import regex
fn main(console: Console):
    console.print("${regex.matches("[", "text")}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("invalid regex is a runtime error");
    assert!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .unwrap_err().to_string().contains("invalid regex pattern"));
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("invalid pattern still admits");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    assert!(matches!(interpret_program(&validated, function, capabilities, Default::default()),
        Err(witchy_wir::canonical_interpreter::InterpretError::ExplicitTrap)));
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).unwrap();
    assert!(vm.run().is_err(), "invalid regex must trap in admitted Wasm");
    assert!(vm.output().is_empty());
}

