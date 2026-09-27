//! RFC-0167 row K: `await` nested inside a larger expression. Async lowering
//! hoists each nested `await` into a temporary bound just before its statement,
//! in evaluation order, so a `match` scrutinee, a binary operand, an `if`
//! condition, a string interpolation, call arguments, and `?` on an awaited
//! `Result`/`Option` all suspend correctly. The awaited calls print, so the
//! trace pins evaluation order, including a non-awaited operand that must run
//! before a later operand suspends. An `await` in the right operand of `&&`
//! is refused by name: hoisting it would run it when the left operand is false.
//!
//! Independent expectation, admitted unoptimized Wasm, zero-leak audit. One
//! program, one async compile.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_async_output(label: &str, source: &str, expected: &[&str]) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} type-checks: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
    .expect_lowered(&format!("{label} admits through canonical SafeWIR"));

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    vm.run().unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(vm.output(), expected, "{label} admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "{label} admitted Wasm releases all roots");
}

const NESTED: &str = r#"import chan

async fn get(console: Console, n: Int) -> Int:
    console.print("get ${n}")
    chan.yield_now().await
    n

fn stamp(console: Console, n: Int) -> Int:
    console.print("stamp ${n}")
    n

fn pair(a: Int, b: Int) -> String:
    "${a}/${b}"

async fn parse(n: Int) -> Result(Int, String):
    chan.yield_now().await
    if n > 0: Ok(n) else: Err("negative ${n}")

async fn find(n: Int) -> Option(Int):
    chan.yield_now().await
    if n > 0: Some(n * 10) else: None

async fn total(n: Int) -> Result(Int, String):
    let v = parse(n).await? + 1
    Ok(v * 2)

async fn tens(n: Int) -> Option(Int):
    Some(find(n).await? + 1)

async fn main(console: Console):
    match get(console, 2).await:
        2 -> console.print("two")
        _ -> console.print("other")
    let x = get(console, 3).await + get(console, 4).await
    console.print("${x}")
    if get(console, 1).await > 0:
        console.print("pos")
    console.print("${get(console, 5).await * 2}")
    console.print(pair(get(console, 6).await, get(console, 7).await))
    console.print("${stamp(console, 8)} ${get(console, 9).await}")
    match total(4).await:
        Ok(v) -> console.print("ok ${v}")
        Err(e) -> console.print(e)
    match total(-1).await:
        Ok(v) -> console.print("ok ${v}")
        Err(e) -> console.print(e)
    match tens(2).await:
        Some(v) -> console.print("some ${v}")
        None -> console.print("none")
    match tens(0).await:
        Some(v) -> console.print("some ${v}")
        None -> console.print("none")
"#;

const SHORT_CIRCUIT: &str = r#"
async fn get(n: Int) -> Int:
    n

async fn main(console: Console):
    let flag = false
    if flag && get(1).await > 0:
        console.print("pos")
"#;

#[test]
fn await_nested_in_expressions_suspends_in_evaluation_order() {
    // match scrutinee 2 -> "two"; 3 + 4 = 7 (3 awaited first); 1 > 0 -> "pos";
    // 5 * 2 = 10; call arguments 6 then 7; `stamp 8` runs before `get 9`
    // suspends; `parse(4).await? + 1` = 5, doubled 10; `parse(-1)` propagates
    // its Err; `find(2).await? + 1` = 21; `find(0)` propagates None.
    assert_async_output(
        "await nested in expressions",
        NESTED,
        &[
            "get 2", "two",
            "get 3", "get 4", "7",
            "get 1", "pos",
            "get 5", "10",
            "get 6", "get 7", "6/7",
            "stamp 8", "get 9", "8 9",
            "ok 10", "negative -1",
            "some 21", "none",
        ],
    );

    let error = match witchy::resolve_std_only_checked(SHORT_CIRCUIT) {
        Ok(_) => panic!("an `await` in the right operand of `&&` must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("right operand of `&&`/`||`"), "{error}");
}
