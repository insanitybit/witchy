//! Dictionary replacement must preserve value semantics and owned children.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

/// (RFC-0167 §4) In `mode opt` a proven owner that was aliased by an ordinary
/// binding is demoted; a later `unique`-requiring mutation is refused rather
/// than repaired by a hidden copy. The normal-mode leg keeps proving the
/// snapshot semantics; the opt leg pins the refusal.
fn assert_opt_refuses_mutation_after_alias(source: &str) {
    let error = witchy::resolve_std_only_checked(&format!("mode opt\n{source}"))
        .expect_err("opt mode refuses mutating an aliased owner")
        .to_string();
    assert!(
        error.contains("cannot repair ordinary storage into a `unique` parameter"),
        "{error}"
    );
}

fn assert_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("dictionary source checks");
    assert_checked_three_legs(&checked, expected);
}

fn assert_checked_three_legs(checked: &witchy_types::pipeline::CheckedModule, expected: &[&str]) {
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter executes dictionary mutation");
    assert_eq!(source_output, expected, "independent expected output");
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("dictionary mutation admits");
    let entry = artifact.entry().expect("source entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(checked)
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
fn option_string_dictionary_values_distinguish_absence_empty_and_inactive_payloads() {
    let source = r#"
fn describe(value: Option(String)) -> String:
    match value:
        Some(text) -> "some[" + text + "]"
        None -> "none"

fn extracted(value: Option(Option(String))) -> String:
    match value:
        Some(item) -> "present:" + describe(item)
        None -> "missing"

fn revive(value: Option(String)) -> Option(String):
    match value:
        Some(text) -> Some(text + "!")
        None -> Some("")

fn identity(value: Option(String)) -> Option(String):
    value

fn main(console: Console):
    var d: Dict(String, Option(String)) = dict.new()
    console.print(extracted(dict.insert(d, "a", Some(""))))
    let retained = dict.at(d, "a")
    console.print(extracted(dict.insert(d, "a", None)))
    console.print(extracted(dict.insert(d, "a", Some("live"))))
    console.print(extracted(dict.insert(d, "a", None)))
    let _ = dict.insert(d, "b", Some(""))
    let removed = dict.remove(d, "b")
    let missing = dict.remove(d, "missing")
    let snapshot = d
    let _ = dict.update(d, "a", Some("ignored"), revive)
    let _ = dict.update(d, "c", Some("seed"), revive)
    let _ = dict.update(d, "n", None, identity)
    console.print(extracted(removed))
    console.print(extracted(missing))
    console.print(describe(retained))
    console.print(describe(dict.at(snapshot, "a")))
    console.print(describe(dict.at(d, "a")))
    console.print(describe(dict.at(d, "c")))
    console.print(describe(dict.at(d, "n")))
    console.print(extracted(dict.get(d, "n")))
    console.print(extracted(dict.get(d, "missing")))
    console.print(describe(dict.get_or(d, "missing", Some("fallback"))))
    let values = dict.values(d)
    console.print(describe(values[2]))
"#;
    let expected = [
        "missing", "present:some[]", "present:none", "present:some[live]",
        "present:some[]", "missing", "some[]", "none", "some[]", "some[seed!]",
        "none", "present:none", "missing", "some[fallback]", "none",
    ];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn list_append_growth_fits_bounded_heap_and_preserves_scalar_snapshots() {
    // Exact-length allocation at every append requests over 64 MiB of Int
    // payloads here. Keep the shared 16 MiB execution limit, without asserting
    // a particular capacity policy or timing the operation.
    let source = r#"
fn main(console: Console):
    var xs: List(Int) = []
    for i in 0..17:
        list.push(xs, i)
    let snapshot = xs
    for i in 17..4096:
        list.push(xs, i)
    var fork = snapshot
    list.push(fork, -1)
    var total = 0
    for value in xs:
        total = total + value
    console.print("${list.length(xs)} ${list.length(snapshot)} ${list.length(fork)}")
    console.print("${xs[0]} ${xs[4095]} ${snapshot[16]} ${fork[17]}")
    console.print("${total}")
"#;
    let expected = ["4096 17 18", "0 4095 16 -1", "8386560"];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn list_append_growth_preserves_initialized_pointer_prefixes_and_nested_snapshots() {
    let source = r#"
fn main(console: Console):
    var words: List(String) = []
    var rows: List(List(String)) = []
    list.push(rows, [])
    for i in 0..96:
        var row: List(String) = []
        list.push(row, "row:" + i.show())
        list.push(row, "")
        list.push(rows, row)
        list.push(row, "after-storage")
        list.push(words, "word:" + i.show())
    let word_snapshot = words
    let row_snapshot = rows
    for i in 96..192:
        list.push(words, "word:" + i.show())
    list.push(rows, ["tail"])
    var first = rows[1]
    list.push(first, "changed")
    var fields = 0
    for row in row_snapshot:
        fields = fields + list.length(row)
    console.print("${list.length(words)} ${list.length(word_snapshot)}")
    console.print(words[0] + "|" + word_snapshot[95] + "|" + words[191])
    console.print("${list.length(rows)} ${list.length(row_snapshot)} ${list.length(rows[0])} ${fields}")
    console.print(rows[1].join("|"))
    console.print(row_snapshot[96].join("|"))
    console.print(rows[97].join("|"))
    console.print(first.join("|"))
    console.print(row_snapshot[1].join("|"))
"#;
    let expected = [
        "192 96", "word:0|word:95|word:191", "98 97 0 192",
        "row:0|", "row:95|", "tail", "row:0||changed", "row:0|",
    ];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn dictionary_append_growth_preserves_private_clone_capacity_and_snapshots() {
    // Repeated insertion clones both private lists before appending. This
    // exercises bounded execution even when those clones intervene between
    // successive appends; the assertions depend only on source values.
    let source = r#"
fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    for i in 0..17:
        d[i] = i * 3
    let snapshot = d
    let snapshot_keys = dict.keys(snapshot)
    for i in 17..2048:
        d[i] = i * 3
    let keys = dict.keys(d)
    let values = dict.values(d)
    var total = 0
    for value in values:
        total = total + value
    console.print("${dict.length(d)} ${dict.length(snapshot)} ${list.length(snapshot_keys)}")
    console.print("${dict.at(snapshot, 16)} ${dict.get_or(snapshot, 17, -1)} ${snapshot_keys[16]}")
    console.print("${keys[0]} ${keys[2047]} ${values[0]} ${values[2047]}")
    console.print("${total}")
"#;
    let expected = ["2048 17 17", "48 -1 16", "0 2047 0 6141", "6288384"];
    assert_three_legs(source, &expected);
    assert_opt_refuses_mutation_after_alias(source);
}
