//! Capability options preserve absence without storing authority in memory.

#[test]
fn nullable_capability_brands_agree_across_all_execution_routes() {
    let source = r#"
capability Output from Console

fn choose(console: Console, present: Bool) -> Option(Output):
    if present:
        Some(Output(console))
    else:
        None

fn emit(output: Output):
    match output:
        Output(console) -> console.print("present")

fn main(console: Console):
    if let Some(output) = choose(console, true):
        emit(output)
    else:
        console.print("wrong")
    if let Some(output) = choose(console, false):
        emit(output)
    else:
        console.print("absent")
"#;
    let expected = vec!["present", "absent"];
    let checked = witchy::resolve_std_only_checked(source).expect("capability fixture checks");
    let interpreted = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter");
    assert_eq!(interpreted, expected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nullable capability fixture admits");
    let entry = artifact.entry().expect("entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("independent oracle validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, Default::default(),
    ).expect("SafeWIR oracle");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted instance");
    vm.run().expect("admitted Wasm");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn nullable_capability_source_refuses_forgery_and_unchecked_use() {
    assert!(witchy::resolve_std_only_checked(
        "capability Output from Console\nfn main(console: Console):\n    let forged = Output(1)\n"
    ).is_err());
    assert!(witchy::resolve_std_only_checked(
        "fn bad(console: Option(Console)):\n    console.print(\"unchecked\")\nfn main(console: Console):\n    bad(None)\n"
    ).is_err());
}
