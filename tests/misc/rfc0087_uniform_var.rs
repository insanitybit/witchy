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

fn runtime_errors(source: &str) -> (String, String) {
    let module = checked(source);
    let interpreted = interpreter::run_checked_module(&module, ".", Vec::new())
        .expect_err("interpreter must abort")
        .message;
    let compiled = compiled_result(&module).expect_err("compiled backend must abort");
    (interpreted, compiled)
}

#[test]
fn projected_var_final_calls_preserve_the_callers_writeback_lane() {
    let source = r#"
fn bump(var n: Int) -> Int:
    n = n + 10
    n * 2

fn implicit(var n: Int) -> Int:
    var rows = [n]
    bump(rows[0])

fn explicit(var n: Int) -> Int:
    var rows = [n]
    return bump(rows[0])

fn indirect(operation: fn(var Int) -> Int, var n: Int) -> Int:
    var rows = [n]
    operation(rows[0])

fn indirect_return(operation: fn(var Int) -> Int, var n: Int) -> Int:
    var rows = [n]
    return operation(rows[0])

fn forwarded(var n: Int) -> Int:
    bump(n)

fn main(console: Console):
    var n = 1
    let a = implicit(n)
    let b = explicit(n)
    let operation: fn(var Int) -> Int = bump
    let c = indirect(operation, n)
    let d = indirect_return(operation, n)
    let e = forwarded(n)
    console.print("${a} ${b} ${c} ${d} ${e} ${n}")
"#;
    assert_both_backends(source, &["22 22 22 22 22 11"]);
}

#[test]
fn owned_binding_snapshots_survive_source_list_growth() {
    let source = r#"
fn main(console: Console):
    var numbers = [1]
    let before = numbers
    numbers.push(2)
    var words = ["root"]
    let old_words = words
    words.push("tail")
    console.print("${before.length()} ${before[0]} ${numbers.length()} ${numbers[1]}")
    console.print("${old_words.length()} ${old_words[0]} ${words.length()} ${words[1]}")
"#;
    assert_both_backends(source, &["1 1 2 2", "1 root 2 tail"]);
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
fn simple_nested_method_and_free_calls_write_back_on_both_backends() {
    let source = r#"
import list

type State:
    value: Int
    rows: List(List(Int))

fn bump(var n: Int, by: Int) -> Int:
    n = n + by
    n * 10

fn main(console: Console):
    var state = State(1, [[2, 3]])
    let plain = bump(state.value, 4)
    let nested = bump(state.rows[0][1], 5)

    var method_values = [1, 2]
    let method_result = method_values.pop()
    var free_values = [1, 2]
    let free_result = list.pop(free_values)

    console.print("${state.value} ${plain}")
    console.print("${state.rows} ${nested}")
    console.print("${method_values} ${method_result ?? -1}")
    console.print("${free_values} ${free_result ?? -1}")
"#;
    assert_both_backends(
        source,
        &["5 50", "[[2, 8]] 80", "[1] 2", "[1] 2"],
    );
}

#[test]
fn list_assignment_preserves_coordinate_order_and_post_rhs_siblings_on_three_legs() {
    let source = r#"
type State:
    rows: List(List(Int))
    flag: Int

fn coordinate(var log: List(Int), marker: Int) -> Int:
    log.push(marker)
    0

fn add_row(var rows: List(List(Int))) -> ():
    rows.push([30])
    return

fn replacement(var state: State, var log: List(Int)) -> Int:
    log.push(3)
    add_row(state.rows)
    state.flag = 7
    9

fn main(console: Console):
    var state = State([[1]], 0)
    let before = state
    var log: List(Int) = []
    state.rows[coordinate(log, 1)][coordinate(log, 2)] = replacement(state, log)
    console.print("${state.rows} ${state.flag} ${log}")
    console.print("${before.rows} ${before.flag}")
"#;
    assert_pop_three_legs(source, &["[[9], [30]] 7 [1, 2, 3]", "[[1]] 0"]);
}

#[test]
fn dict_place_assignment_uses_language_key_equality() {
    let source = r#"
import cmp
import dict

type Key derive(Eq):
    id: Int
    cache: Int

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        self.id == other.id

fn main(console: Console):
    var values: Dict(Key, Int) = dict.new()
    values[Key(1, 10)] = 1
    values[Key(1, 20)] = 2
    console.print("${values.length()} ${values.get_or(Key(1, 30), 0)}")
"#;
    assert_both_backends(source, &["1 2"]);
}

#[test]
fn source_order_coordinates_and_same_root_snapshots_are_stable() {
    let source = r#"
import list

fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn next_index(var calls: Int) -> Int:
    calls = calls + 1
    0

fn bump(var n: Int) -> ():
    n = n + 10
    return

fn append_snapshot_len(var values: List(Int), snapshot: List(Int)) -> Int:
    values.push(snapshot.length())
    snapshot.length()

fn main(console: Console):
    var log: List(Int) = []
    let ordered = (mark(log, 1), mark(log, 2), [mark(log, 3), mark(log, 4)])

    var calls = 0
    var rows = [[5, 6]]
    let _ = bump(rows[next_index(calls)][1])

    var values = [7]
    let old_len = append_snapshot_len(values, values)

    console.print("${log} ${ordered}")
    console.print("${calls} ${rows}")
    console.print("${values} ${old_len}")
"#;
    assert_both_backends(
        source,
        &[
            "[1, 2, 3, 4] (1, 2, [3, 4])",
            "1 [[5, 16]]",
            "[7, 1] 1",
        ],
    );
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
fn projected_var_siblings_snapshots_and_nested_argument_calls_are_preserved() {
    let source = r#"
type State:
    left: List(Int)
    right: List(Int)

fn change(var left: Int, var right: Int) -> ():
    left = left + 10
    right = right + 100
    return

fn bump(var value: Int) -> Int:
    value = value + 1
    value

fn coordinate(var calls: Int) -> Int:
    let _ = bump(calls)
    0

fn append(var values: List(Int), snapshot: List(Int)) -> ():
    values.push(snapshot.length())
    return

fn apply(operation: fn(var Int) -> Int, var value: Int) -> Int:
    operation(value)

fn main(console: Console):
    var state = State([1], [2])
    let snapshot = state
    change(state.left[0], state.right[0])
    var calls = 0
    change(state.left[coordinate(calls)], state.right[coordinate(calls)])
    append(state.left, state.left)
    let operation: fn(var Int) -> Int = bump
    let result = operation(state.right[0])
    let forwarded = apply(operation, state.left[0])
    for i in [0, 1]:
        let _ = bump(state.left[i])
    console.print("${state.left} ${state.right} ${calls} ${result} ${forwarded}")
    console.print("${snapshot.left} ${snapshot.right}")
"#;
    assert_both_backends(source, &["[23, 2] [203] 2 203 22", "[1] [2]"]);
}

#[test]
fn every_structured_return_commits_the_complete_multi_var_envelope() {
    let source = r#"
fn stop() -> Result(Int, String):
    Err("stop")

fn via_try(var left: Int, var right: Int) -> Result(Int, String):
    left = left + 100
    right = right + 10
    let value = stop()?
    Ok(value)

fn via_return(var left: Int, var right: Int) -> Result(Int, String):
    left = left + 100
    right = right + 10
    return Err("stop")

fn via_tail(var left: Int, var right: Int) -> Result(Int, String):
    left = left + 100
    right = right + 10
    Err("stop")

fn success(var left: Int, var right: Int) -> Int:
    left = left + 100
    right = right + 10
    left + right

fn main(console: Console):
    var a = 1
    var b = 2
    var c = 1
    var d = 2
    var e = 1
    var f = 2
    var g = 1
    var h = 2
    let r1 = via_try(a, b) ?? -1
    let r2 = via_return(c, d) ?? -1
    let r3 = via_tail(e, f) ?? -1
    let r4 = success(g, h)
    console.print("${a} ${b} ${r1}")
    console.print("${c} ${d} ${r2}")
    console.print("${e} ${f} ${r3}")
    console.print("${g} ${h} ${r4}")
"#;
    assert_both_backends(
        source,
        &["101 12 -1", "101 12 -1", "101 12 -1", "101 12 113"],
    );
}

#[test]
fn traps_are_terminal_and_do_not_create_a_backend_specific_writeback_rule() {
    let source = r#"
fn explode(var n: Int) -> ():
    n = 99
    fail("boom after local mutation")

fn main(console: Console):
    var n = 1
    explode(n)
    console.print("unreachable ${n}")
"#;
    let (interpreted, compiled) = runtime_errors(source);
    assert_eq!(compiled, format!("runtime error: {interpreted}"));
    assert!(interpreted.ends_with("boom after local mutation"), "{interpreted}");
    assert_source_facing(&interpreted);
}

#[test]
fn canonical_abort_evaluates_nested_message_once_and_keeps_source_site() {
    use witchy_wir::canonical_interpreter::{interpret_program, InterpretError, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;
    let sources = [
        (r#"fn message(console: Console, var calls: Int) -> String:
    calls = calls + 1
    console.print("message")
    "boom ${calls}"

fn explode(console: Console, var n: Int):
    n = 99
    fail(message(console, n))

fn main(console: Console):
    var n = 1
    console.print("before")
    explode(console, n)
    console.print("unreachable ${n}")
"#, "runtime error: `main.explode`, line 8: boom 100"),
        (r#"fn message(console: Console, stop: Bool) -> String:
    console.print("message")
    if stop:
        fail("inner")
    "unused"

fn main(console: Console):
    console.print("before")
    fail(message(console, true))
"#, "runtime error: `main.message`, line 4: inner"),
    ];
    for (source, expected) in sources {
        let module = checked(source);
        let source_error = interpreter::run_checked_module(&module, ".", Vec::new()).expect_err("source abort");
        assert_eq!(format!("runtime error: {}", source_error.message), expected);
        let artifact = codegen::compile_checked_module_artifact(&module).expect_lowered("canonical diagnostic admission");
        let validated = codegen::validate_checked_module_for_oracle(&module).expect("independent oracle validation");
        let entry = artifact.entry().expect("sealed entry");
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let oracle = interpret_program(&validated, function, arguments, Default::default()).expect_err("oracle abort");
        assert_eq!(oracle, InterpretError::DiagnosticAbort {
            message: expected.to_owned(), output: vec!["before".into(), "message".into()],
        });
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted diagnostic launch");
        assert_eq!(vm.run().expect_err("compiled abort").root_cause().to_string(), expected);
        assert_eq!(vm.output(), ["before", "message"]);
        assert_eq!(vm.run().expect_err("trap is terminal").root_cause().to_string(), "an aborted Witchy VM cannot be run again");
        assert_eq!(vm.output(), ["before", "message"]);
    }
}

#[test]
fn stale_assignment_projection_uses_ordinary_bounds_behavior() {
    let source = r#"
fn shrink(var values: List(Int)) -> Int:
    let _ = values.pop()
    9

fn main(console: Console):
    var values = [1]
    values[0] = shrink(values)
    console.print("unreachable")
"#;
    let module = checked(source);
    let interpreted = interpreter::run_checked_module(&module, ".", Vec::new())
        .map_err(|error| error.message);
    let compiled = compiled_result(&module);
    assert!(
        interpreted.is_err() && compiled.is_err(),
        "stale captured projection must fail through the ordinary bounds rule; \
         interpreter={interpreted:?}, compiled={compiled:?}",
    );
    let interpreted = interpreted.unwrap_err();
    let compiled = compiled.unwrap_err();
    assert_eq!(compiled, format!("runtime error: {interpreted}"));
    assert!(
        interpreted.contains("index 0") && interpreted.contains("length 0"),
        "stale captured projection must report the invalid current coordinate: {interpreted}",
    );
    assert_source_facing(&interpreted);

    let nested = r#"
fn extend(var values: List(List(Int))) -> Int:
    values.push([30])
    9

fn main(console: Console):
    var values = [[1]]
    values[0][0] = extend(values)
    console.print("${values}")
"#;
    assert_both_backends(nested, &["[[9], [30]]"]);
}

#[test]
fn canonical_bounds_diagnostics_preserve_current_root_effects_and_owned_snapshots() {
    use witchy_wir::canonical_interpreter::{interpret_program, InterpretError, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;
    let template = r#"fn coordinate(console: Console, var calls: Int) -> Int:
    calls = calls + 1
    console.print("index ${calls}")
    return INDEX

fn replacement(console: Console, var values: List(String), shrink: Bool) -> String:
    console.print("rhs")
    if shrink:
        let _ = values.pop()
    "new"

fn main(console: Console):
    var calls = 0
    var values = ["old" + "!"]
    let snapshot = values
    values[coordinate(console, calls)] = replacement(console, values, SHRINK)
    console.print(values[0])
    console.print(snapshot[0])
"#;
    for (index, shrink, length) in [(0_i64, true, 0), (-1, false, 1), (4294967296, false, 1), (0, false, 1)] {
        let source = template.replace("INDEX", &index.to_string()).replace("SHRINK", if shrink { "true" } else { "false" });
        let checked = checked(&source);
        let traps = index != 0 || shrink;
        let expected = format!("runtime error: `main`, line 16: list index {index} out of bounds (length {length})");
        let source_run = interpreter::run_checked_module(&checked, ".", Vec::new());
        let output = ["index 1", "rhs", "new", "old!"].map(str::to_owned).to_vec();
        if traps {
            assert_eq!(format!("runtime error: {}", source_run.unwrap_err().message), expected);
        } else {
            assert_eq!(source_run.unwrap(), output);
        }
        let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("bounds diagnostic admission");
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("independent bounds validation");
        let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let oracle = interpret_program(&validated, function, arguments, Default::default());
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256).unwrap();
        let wasm = vm.run();
        if traps {
            assert_eq!(oracle.unwrap_err(), InterpretError::DiagnosticAbort {
                message: expected.clone(), output: vec!["index 1".into(), "rhs".into()],
            });
            assert_eq!(wasm.unwrap_err().root_cause().to_string(), expected);
            assert_eq!(vm.output(), ["index 1", "rhs"]);
            assert_eq!(vm.run().unwrap_err().root_cause().to_string(), "an aborted Witchy VM cannot be run again");
            assert_eq!(vm.output(), ["index 1", "rhs"]);
        } else {
            let oracle = oracle.unwrap();
            assert_eq!(oracle.output, output);
            assert_eq!(oracle.leaked_objects, 0);
            wasm.unwrap();
            assert_eq!(vm.output(), output);
            assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        }
    }
}

#[test]
fn function_value_conventions_are_identity_and_preserve_writeback() {
    let source = r#"
fn bump(var n: Int) -> Int:
    n = n + 2
    n * 10

fn apply(operation: fn(var Int) -> Int, var n: Int) -> Int:
    operation(n)

fn main(console: Console):
    var first = 1
    let named: fn(var Int) -> Int = bump
    let a = apply(named, first)

    var second = 3
    let closure: fn(var Int) -> Int = fn(var n: Int):
        n = n + 4
        n * 100
    let b = apply(closure, second)
    console.print("${first} ${a}")
    console.print("${second} ${b}")
"#;
    assert_both_backends(source, &["3 30", "7 700"]);

    let mismatch = type_error(
        "fn pure(n: Int) -> Int:\n\
         \x20   n\n\
         fn consume(operation: fn(var Int) -> Int) -> ():\n\
         \x20   return\n\
         fn main(console: Console):\n\
         \x20   consume(pure)\n",
    );
    assert!(
        mismatch.contains("fn(var Int) -> Int")
            && mismatch.contains("fn(Int) -> Int"),
        "function convention mismatch must render both source types: {mismatch}",
    );
    assert_source_facing(&mismatch);
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
fn generator_locals_may_use_synchronous_var_calls_between_yields() {
    let source = r#"
import iter

fn bump(var n: Int) -> ():
    n = n + 1
    return

gen fn counts() -> Iter(Int):
    var n = 0
    bump(n)
    yield n
    bump(n)
    yield n

fn main(console: Console):
    let values: List(Int) = iter.collect(counts())
    console.print("${values}")
"#;
    assert_both_backends(source, &["[1, 2]"]);
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
fn assignment_provenance_ordinary_call_retains_its_earlier_snapshot_on_three_legs() {
    let source = r#"
type Pair:
    a: Int
    b: Int

fn replace(var pair: Pair) -> Int:
    pair.b = 9
    7

fn earlier(snapshot: Pair, ignored: Int) -> Pair:
    snapshot

fn main(console: Console):
    var pair = Pair(1, 2)
    pair = earlier(pair, replace(pair))
    console.print("${pair.a} ${pair.b}")
    var values: Dict(Int, Int) = dict.new()
    values[1] = 2
    console.print("${values.get_or(1, 0)}")
"#;
    assert_pop_three_legs(source, &["1 2", "2"]);
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

#[test]
fn list_pop_loop_writebacks_preserve_unique_and_shared_lanes_on_three_legs() {
    let source = r#"
fn main(console: Console):
    var words = ["a", "b"]
    var rendered = ""
    for ignored in [0, 1, 2]:
        rendered = rendered + (words.pop() ?? "_")
    console.print("${rendered}:${words.length()}")
    console.print("${words.pop() ?? "_"}:${words.length()}")

    var numbers = [1, 2]
    let saved = numbers
    var total = 0
    while numbers.length() > 0:
        total = total + (numbers.pop() ?? 0)
    console.print("${total}:${numbers.length()}:${saved[0]}:${saved[1]}")
"#;
    assert_pop_three_legs(source, &["ba_:0", "_:0", "3:0:1:2"]);
}
