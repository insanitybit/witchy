//! Nested constructor, tuple, and list patterns keep guarded loads and RC balanced.

#[test]
fn nested_patterns_preserve_references_and_reclaim_across_three_legs() {
    let source = r#"
fn classify(value: Option((String, List(Option(String))))) -> String:
    match value:
        Some((name, [Some(first), ..tail])) -> "${name}:${first}:${tail.length()}"
        Some((name, [None, ..tail])) -> "${name}:none:${tail.length()}"
        Some((name, [])) -> "${name}:empty"
        Some(_) -> "other"
        None -> "none"

fn main(console: Console):
    let retained = "keep"
    console.print(classify(Some((retained, [Some("first"), None]))))
    console.print(retained)
    console.print(classify(Some(("n", [None, Some("later")]))))
    console.print(classify(Some(("n", []))))
    console.print(classify(None))
"#;
    let expected = ["keep:first:1", "keep", "n:none:1", "n:empty", "none"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    let checked = witchy::resolve_std_only_checked(source).expect("nested pattern source checks");
    let output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter");
    assert_eq!(output, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nested patterns admit");
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("canonical oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).expect("canonical oracle");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = witchy::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}
