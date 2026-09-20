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

#[test]
fn string_dictionary_removal_preserves_order_snapshots_and_displaced_values() {
    assert_three_legs(r#"
fn label(value: Option(String)) -> String:
    match value:
        Some(text) -> "some:" + text
        None -> "none"

fn main(console: Console):
    var d: Dict(String, String) = dict.new()
    let _ = dict.insert(d, "a", "first")
    let _ = dict.insert(d, "b", "")
    let _ = dict.insert(d, "c", "last")
    let snapshot = d
    console.print(label(dict.remove(d, "b")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "missing")))
    console.print(dict.keys(d).join(","))
    console.print(dict.keys(snapshot).join(","))
    console.print(dict.at(snapshot, "b"))
    console.print(label(dict.remove(d, "a")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "c")))
    console.print(dict.keys(d).join(","))
    console.print(label(dict.remove(d, "c")))
"#, &["some:", "a,c", "none", "a,c", "a,b,c", "", "some:first", "c", "some:last", "", "none"]);
}

#[test]
fn scalar_dictionary_removal_handles_middle_first_last_and_empty_in_both_modes() {
    let source = r#"
fn label(value: Option(Int)) -> String:
    match value:
        Some(n) -> n.show()
        None -> "none"

fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    let _ = dict.insert(d, 1, 10)
    let _ = dict.insert(d, 2, 20)
    let _ = dict.insert(d, 3, 30)
    console.print(label(dict.remove(d, 2)))
    console.print(label(dict.remove(d, 1)))
    console.print(label(dict.remove(d, 3)))
    console.print(label(dict.remove(d, 3)))
"#;
    let expected = ["20", "10", "30", "none"];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn pure_dictionary_removal_preserves_multi_entry_tail_and_missing_snapshots() {
    let source = r#"
fn main(console: Console):
    var d: Dict(Int, String) = dict.new()
    let _ = dict.insert(d, 1, "first")
    let _ = dict.insert(d, 2, "middle")
    let _ = dict.insert(d, 3, "last")
    let shorter = dict.__remove(d, 3)
    let unchanged = dict.__remove(shorter, 99)
    console.print(dict.values(d).join(","))
    console.print(dict.values(shorter).join(","))
    console.print(dict.values(unchanged).join(","))
"#;
    let privacy_error = witchy::resolve_std_only_checked(source)
        .err().expect("the pure dictionary operation remains private");
    assert!(privacy_error.to_string().contains("private to module"));

    // Exercise the private operation inside its actual bundled module, without
    // changing production std sources or weakening reserved-module ownership.
    let bundled = witchy_syntax::linker::bundled_source("dict").expect("bundled dict source");
    let entry = source.replacen(
        "fn main(console: Console):", "pub fn test_pure_remove(console: Console):", 1,
    );
    let main = "fn main(console: Console):\n    dict.test_pure_remove(console)\n";
    let mut pending = vec![
        ("dict".to_string(), format!("{bundled}\n{entry}")),
        ("main".to_string(), main.to_owned()),
    ];
    let mut loaded = std::collections::HashSet::from(["dict".to_string(), "main".to_string()]);
    for &prelude in witchy_syntax::linker::PRELUDE_MODULES {
        if loaded.insert(prelude.to_owned()) {
            let prelude_source = witchy_syntax::linker::bundled_source(prelude)
                .expect("bundled implicit prelude");
            pending.push((prelude.to_owned(), prelude_source.to_owned()));
        }
    }
    let mut sources = Vec::new();
    while let Some((name, module_source)) = pending.pop() {
        let module = witchy_syntax::parser::parse_module(&module_source)
            .expect("bundled test module parses");
        for import in module.imports {
            if loaded.insert(import.clone()) {
                let dependency = witchy_syntax::linker::bundled_source(&import)
                    .expect("bundled dependency");
                pending.push((import, dependency.to_owned()));
            }
        }
        sources.push((name, module_source.into_bytes()));
    }
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", env!("CARGO_PKG_VERSION")).unwrap();
    let application = PackageCoordinate::new(PackageSource::Workspace, "witchy/source", env!("CARGO_PKG_VERSION")).unwrap();
    let assignments: Vec<_> = sources.iter().map(|(name, _)| {
        let owner = if name == "main" {
            ModuleLoadIdentity::new(application.clone(), ["source", "main"]).unwrap()
        } else {
            ModuleLoadIdentity::new(toolchain.clone(), ["std", name.as_str()]).unwrap()
        };
        (name.clone(), owner)
    }).collect();
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    let checked = witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources, "main", &std::collections::HashSet::from(["main".to_string()]), owners,
    ).expect("authenticated dictionary entry checks with its real dependencies");
    assert_checked_three_legs(&checked, &["first,middle,last", "first,middle", "first,middle"]);
}

#[test]
fn dictionary_update_evaluates_defaults_and_calls_once_on_both_lookup_paths() {
    let source = r#"
fn default_value(console: Console) -> Int:
    console.print("default")
    100

fn changed(console: Console, value: Int) -> Int:
    console.print("callback:${value}")
    value + 1

fn main(console: Console):
    var d: Dict(Int, Int) = dict.new()
    d[1] = 5
    let callback = fn(value: Int): changed(console, value)
    let _ = dict.update(d, 1, default_value(console), callback)
    let _ = dict.update(d, 2, default_value(console), callback)
    console.print("${dict.at(d, 1)} ${dict.at(d, 2)} ${dict.length(d)}")
"#;
    let expected = ["default", "callback:5", "default", "callback:100", "6 101 2"];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn dictionary_update_preserves_string_snapshots_captures_and_aliased_results() {
    assert_three_legs(r#"
fn append(value: String) -> String:
    value + "!"

fn identity(value: String) -> String:
    value

fn main(console: Console):
    var d: Dict(String, String) = dict.new()
    d["a"] = "old"
    let _ = dict.update(d, "a", "ignored", append)
    let snapshot = d
    let retained = dict.at(d, "a")
    let callback = fn(value: String): value + retained
    let _ = dict.update(d, "a", "", callback)
    let _ = dict.update(d, "b", "new", callback)
    let _ = dict.update(d, "a", "ignored", identity)
    console.print(dict.at(snapshot, "a"))
    console.print(retained)
    console.print(dict.at(d, "a"))
    console.print(dict.at(d, "b"))
    console.print(dict.keys(d).join(","))
"#, &["old!", "old!", "old!old!", "newold!", "a,b"]);
}

#[test]
fn dictionary_capacity_hint_evaluates_once_and_negative_hint_keeps_an_empty_dict() {
    let source = r#"
fn hint(console: Console) -> Int:
    console.print("hint")
    return -5

fn main(console: Console):
    var d: Dict(Int, Int) = dict.with_capacity(hint(console))
    console.print("${dict.length(d)}")
    let _ = dict.update(d, 4, 40, fn(value: Int): value + 2)
    console.print("${dict.at(d, 4)}")
"#;
    assert_three_legs(source, &["hint", "0", "42"]);
    assert_three_legs(&format!("mode opt\n{source}"), &["hint", "0", "42"]);
}

#[test]
fn record_dictionary_values_preserve_displaced_removed_and_updated_snapshots() {
    let source = r#"
type Entry:
    name: String
    count: Int

fn describe(value: Entry) -> String:
    value.name + ":" + value.count.show()

fn extracted(value: Option(Entry)) -> String:
    match value:
        Some(entry) -> "some:" + describe(entry)
        None -> "none"

fn bump(value: Entry) -> Entry:
    Entry(value.name + "!", value.count + 1)

fn main(console: Console):
    var d: Dict(String, Entry) = dict.new()
    console.print(extracted(dict.insert(d, "a", Entry("old", 1))))
    let retained = dict.at(d, "a")
    let replaced = dict.insert(d, "a", Entry("new", 2))
    let _ = dict.insert(d, "b", Entry("tail", 3))
    let removed = dict.remove(d, "b")
    console.print(extracted(dict.remove(d, "missing")))
    let snapshot = d
    let _ = dict.update(d, "a", Entry("unused", 90), bump)
    let _ = dict.update(d, "c", Entry("seed", 4), bump)
    console.print(describe(retained))
    console.print(extracted(replaced))
    console.print(extracted(removed))
    console.print(describe(dict.at(snapshot, "a")))
    console.print(describe(dict.at(d, "a")))
    console.print(describe(dict.at(d, "c")))
    console.print(dict.keys(snapshot).join(","))
    console.print(dict.keys(d).join(","))
    let pairs = dict.pairs(d)
    let (key, value) = pairs[0]
    console.print(key + "=" + describe(value))
    console.print(describe(dict.get_or(d, "absent", Entry("fallback", 8))))
"#;
    let expected = [
        "none", "none", "old:1", "some:old:1", "some:tail:3", "new:2",
        "new!:3", "seed!:5", "a", "a,c", "a=new!:3", "fallback:8",
    ];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
}

#[test]
fn list_string_dictionary_values_preserve_nested_owners_and_identity_results() {
    let source = r#"
fn describe(value: List(String)) -> String:
    value.length().show() + ":" + value.join("|")

fn extracted(value: Option(List(String))) -> String:
    match value:
        Some(items) -> "some:" + describe(items)
        None -> "none"

fn extend(value: List(String)) -> List(String):
    list.concat(value, ["tail"])

fn identity(value: List(String)) -> List(String):
    value

fn main(console: Console):
    var d: Dict(String, List(String)) = dict.new()
    let _ = dict.insert(d, "a", ["old", ""])
    let retained = dict.at(d, "a")
    let replaced = dict.insert(d, "a", ["new"])
    let _ = dict.insert(d, "b", [])
    let removed = dict.remove(d, "b")
    console.print(extracted(dict.remove(d, "missing")))
    let snapshot = d
    let _ = dict.update(d, "a", [], extend)
    let _ = dict.update(d, "c", [], extend)
    let _ = dict.update(d, "a", [], identity)
    console.print(describe(retained))
    console.print(extracted(replaced))
    console.print(extracted(removed))
    console.print(describe(dict.at(snapshot, "a")))
    console.print(describe(dict.at(d, "a")))
    console.print(describe(dict.at(d, "c")))
    let values = dict.values(d)
    console.print(describe(values[0]))
    console.print(dict.keys(d).join(","))
"#;
    let expected = [
        "none", "2:old|", "some:2:old|", "some:0:", "1:new",
        "2:new|tail", "1:tail", "2:new|tail", "a,c",
    ];
    assert_three_legs(source, &expected);
    assert_three_legs(&format!("mode opt\n{source}"), &expected);
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
