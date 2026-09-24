//! RFC-0087 uniform `var` write-back conformance matrix.
//!
//! Keep this coverage separate from the broad example-test module: these are
//! release criteria for one language convention, and every runtime case is
//! asserted against both the interpreter oracle and compiled WebAssembly.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

fn checked(source: &str) -> witchy::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source).expect("check RFC-0087 program")
}

fn compiled_result(module: &witchy::pipeline::CheckedModule) -> Result<Vec<String>, String> {
    let artifact = codegen::compile_checked_module_artifact(module)
        .expect_lowered("compile RFC-0087 program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0087 program");
    actor
        .run()
        .map_err(|error| error.root_cause().to_string())?;
    Ok(actor.output())
}

fn compiled_output(module: &witchy::pipeline::CheckedModule) -> Vec<String> {
    compiled_result(module).expect("run compiled RFC-0087 program")
}

fn assert_both_backends(source: &str, expected: &[&str]) {
    let module = checked(source);
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(
        interpreter::run_checked_module(&module, ".", Vec::new()).expect("interpret"),
        expected,
        "interpreter output",
    );
    assert_eq!(compiled_output(&module), expected, "compiled output");
}

fn type_error(source: &str) -> String {
    match witchy::resolve_std_only_checked(source) {
        Ok(_) => panic!("diagnostic probe must be rejected"),
        Err(witchy::ResolveStdError::Pipeline(witchy::pipeline::PipelineError::Source(
            error,
        ))) => error.message,
        Err(witchy::ResolveStdError::Pipeline(witchy::pipeline::PipelineError::Type(error))) => {
            error.message
        }
        Err(error) => panic!("diagnostic probe failed outside semantic checking: {error}"),
    }
}

fn assert_source_facing(message: &str) {
    for artifact in [
        "CallStoreMulti",
        "writeback_places",
        "__cap",
        "__witchy",
        "multi-result",
    ] {
        assert!(
            !message.contains(artifact),
            "compiler-only artifact `{artifact}` leaked into diagnostic: {message}",
        );
    }
}

fn assert_pop_three_legs(source: &str, expected: &[&str]) {
    use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;

    let module = checked(source);
    assert_eq!(interpreter::run_checked_module(&module, ".", Vec::new()).expect("source pop"), expected);
    let artifact = codegen::compile_checked_module_artifact(&module).expect_lowered("pop admits");
    let validated = codegen::validate_checked_module_for_oracle(&module).expect("independent pop validation");
    let entry = artifact.entry().expect("pop entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_entry_program(&validated, function, capabilities, &[], InterpreterConfig::default())
        .expect("canonical pop oracle");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0, "pop oracle owns and releases every payload");
    let mut runtime = Runtime::batch().expect("pop runtime");
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, print_int: true, quiet: true, ..Default::default()
    }, 256).expect("admitted pop instance");
    vm.run().expect("admitted pop execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("pop heap audit"), Some(0));
}

#[test]
fn list_pop_scalar_and_string_payloads_preserve_snapshots_on_three_legs() {
    let source = r#"
import list

fn main(console: Console):
    var numbers = [10, 20]
    let original = numbers
    let first = numbers.pop()
    let second = list.pop(numbers)
    let empty = numbers.pop()
    console.print("${first ?? -1} ${second ?? -1} ${empty ?? -1} ${numbers.length()} ${original}")

    var words = ["root", "tail"]
    let old_words = words
    let tail = words.pop()
    let root = list.pop(words)
    let none = words.pop()
    words.push("new")
    console.print("${tail ?? "missing"} ${root ?? "missing"} ${none ?? "missing"} ${words[0]}")
    console.print("${old_words[0]} ${old_words[1]}")
    let _ = words.pop()
    let _ = words.pop()
    console.print("${words.length()}")
"#;
    assert_pop_three_legs(source, &["20 10 -1 0 [10, 20]", "tail root missing new", "root tail", "0"]);
}

#[test]
fn list_pop_projected_coordinates_and_nested_payloads_on_three_legs() {
    let source = r#"
type State:
    rows: List(List(String))

fn grow(var words: List(String)) -> ():
    words.push("grown")
    return

fn coordinate(var calls: Int) -> Int:
    calls = calls + 1
    0

fn main(console: Console):
    var calls = 0
    var state = State([["a", "b"], ["sibling"]])
    let before = state
    let item = state.rows[coordinate(calls)].pop()
    console.print("${calls} ${item ?? "missing"} ${state.rows[0].length()} ${state.rows[1][0]}")
    console.print("${before.rows[0][0]} ${before.rows[0][1]}")
    var rows = [["kept"], ["last"]]
    let original = rows
    let removed = rows.pop() ?? []
    grow(rows[0])
    console.print("${removed[0]} ${rows[0].length()} ${original[0].length()} ${original[1][0]}")
    var total = 0
    var numbers = [1, 2, 3]
    for ignored in [0, 1, 2, 3]:
        total = total + (numbers.pop() ?? 0)
    console.print("${total} ${numbers.length()}")
"#;
    assert_pop_three_legs(source, &["1 b 1 sibling", "a b", "last 2 1 last", "6 0"]);
}

#[test]
fn exclusivity_and_place_diagnostics_are_exact_and_source_facing() {
    let duplicate = type_error(
        "fn exchange(var left: Int, var right: Int) -> ():\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   var n = 1\n\
         \x20   exchange(n, n)\n",
    );
    assert_eq!(
        duplicate,
        "`main`, line 5: arguments 1 and 2 to `main.exchange` are overlapping `var` places rooted in `n`",
    );
    assert_source_facing(&duplicate);

    let reservation = type_error(
        "fn inner(var n: Int) -> Int:\n\
         \x20   n = n + 1\n\
         \x20   n\n\
         fn outer(var n: Int, snapshot: Int) -> ():\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   var n = 0\n\
         \x20   outer(n, inner(n))\n",
    );
    assert_eq!(
        reservation,
        "`main`, line 8: argument 1 to `main.outer` reserves `var` place rooted in `n` until the call returns, but later argument 2 writes back to an overlapping place through argument 1 of `main.inner`; written evaluation order keeps the earlier reservation live",
    );
    assert_source_facing(&reservation);

    let immutable = type_error(
        "fn bump(var n: Int) -> ():\n\
         \x20   return\n\
         fn test(n: Int) -> ():\n\
         \x20   bump(n)\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   test(0)\n",
    );
    assert_eq!(
        immutable,
        "`main.test`, line 4: argument 1 to `var` parameter `n` of `main.bump` has immutable root `n`; root `n` must be a mutable `var` for write-back",
    );
    assert_source_facing(&immutable);

    let temporary = type_error(
        "fn bump(var n: Int) -> ():\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   bump(1)\n",
    );
    assert_eq!(
        temporary,
        "`main`, line 4: argument 1 to `var` parameter `n` of `main.bump` must be a mutable place; bind the expression to a mutable `var` before the call",
    );
    assert_source_facing(&temporary);

    let moved = type_error(
        "fn bump(var n: Int) -> ():\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   var n = 0\n\
         \x20   bump(move n)\n",
    );
    assert_eq!(
        moved,
        "`main`, line 5: argument 1 to `var` parameter `n` of `main.bump` uses `move`; write-back requires a live mutable place in the caller",
    );
    assert_source_facing(&moved);

    let convention_mismatch = type_error(
        "trait Advance:\n\
         \x20   fn advance(var self, by: Int) -> Int\n\
         type Counter:\n\
         \x20   value: Int\n\
         impl Advance for Counter:\n\
         \x20   fn advance(self, by: Int) -> Int:\n\
         \x20       self.value + by\n",
    );
    assert_eq!(
        convention_mismatch,
        "in impl declaration `impl Advance for Counter`, method `advance` parameter 1 `self` is plain, but trait declaration `Advance.advance` parameter 1 `self` is `var`; Variable write-back parameter conventions must match exactly",
    );
    assert_source_facing(&convention_mismatch);
}

#[test]
fn comprehensions_closures_and_async_segments_preserve_writeback() {
    let source = r#"
import chan
import list

fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn apply(operation: fn(var Int) -> Int, var value: Int) -> Int:
    operation(value)

async fn seam(console: Console) -> ():
    var state = 10
    let operation: fn(var Int) -> Int = fn(var n: Int):
        n = n + 1
        n
    let before = apply(operation, state)
    chan.yield_now().await
    let after = apply(operation, state)
    console.print("${before} ${after} ${state}")

async fn main(console: Console):
    var log: List(Int) = []
    let values = [mark(log, n * 10) for n in [1, 2] if mark(log, n) > 0]
    console.print("${log} ${values}")
    seam(console).await
"#;
    assert_both_backends(source, &["[1, 10, 2, 20] [10, 20]", "11 12 12"]);
}

#[test]
fn async_and_generator_var_parameters_reject_at_the_shared_parser() {
    for (source, expected) in [
        (
            "async fn bad(var state: Int) -> ():\n    return\n",
            "parse error at 1:30: a async function cannot take `var` parameter `state`: suspension may outlive the caller's write-back place; use an ordinary parameter or mutate a local until the lifetime model admits suspended `var` access",
        ),
        (
            "gen fn bad(var state: Int) -> Iter(Int):\n    yield state\n",
            "parse error at 1:28: a generator function cannot take `var` parameter `state`: suspension may outlive the caller's write-back place; use an ordinary parameter or mutate a local until the lifetime model admits suspended `var` access",
        ),
    ] {
        let error = witchy::parser::parse_module(source)
            .expect_err("suspending var parameter must reject")
            .to_string();
        assert_eq!(error, expected);
        assert_source_facing(&error);
    }
}

#[test]
fn assignment_provenance_spreads_follow_written_order_on_three_legs() {
    // spec/language.md: overrides precede the trailing ..base, and written
    // field order is independent of declaration order. These are expected
    // effects, not a majority vote between the three execution backends.
    let source = r#"
type Pair:
    a: Int
    b: Int

type Holder:
    saved: List(String)

fn reset(var words: List(String)) -> Holder:
    words = ["new"]
    Holder(["base"])

fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn base(var log: List(Int)) -> Pair:
    log.push(3)
    Pair(10, 20)

fn replace(var pair: Pair) -> Int:
    pair.b = 9
    7

fn main(console: Console):
    var log: List(Int) = []
    let named = Pair(b: mark(log, 2), a: mark(log, 1), ..base(log))
    console.print("${named.a} ${named.b} ${log}")
    log = []
    let anonymous: Pair = .{b: mark(log, 2), a: mark(log, 1), ..base(log)}
    console.print("${anonymous.a} ${anonymous.b} ${log}")
    var explicit = Pair(1, 2)
    explicit = .{a: replace(explicit), ..explicit}
    console.print("${explicit.a} ${explicit.b}")
    var projected = Pair(1, 2)
    projected.a = replace(projected)
    console.print("${projected.a} ${projected.b}")
    var words = ["old"]
    let held = Holder(saved: words, ..reset(words))
    console.print("${held.saved[0]} ${words[0]}")
"#;
    assert_pop_three_legs(source, &["1 2 [2, 1, 3]", "1 2 [2, 1, 3]", "7 9", "7 9", "old new"]);
}

#[test]
fn assignment_provenance_nested_rhs_repeats_its_own_coordinate_on_three_legs() {
    let source = r#"
type Pair:
    a: Int
    b: Int

type State:
    rows: List(Pair)

fn coordinate(var calls: Int) -> Int:
    calls = calls + 1
    0

fn replace(var state: State, var calls: Int) -> Int:
    calls = calls * 10 + 3
    state.rows[0].b = 9
    7

fn main(console: Console):
    var calls = 0
    var state = State([Pair(1, 2)])
    let before = state
    state.rows[coordinate(calls)] = .{a: replace(state, calls), ..state.rows[coordinate(calls)]}
    console.print("${state.rows[0].a} ${state.rows[0].b} ${calls}")
    console.print("${before.rows[0].a} ${before.rows[0].b}")
    calls = 0
    state = State([Pair(1, 2)])
    state.rows[coordinate(calls)].a = replace(state, calls)
    console.print("${state.rows[0].a} ${state.rows[0].b} ${calls}")
"#;
    assert_pop_three_legs(source, &["7 9 14", "1 2", "7 9 13"]);
}

#[test]
fn assignment_provenance_dictionary_captures_key_and_reloads_current_root_on_three_legs() {
    let source = r#"
import dict

type Box:
    values: Dict(String, Int)
    marker: Int

fn coordinate(var calls: Int, key: String) -> String:
    calls = calls + 1
    key

fn replace(var values: Dict(String, Int), var key: String) -> Int:
    values["sibling"] = 9
    key = "changed"
    7

fn replace_box(var box: Box) -> Int:
    box.values["sibling"] = 11
    box.marker = 6
    8

fn main(console: Console):
    var values: Dict(String, Int) = dict.new()
    values["original"] = 2
    let snapshot = values
    var key = "original"
    var calls = 0
    values[coordinate(calls, key)] = replace(values, key)
    console.print("${calls} ${key} ${values.get_or("original", 0)} ${values.get_or("sibling", 0)} ${snapshot.get_or("original", 0)}")
    var box = Box(values, 0)
    box.values[coordinate(calls, "original")] = replace_box(box)
    console.print("${calls} ${box.marker} ${box.values.get_or("original", 0)} ${box.values.get_or("sibling", 0)}")
    console.print("${values.get_or("original", 0)} ${values.get_or("sibling", 0)}")
"#;
    assert_pop_three_legs(source, &["1 changed 7 9 2", "2 6 8 11", "7 9"]);
}

