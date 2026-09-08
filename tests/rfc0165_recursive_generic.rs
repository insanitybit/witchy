//! Concrete generic recursive nominals must carry their instantiated layout and
//! ownership through the source interpreter, canonical oracle, and admitted Wasm.

#[test]
fn recursive_generic_stack_int_and_string_agree_across_three_legs() {
    let source = r#"
type Stack:
    Empty
    Push(a, Stack(a))

fn sum_int(stack: Stack(Int)) -> Int:
    match stack:
        Empty -> 0
        Push(value, rest) -> value + sum_int(rest)

fn string_weight(stack: Stack(String)) -> Int:
    match stack:
        Empty -> 0
        Push(value, rest) -> value.length() + string_weight(rest)

fn main(console: Console):
    let ints = Push(1, Push(1, Empty))
    let words = Push("x", Push("yy", Empty))
    console.print("${sum_int(ints) + string_weight(words)}")
"#;
    assert_three_legs(source, vec!["5".to_owned()]);
}

#[test]
fn generic_templates_do_not_shift_executable_tail_site_owners() {
    assert_three_legs(include_str!("../examples/generic_stack/src/generic_stack.witchy"), vec![
        "nums size:  3".to_owned(),
        "words size: 2".to_owned(),
        "nums top:   1".to_owned(),
        "words top:  first".to_owned(),
        "rev nums top:  3".to_owned(),
        "rev words top: second".to_owned(),
    ]);
}

fn assert_three_legs(source: &str, expected: Vec<String>) {

    let checked = witchy::resolve_std_only_checked(source)
        .expect("recursive generic source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter executes both concrete Stack instantiations");
    assert_eq!(source_output, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recursive generic source admits");
    let entry = artifact.entry().expect("main entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export prefix").parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        capabilities,
        Default::default(),
    )
    .expect("canonical oracle executes both concrete Stack instantiations");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true,
            quiet: true,
            ..Default::default()
        },
        256,
    )
    .expect("admitted instance");
    vm.run().expect("Wasm executes both concrete Stack instantiations");
    assert_eq!(vm.output(), expected);
    assert_eq!(
        vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"),
        0,
    );
}
