//! Nested joins must reload live headers, not reuse a previous object's count.
use super::super::super::temp_dir::TempDir;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};

fn positive(source: &str, expected: &[&str]) {
    let dir = TempDir::new("canonical-loop-extents");
    let checked = witchy::resolve_std_only_checked(source).expect("source checks");
    let output = witchy::interpreter::run_checked_module(&checked, dir.path(), Vec::new())
        .expect("source executes");
    assert_eq!(output, expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap());
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default())
                .expect("canonical oracle executes");
            assert_eq!(run.output, expected);
            assert_eq!(run.leaked_objects, 0);
        },
    ).expect_lowered("live extent headers admit independently");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().expect("admitted Wasm executes");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn nested_loop_extents_retain_immutable_string_parameter_evidence() {
    positive(r#"
import toml
fn main(console: Console):
    let fields = toml.table("[package]\nname = \"demo\"\n", "package")
    console.print("${list.length(fields)}")
"#, &["1"]);
    positive(r#"
fn main(console: Console):
    var out = []
    for f in ["abc"]:
        var generated = []
        for c in ["cap"]:
            generated.push(c)
        out.push(f + "!")
    console.print(list.join(out, ","))
"#, &["abc!"]);
}

#[test]
fn nested_loop_extents_preserve_string_invalidation_through_scalar_while() {
    positive(r#"
fn main(console: Console):
    var out = []
    for f in ["abc"]:
        var once = true
        while once:
            var generated = []
            for c in ["cap"]:
                generated.push(c)
            once = false
        out.push(f + "!")
    console.print(list.join(out, ","))
"#, &["abc!"]);
}

#[test]
fn nested_loop_extents_reload_current_list_and_string_on_backedge() {
    positive(r#"
fn main(console: Console):
    var words = ["first"]
    let snapshot = words
    var label = "first"
    var outer = 0
    while outer < 2:
        words.push("next")
        label = label + "!"
        var inner = 0
        while inner < 1:
            inner = inner + 1
        console.print(label)
        outer = outer + 1
    console.print("${list.length(words)}")
    console.print(words[2])
    console.print("${list.length(snapshot)}")
"#, &["first!", "first!!", "3", "next", "1"]);
}

#[test]
fn nested_loop_extents_reload_current_list_on_continue_and_break() {
    positive(r#"
fn main(console: Console):
    var words = ["first"]
    let snapshot = words
    var label = "first"
    var outer = 0
    while outer < 3:
        words.push("next")
        label = label + "!"
        var inner = 0
        while inner < 1:
            inner = inner + 1
        outer = outer + 1
        if outer == 1:
            continue
        console.print(label)
        if outer == 2:
            break
    console.print("${list.length(words)}")
    console.print(words[2])
    console.print("${list.length(snapshot)}")
"#, &["first!!", "3", "next", "1"]);
}

#[test]
fn nested_loop_extents_do_not_authorize_old_count_after_replacement() {
    let source = r#"
fn main(console: Console):
    var words = ["old", "tail"]
    var once = true
    while once:
        words = ["fresh"]
        var inner = 0
        while inner < 1:
            inner = inner + 1
        once = false
    console.print("before")
    console.print(words[1])
"#;
    let dir = TempDir::new("canonical-loop-extents-bounds");
    let checked = witchy::resolve_std_only_checked(source).expect("source checks");
    let error = witchy::interpreter::run_checked_module(&checked, dir.path(), Vec::new())
        .expect_err("the current list has length one, not the old length two");
    assert!(error.to_string().contains("list index 1 out of bounds (length 1)"), "{error}");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked, |_, _| (),
    ).expect_lowered("current-count bounds probe admits independently");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
    let error = vm.run().expect_err("admitted execution must trap at the current list bound");
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("list index 1 out of bounds (length 1)"), "{diagnostic}");
    // This prefix proves admission and actual execution reached the access;
    // a compile-time refusal cannot stand in for the required bounds trap.
    assert_eq!(vm.output(), &["before"]);
    // Aborted execution is not a successful zero-leak observation.
}
