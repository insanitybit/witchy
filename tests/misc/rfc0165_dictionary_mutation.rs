//! Dictionary replacement must preserve value semantics and owned children.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn assert_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("dictionary source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes dictionary mutation");
    assert_eq!(source_output, expected, "independent expected output");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("dictionary reference insertion admits");
    let entry = artifact.entry().expect("source entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("dictionary source validates independently");
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_program(&validated, function, arguments, InterpreterConfig {
        max_steps: 20_000_000_000, ..Default::default()
    }).expect("canonical interpreter executes dictionary mutation");
    assert_eq!(oracle.output, source_output);
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter ownership balance");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256,
    ).expect("admitted dictionary source");
    vm.run().expect("Wasm executes dictionary mutation");
    assert_eq!(vm.output(), source_output);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn string_dictionary_replacement_preserves_snapshot_and_displaced_value() {
    assert_three_legs(r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(String, String) = dict.new()
    console.print(label(dict.insert(d, "a", "first")))
    let snapshot = d
    console.print(label(dict.insert(d, "a", "replacement")))
    console.print(label(dict.insert(d, "b", "last")))
    console.print(dict.at(snapshot, "a"))
    console.print(dict.at(d, "a"))
    console.print(dict.at(d, "b"))
    console.print(dict.keys(d).join(","))
"#, &["none", "some:first", "none", "first", "replacement", "last", "a,b"]);
}

#[test]
fn int_key_string_values_preserve_empty_utf8_and_option_identity() {
    let source = r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    console.print(label(dict.insert(d, 7, "")))
    console.print(label(dict.insert(d, 7, "UTF8")))
    console.print(label(dict.insert(d, 8, "tail")))
    console.print(label(dict.insert(d, 7, "")))
    console.print(dict.at(d, 7))
    console.print(dict.at(d, 8))
"#.replace("UTF8", "\u{03bb}");
    let old = format!("some:{}", '\u{03bb}');
    let expected = ["none", "some:", "none", old.as_str(), "", "tail"];
    assert_three_legs(&source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn pure_string_dictionary_assignment_needs_no_extracted_option() {
    assert_three_legs(r#"
fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    d[1] = "old"
    let snapshot = d
    d[1] = "new"
    d[2] = "appended"
    console.print(dict.at(snapshot, 1))
    console.print(dict.at(d, 1))
    console.print(dict.at(d, 2))
"#, &["old", "new", "appended"]);
}

#[test]
fn retained_nonliteral_string_survives_reinsertion_and_replacement() {
    assert_three_legs(r#"
fn make(part: String) -> String:
    part + "-right"

fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    let _ = dict.insert(d, 1, make("left"))
    let retained = dict.at(d, 1)
    let snapshot = d
    let _ = dict.insert(d, 2, retained)
    let _ = dict.insert(d, 1, retained)
    let displaced = dict.insert(d, 1, retained + "!")
    console.print(retained)
    console.print(dict.at(snapshot, 1))
    console.print(dict.at(d, 1))
    console.print(dict.at(d, 2))
    match displaced:
        Some(text) -> console.print("some:" + text)
        None -> console.print("none")
"#, &["left-right", "left-right", "left-right!", "left-right", "some:left-right"]);
}
