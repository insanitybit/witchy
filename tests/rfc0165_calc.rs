//! Real recursive evaluator coverage with independently specified results.

#[test]
fn recursive_calc_matches_expected_results_and_reclaims_objects() {
    use witchy::codegen;
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;

    let checked = witchy::resolve_std_only_checked(
        include_str!("../examples/calc/src/calc.witchy"),
    ).expect("checked calc source");
    let cases = [
        ("2 + 3 * 4", "14"),
        ("(2 + 3) * 4", "20"),
        ("100 - 2 - 3", "95"),
        ("2 * (10 - 1)", "18"),
        ("8 / (4 - 4)", "error: division by zero"),
        ("2 * (3 +", "error: unexpected end of input"),
    ];
    let expected: Vec<String> = cases.into_iter()
        .map(|(source, answer)| format!("{source:<16}=> {answer}"))
        .collect();
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source evaluator");
    assert_eq!(source_output, expected);

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("calc production admission");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("independent validation");
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_program(&validated, function, arguments, Default::default())
        .expect("SafeWIR evaluator");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted instance");
    vm.run().expect("Wasm evaluator");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
