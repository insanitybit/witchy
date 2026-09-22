//! RFC-0167 section 7 / row K: immutable views cross suspension.
//!
//! A view retains its own root, so an async function or a spawned task may take
//! one as a parameter; each carrier used here (`&Int` cell, `&Float` value,
//! `&String` and `&List(Int)` tokens) is an authenticated retained-root
//! representation. Independent expected output, canonical SafeWIR interpreter,
//! admitted unoptimized Wasm, zero leaks on both.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("async view source must check: {error}"))
}

fn assert_async_output(label: &str, source: &str, expected: &[&str]) {
    let checked = checked(source);
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("async view entry");
            let function = FunctionId::new(
                entry
                    .export()
                    .strip_prefix('f')
                    .expect("numeric entry export")
                    .parse()
                    .expect("numeric entry function"),
            );
            let arguments = entry
                .capabilities()
                .iter()
                .copied()
                .map(InterpreterValue::entry_capability)
                .collect();
            interpret_program(validated, function, arguments, Default::default())
                .unwrap_or_else(|error| panic!("{label} canonical SafeWIR execution: {error}"))
        },
    )
    .expect_lowered(&format!("{label} async view admits through canonical SafeWIR"));

    assert_eq!(
        oracle.output, expected,
        "{label} canonical SafeWIR matches the independent expectation",
    );
    assert_eq!(
        oracle.leaked_objects, 0,
        "{label} canonical SafeWIR releases retained roots",
    );

    let mut runtime = Runtime::batch().expect("create compiled runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    actor
        .run()
        .unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(
        actor.output(), expected,
        "{label} admitted unoptimized Wasm matches the independent expectation",
    );
    assert_eq!(
        actor.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "{label} admitted Wasm releases retained roots",
    );
}

#[test]
fn an_awaited_async_fn_reads_an_int_view_snapshot() {
    assert_async_output("int view", r#"
async fn read_later(v: &Int) -> Int:
    (*v) * 2

async fn main(console: Console):
    var n = 21
    let v = &n
    n = 5
    let r = read_later(v).await
    console.print("${r}")
"#, &["42"]);
}

#[test]
fn an_awaited_async_fn_reads_a_float_view() {
    assert_async_output("float view", r#"
async fn helper(x: &Float) -> Float:
    (*x) * 2.0

async fn main(console: Console):
    let p = 2.5
    let doubled = helper(&p).await
    console.print("${doubled}")
"#, &["5.0"]);
}

#[test]
fn a_spawned_task_reads_string_and_list_views_after_yielding() {
    assert_async_output("task views", r#"
import chan

async fn worker(console: Console, label: &String, xs: &List(Int)):
    chan.yield_now().await
    let a = (*label).length()
    let b = (*xs).length()
    console.print("${a} ${b}")

async fn main(console: Console):
    let name = "abc"
    let xs = [1, 2]
    let h = chan.spawn(worker(console, &name, &xs)).await
    console.print("spawned")
    chan.join(h).await
"#, &["spawned", "3 2"]);
}

#[test]
fn a_legacy_borrow_still_cannot_cross_an_async_boundary() {
    let error = witchy::resolve_std_only_checked(
        "async fn f(x: &'a Int) -> Int:\n    (*x)\n\nasync fn main(console: Console):\n    let n = 1\n    let r = f(&n).await\n    console.print(\"${r}\")\n",
    )
    .expect_err("a lifetime-bearing reference is retired");
    assert!(error.to_string().contains("retired"), "{error}");
}
