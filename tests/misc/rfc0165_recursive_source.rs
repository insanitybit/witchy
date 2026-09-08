//! Recursive source graphs must terminate in an explicit compiler outcome.

#[test]
fn recursive_source_shape_support_admits_deeper_acyclic_records() {
    let source = r#"
type Inner:
    Inner(String)
type Middle:
    Middle(Inner)
type Outer:
    Outer(Middle)

fn main(console: Console):
    let value = Outer(Middle(Inner("kept")))
    match value:
        Outer(middle) ->
            match middle:
                Middle(inner) ->
                    match inner:
                        Inner(text) -> console.print(text)
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("nested source checks");
    let expected = vec!["kept"];
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter executes nested records");
    assert_eq!(source_output, expected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("acyclic nominal nesting must admit");
    let entry = artifact.entry().expect("entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, Default::default(),
    ).expect("oracle executes nested records");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted instance");
    vm.run().expect("Wasm executes nested records");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn recursive_source_graph_executes_and_reclaims_nullable_variants() {
    let source = r#"
type Tree:
    Leaf
    Node(Tree, Int, Tree)

fn main(console: Console):
    let tree = Node(Leaf, 1, Leaf)
    match tree:
        Leaf -> console.print("empty")
        Node(_, value, _) -> console.print("${value}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("recursive source is valid Witchy");
    let expected = vec!["1"];
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes recursive variants");
    assert_eq!(source_output, expected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recursive nullable variants admit");
    let entry = artifact.entry().expect("entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("oracle validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, Default::default(),
    ).expect("oracle executes recursive variants");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted instance");
    vm.run().expect("Wasm executes recursive variants");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
