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
    let wasm = codegen::compile_checked_module_binary(module)
        .expect_lowered("compile RFC-0087 program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
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
        Err(witchy::ResolveStdError::Pipeline(witchy::pipeline::PipelineError::Type(error))) => {
            error.message
        }
        Err(error) => panic!("diagnostic probe failed before type checking: {error}"),
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

fn bump(var n: Int) -> Nil:
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
    bump(rows[next_index(calls)][1])

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
        "fn exchange(var left: Int, var right: Int) -> Nil:\n\
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
         fn outer(var n: Int, snapshot: Int) -> Nil:\n\
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
        "fn bump(var n: Int) -> Nil:\n\
         \x20   return\n\
         fn test(n: Int) -> Nil:\n\
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
        "fn bump(var n: Int) -> Nil:\n\
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
        "fn bump(var n: Int) -> Nil:\n\
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
fn explode(var n: Int) -> Nil:
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
         fn consume(operation: fn(var Int) -> Int) -> Nil:\n\
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

async fn seam(console: Console) -> Nil:
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

fn bump(var n: Int) -> Nil:
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
            "async fn bad(var state: Int) -> Nil:\n    return\n",
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
