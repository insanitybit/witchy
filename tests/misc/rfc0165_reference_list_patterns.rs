//! Reference list patterns retain elements and copy tails with value semantics.

const OPT_POP_HELPER: &str = r#"mode opt

pub fn take(var xs: unique List(String)) -> Option(String):
    xs.pop()
"#;

const OPT_POP_CALLER: &str = r#"import helper

fn main(console: Console):
    var xs = ["first", "last"]
    let snapshot = xs
    let removed = helper.take(xs) ?? "empty"
    console.print(removed)
    console.print("${xs.length()}:${snapshot.length()}")
    console.print(snapshot[1])
"#;

fn checked_opt_pop_boundary(opt_caller: bool) -> witchy_types::pipeline::CheckedModule {
    let caller = if opt_caller {
        format!("mode opt\n\n{OPT_POP_CALLER}")
    } else {
        OPT_POP_CALLER.to_owned()
    };
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("main".into(), caller.into_bytes()),
            ("helper".into(), OPT_POP_HELPER.as_bytes().to_vec()),
        ],
        "main",
        witchy_interp::comptime::expand_compile_time,
    ).expect("fresh checked raw-source normal/opt graph")
}

#[test]
fn normal_opt_pop_retains_snapshot_across_three_legs() {
    // Value/ownership evidence only. The separately recorded no-copy defect
    // must not be hidden by all three backends returning the correct values.
    let checked = checked_opt_pop_boundary(false);
    witchy::enforce_performance_modes(checked.module(), "main")
        .expect("normal caller may prepare an aliased unique argument");
    assert!(witchy_lower::analysis::function_is_opt(checked.module(), "helper.take"));
    let repairs = witchy_lower::analysis::module_boundary_repairs(checked.module());
    assert!(repairs.iter().any(|repair| repair.function == "main"
        && repair.callee == "helper.take" && repair.arg_index == 0), "{repairs:?}");
    let expected = vec!["last".to_owned(), "1:2".to_owned(), "last".to_owned()];
    let source = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter normal/opt boundary");
    assert_eq!(source, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal/opt pop admits");
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("independent normal/opt pop validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).expect("independent canonical oracle");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = witchy::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted normal/opt pop instance");
    vm.run().expect("admitted normal/opt pop execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn normal_opt_pop_aliased_opt_caller_refuses_no_copy_miss() {
    let checked = checked_opt_pop_boundary(true);
    let error = witchy::enforce_performance_modes(checked.module(), "main")
        .expect_err("an opt caller cannot silently prepare the aliased argument");
    assert!(error.contains("no-copy `var` contract"), "{error}");
    assert!(error.contains("helper.take"), "{error}");
}

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
