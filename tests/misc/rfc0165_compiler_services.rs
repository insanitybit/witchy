//! Stateless compiler-service buffers preserve expected JSON on every route.

#[test]
fn compiler_services_agree_across_source_oracle_and_admitted_wasm() {
    let source = r#"
fn main(console: Console):
    let code = "fn main(console: Console):\n    console.print(\"hi\")\n"
    console.print(compiler.footprint(code))
    console.print(compiler.diff(code, code))
    console.print(compiler.footprint("this is not valid Witchy"))
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("compiler service source checks");
    let interpreted = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source compiler services");
    assert_eq!(interpreted.len(), 3);
    assert_eq!(interpreted[0], r#"{"total":["Console"],"build":[],"user_caps":[],"entries":[{"name":"main","capabilities":["Console"],"brands":[]}]}"#);
    assert_eq!(interpreted[1], r#"{"widened":false,"added":[],"removed":[],"build_added":[],"build_removed":[],"user_caps_added":[],"user_caps_removed":[]}"#);
    assert!(interpreted[2].starts_with("{\"error\":"));
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("bounded compiler services admit");
    let entry = artifact.entry().expect("entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("oracle validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let config = witchy_wir::canonical_interpreter::InterpreterConfig {
        compiler_services: Some(witchy::compiler_oracle::call),
        ..Default::default()
    };
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, config,
    ).expect("oracle compiler services");
    assert_eq!(oracle.output, interpreted);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted instance");
    vm.run().expect("admitted compiler services");
    assert_eq!(vm.output(), interpreted);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
