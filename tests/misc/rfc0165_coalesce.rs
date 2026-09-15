//! Lazy defaults preserve ordinary var writebacks and owned result snapshots.

#[test]
fn coalesce_evaluates_left_once_and_only_the_selected_defaults() {
    assert_both_modes(r#"
fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn choose(var log: List(Int), value: Int, present: Bool) -> Option(Int):
    log.push(value)
    if present:
        Some(value)
    else:
        None

fn forbidden(var calls: Int) -> Int:
    calls = calls + 1
    fail("unselected coalesce default executed")
    0

fn main(console: Console):
    var log: List(Int) = []
    let first = choose(log, 1, false) ?? mark(log, 2)
    let second = choose(log, 3, true) ?? mark(log, 4)
    let missing: Option(Int) = None
    let nested = missing ?? (missing ?? mark(log, 5))
    var calls = 0
    let kept = Some(7) ?? forbidden(calls)
    let success: Result(Int, String) = Ok(9)
    let result = success ?? forbidden(calls)
    console.print("${first} ${second} ${nested}")
    console.print("${log}")
    console.print("${kept} ${result} ${calls}")
"#, &["2 3 5", "[1, 2, 3, 5]", "7 9 0"]);
}

#[test]
fn coalesce_reference_defaults_keep_writebacks_and_result_snapshots_distinct() {
    assert_both_modes(r#"
fn append(var words: List(String), var calls: Int) -> List(String):
    calls = calls + 1
    words.push("new")
    words

fn main(console: Console):
    var words = ["old"]
    var calls = 0
    let snapshot = words
    let missing: Option(List(String)) = None
    let selected = missing ?? append(words, calls)
    let kept = Some(words) ?? append(words, calls)
    let bound_default = missing ?? words
    words.push("later")
    console.print("${snapshot}")
    console.print("${selected}")
    console.print("${kept}")
    console.print("${bound_default}")
    console.print("${words}")
    console.print("${calls}")
"#, &["[old]", "[old, new]", "[old, new]", "[old, new]", "[old, new, later]", "1"]);
}

#[test]
fn coalesce_joins_static_strings_and_owned_records_without_reading_error_payloads() {
    assert_both_modes(r#"
type Box:
    values: List(String)

fn replace(var value: Box) -> Box:
    value = Box(["new"])
    value

fn text(var calls: Int) -> String:
    calls = calls + 1
    "made" + "!"

fn main(console: Console):
    var value = Box(["old"])
    let snapshot = value
    let missing: Option(Box) = None
    let selected = missing ?? replace(value)
    console.print("${snapshot.values[0]} ${selected.values[0]} ${value.values[0]}")
    var calls = 0
    let absent: Option(String) = None
    let failed: Result(String, String) = Err("not a success payload")
    let produced = absent ?? text(calls)
    let kept = Some("kept") ?? text(calls)
    let empty = failed ?? ""
    console.print(produced)
    console.print(kept)
    console.print(empty)
    console.print("${calls}")
"#, &["old new new", "made!", "kept", "", "1"]);
}

#[test]
fn conditional_reference_results_preserve_unmodified_bindings_and_snapshots() {
    assert_both_modes(r#"
fn empty(length: Int) -> Bool:
    length == 0

fn main(console: Console):
    var words = ["old"]
    let snapshot = words
    let flag = empty(words.length())
    let selected = if flag:
        ["fallback"]
    else:
        words
    words.push("new")
    let picked = if flag:
        selected
    else:
        words
    words.push("last")
    console.print("${snapshot}")
    console.print("${selected}")
    console.print("${picked}")
    console.print("${words}")
"#, &["[old]", "[old]", "[old, new]", "[old, new, last]"]);
}

#[test]
fn nested_match_string_snapshot_lanes_are_authenticated_before_outer_phi() {
    assert_both_modes(r##"
fn choose(line: String, outer: Bool, inner: Bool) -> String:
    if outer:
        line
    else:
        match inner:
            true -> line + "!"
            false -> line

fn header(line: String) -> String:
    let close = line.index_of("]") ?? -1
    if close < 0:
        line
    else:
        let rest = line.substring(close + 1, line.char_count())
        match rest.index_of("#"):
            Some(h) -> line.substring(0, close + 1 + h).trim()
            None -> line

fn main(console: Console):
    let snapshot = "old" + " value"
    console.print(choose(snapshot, true, true))
    console.print(choose(snapshot, false, true))
    console.print(choose(snapshot, false, false))
    console.print(snapshot)
    console.print(header("plain"))
    console.print(header("[section]"))
    console.print(header("[section]  # comment"))
"##, &["old value", "old value!", "old value", "old value", "plain", "[section]", "[section]"]);
}

fn assert_both_modes(source: &str, expected: &[&str]) {
    for source in [source.to_owned(), format!("mode opt\n{source}")] {
        let checked = witchy::resolve_std_only_checked(&source).expect("coalesce source checks");
        let source_output = witchy::interpreter::run_module_args(
            checked.module().clone(), ".", Vec::new(), Vec::new(),
        ).expect("source interpreter executes coalesce");
        assert_eq!(source_output, expected, "source expectation");
        let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
            &checked, |validated, artifact| {
                use witchy_wir::canonical_interpreter::{InterpreterValue, interpret_entry_program};
                let entry = artifact.entry().expect("source entry");
                let function = witchy_wir::canonical_safewir::FunctionId::new(
                    entry.export().strip_prefix('f').expect("canonical export")
                        .parse().expect("canonical function id"),
                );
                let arguments = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let run = interpret_entry_program(validated, function, arguments, &[], Default::default())
                    .expect("canonical interpreter executes coalesce");
                assert_eq!(run.output, expected, "canonical expectation");
                assert_eq!(run.leaked_objects, 0, "canonical ownership balance");
            },
        ).expect_lowered("coalesce admits through the canonical route");
        let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted coalesce instance");
        vm.run().expect("Wasm executes coalesce");
        assert_eq!(vm.output(), expected, "Wasm expectation");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    }
}
