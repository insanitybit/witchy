//! Iteration variables are immutable lexical bindings, not writes to a
//! same-named outer variable. All successful probes check independent source
//! expectations, canonical interpretation, admitted Wasm, and ownership.

#[test]
fn range_binder_shadows_only_after_bounds_and_restores_outer_mutability() {
    assert_both_modes(r#"
fn bound(var calls: Int, console: Console, value: Int) -> Int:
    calls = calls + 1
    console.print("bound:${value}")
    value

fn main(console: Console):
    var j = 4
    var calls = 0
    var total = 0
    for j in 1..bound(calls, console, j):
        if j == 2:
            continue
        total = total + j
        if j == 3:
            break
    console.print("${j} ${calls} ${total}")
    j = j + 5
    for j in j..j:
        total = total + 1000
    for j in j..=j:
        total = total + j
    console.print("${j} ${total}")
"#, &["bound:4", "4 1 4", "9 13"]);
}

#[test]
fn scalar_iteration_binders_preserve_shadowed_reference_owners_and_snapshots() {
    assert_both_modes(r#"
fn main(console: Console):
    var item: List(String) = ["keep", ""]
    let snapshot = item
    var total = 0
    for item in [1, 2, 3, 4]:
        if item == 2:
            continue
        total = total + item
        if item == 3:
            break
    for item in 0..2:
        total = total + item
    list.push(item, "after")
    console.print("${total}")
    console.print(item.join("|"))
    console.print(snapshot.join("|"))
"#, &["5", "keep||after", "keep|"]);
}

#[test]
fn reference_iteration_binder_evaluates_outer_iterable_once_and_restores_it() {
    assert_both_modes(r#"
fn sequence(var calls: Int, console: Console, values: List(String)) -> List(String):
    calls = calls + 1
    console.print("sequence")
    values

fn main(console: Console):
    var word: List(String) = ["", "a", "bc", "late"]
    let snapshot = word
    var calls = 0
    var total = 0
    for word in sequence(calls, console, word):
        if word == "":
            continue
        total = total + word.length()
        if word == "bc":
            break
    list.push(word, "tail")
    console.print("${calls} ${total}")
    console.print(word.join("/"))
    console.print(snapshot.join("/"))
"#, &["sequence", "1 3", "/a/bc/late/tail", "/a/bc/late"]);
}

#[test]
fn nested_range_list_and_local_scopes_keep_distinct_binders_and_outer_writebacks() {
    assert_both_modes(r#"
fn main(console: Console):
    var j = 40
    var total = 0
    for j in 1..3:
        total = total + j
        for j in [3, 4]:
            if j == 3:
                continue
            total = total + j
        if true:
            var j = 10
            j = j + 1
            total = total + j
        total = total + j
    console.print("${j} ${total}")
    j = j + 1
    console.print("${j}")
"#, &["40 36", "41"]);
}

#[test]
fn bounded_arena_and_region_shapes_allow_var_then_same_named_range_binder() {
    assert_both_modes(r#"
fn main(console: Console):
    var total = 0
    var j = 77
    for i in 0..4:
        var row: List(Int) = []
        var j = 0
        for j in 0..3:
            list.push(row, j)
        total = total + row[2] + j
    console.print("${total} ${j}")
    var regions = 0
    for i in 0..3:
        let last = region -> Int:
            var row: List(Int) = []
            var j = 0
            for j in 0..3:
                list.push(row, j)
            row[2] + j
        regions = regions + last
    console.print("${regions} ${j}")
"#, &["8 77", "6 77"]);
}

fn assert_both_modes(source: &str, expected: &[&str]) {
    assert_three_legs(source, expected);
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

fn assert_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("iteration source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes lexical iteration");
    assert_eq!(source_output, expected, "independent source expectation");

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("checked lexical iteration admits");
    let entry = artifact.entry().expect("source entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export").parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("independent canonical validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, Default::default(),
    ).expect("canonical interpreter executes lexical iteration");
    assert_eq!(oracle.output, expected, "independent canonical expectation");
    assert_eq!(oracle.leaked_objects, 0, "canonical ownership balance");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted lexical iteration instance");
    vm.run().expect("Wasm executes lexical iteration");
    assert_eq!(vm.output(), expected, "independent Wasm expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
