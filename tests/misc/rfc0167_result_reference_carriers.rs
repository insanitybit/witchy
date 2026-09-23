//! RFC-0167 rooted-view `Result` carrier.

const IMMUTABLE_RESULT_VIEW: &str = r#"
mode opt

fn choose(value: &Int, selected: Bool) -> Result(&Int, String):
    if selected:
        Ok(value)
    else:
        Err("not selected")

fn main() -> Int:
    var value = 7
    let selected = choose(&value, true)
    match selected:
        Ok(view) -> *view
        Err(_) -> 0
"#;

#[test]
fn result_immutable_view_executes_through_the_authenticated_aggregate_carrier() {
    let checked = witchy::resolve_std_only_checked(IMMUTABLE_RESULT_VIEW)
        .expect("immutable rooted-view Result should type-check");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("Result owner family must retain its rooted-view payload");
    let entry = artifact.entry().expect("Result carrier entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("numeric entry export").parse().expect("numeric entry function"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("Result carrier SafeWIR validates");
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        Vec::new(),
        Default::default(),
    )
    .expect("canonical interpreter executes Result carrier");
    assert!(
        matches!(oracle.results.as_slice(), [witchy_wir::canonical_interpreter::InterpreterValue::Int(7)]),
        "canonical interpreter reads the retained Result payload",
    );
    assert_eq!(oracle.leaked_objects, 0, "Result carrier releases its root");

    let mut runtime = witchy::runtime::Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, witchy::runtime::Capabilities::none(), 128)
        .expect("spawn admitted Result carrier");
    vm.run().expect("execute admitted Result carrier");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn result_immutable_view_rejects_region_owned_roots_during_checking() {
    let source = r#"
fn bad() -> Result(&Int, String):
    region:
        let local = 7
        Ok(&local)
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a Result view may not smuggle a region-owned root");
    assert!(
        error.to_string().contains("region-born view")
            || error.to_string().contains("per-field parent-root provenance")
            || error.to_string().contains("rooted"),
        "region-owned Result rejection should retain a fail-closed diagnostic: {error}"
    );
}
