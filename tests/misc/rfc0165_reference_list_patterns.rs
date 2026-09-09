//! Reference list patterns retain elements and copy tails with value semantics.

#[test]
fn reference_list_patterns_preserve_values_and_reclaim_across_three_legs() {
    let source = r#"
fn describe(xs: List(String)) -> String:
    match xs:
        [first, second, ..rest] -> first + ":" + second + ":${rest.length()}"
        [only] -> only
        [] -> "empty"

fn suffix(xs: List(String)) -> List(String):
    match xs:
        [_, ..rest] -> rest
        [] -> []

fn first(xs: List(String)) -> String:
    match xs:
        [head, ..] -> head
        [] -> "empty"

fn optional_head(xs: List(Option(String))) -> String:
    match xs:
        [head, ..] ->
            match head:
                Some(value) -> value
                None -> "absent"
        [] -> "empty"

fn main(console: Console):
    console.print(describe([]))
    console.print(describe(["one"]))
    let words = ["aa", "bbb", "cccc"]
    console.print(describe(words))
    let retained = words
    var tail = suffix(words)
    tail.push("added")
    console.print(first(tail))
    console.print(first(retained))
    console.print("${tail.length()}:${retained.length()}:${words.length()}")
    console.print("${suffix(["only"]).length()}:${suffix([]).length()}")
    console.print(first(["held"]))
    console.print(optional_head([None, Some("unread")]))
    console.print(optional_head([Some("present")]))
"#;
    let expected = ["empty", "one", "aa:bbb:1", "bbb", "aa", "3:3:3", "0:0", "held", "absent", "present"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    let checked = witchy::resolve_std_only_checked(source).expect("reference list source checks");
    let output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter");
    assert_eq!(output, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("checked reference list patterns admit");
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
    assert_eq!(oracle.leaked_objects, 0, "head clones and tail copies must be reclaimed");

    let mut runtime = witchy::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}
