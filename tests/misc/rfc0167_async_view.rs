//! RFC-0167 section 7 / row K: immutable views cross suspension.
//!
//! A view retains its own root, so an async function or a spawned task may take
//! one as a parameter; each carrier used here (`&Int` cell, `&Float` value,
//! `&String` and `&List(Int)` tokens) is an authenticated retained-root
//! representation. Independent expected output, admitted unoptimized Wasm, zero leaks on both.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("async view source must check: {error}"))
}

fn assert_async_output(label: &str, source: &str, expected: &[&str]) {
    let checked = checked(source);
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
    .expect_lowered(&format!("{label} async view admits through canonical SafeWIR"));

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

/// One async program, one compile: an awaited `&Int` snapshot, an awaited
/// `&Float` view, and a spawned task reading `&String` and `&List(Int)` views
/// after yielding.
#[test]
fn awaited_calls_and_spawned_tasks_read_views() {
    // `v` is taken while n = 21, then n = 5: the view reads 21, doubled 42.
    // 2.5 doubled is 5.0. The worker prints after main's "spawned": "abc"
    // and [1, 2] have lengths 3 and 2.
    assert_async_output("async views", r#"
import chan

async fn read_later(v: &Int) -> Int:
    (*v) * 2

async fn helper(x: &Float) -> Float:
    (*x) * 2.0

async fn worker(console: Console, label: &String, xs: &List(Int)):
    chan.yield_now().await
    let a = (*label).length()
    let b = (*xs).length()
    console.print("${a} ${b}")

async fn main(console: Console):
    var n = 21
    let v = &n
    n = 5
    let r = read_later(v).await
    console.print("${r}")
    let p = 2.5
    let doubled = helper(&p).await
    console.print("${doubled}")
    let name = "abc"
    let xs = [1, 2]
    let h = chan.spawn(worker(console, &name, &xs)).await
    console.print("spawned")
    chan.join(h).await
"#, &["42", "5.0", "spawned", "3 2"]);
}

#[test]
fn a_legacy_borrow_still_cannot_cross_an_async_boundary() {
    let error = witchy::resolve_std_only_checked(
        "async fn f(x: &'a Int) -> Int:\n    (*x)\n\nasync fn main(console: Console):\n    let n = 1\n    let r = f(&n).await\n    console.print(\"${r}\")\n",
    )
    .expect_err("a lifetime-bearing reference is retired");
    assert!(error.to_string().contains("retired"), "{error}");
}

/// A task recipe or closure formed in a region cannot carry a view of a
/// region-born root out of it (RFC-0167 section 8; BUG-646): the callable or
/// recipe type hides the captured view, so the region refuses the copy-out.
#[test]
fn a_region_born_view_cannot_escape_in_a_task_recipe_or_closure() {
    for (label, source) in [
        ("task recipe", r#"
import chan
from task import Task

async fn worker(console: Console, v: &List(Int)):
    chan.yield_now().await
    console.print("${(*v).length()}")

async fn main(console: Console):
    let recipe = region -> Task(()):
        let local = [1, 2, 3]
        worker(console, &local)
    let h = chan.spawn(recipe).await
    chan.join(h).await
"#),
        ("closure", r#"
fn keep(v: &String) -> fn() -> Int:
    fn() -> Int: (*v).length()

fn main(console: Console):
    let f = region -> fn() -> Int:
        let local = "abc" + "def"
        keep(&local)
    console.print("${f()}")
"#),
    ] {
        let error = witchy::resolve_std_only_checked(source)
            .expect_err(label);
        assert!(error.to_string().contains("region-born root"), "{label}: {error}");
    }
}

/// A region that returns a closure with no view of its own locals still
/// copies out as before.
#[test]
fn a_region_returning_a_view_free_closure_still_copies_out() {
    let source = "fn main(console: Console):\n    let f = region -> fn() -> Int:\n        let local = [1, 2, 3]\n        let n = local.length()\n        fn() -> Int: n * 2\n    console.print(\"${f()}\")\n";
    witchy::resolve_std_only_checked(source).expect("a view-free closure leaves the region");
}
