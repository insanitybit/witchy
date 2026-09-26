//! RFC-0167 row K: `&Float`, `&Bool`, and `&Duration` views cross `await`.
//! These views are carried as the scalar value (`ScalarValue`), and async
//! lowering carries live locals and parameters into continuation segments as
//! `own` parameters; consuming a scalar-value view is a plain copy of the value.
//!
//! Independent expectation, admitted unoptimized Wasm, zero-leak audit. One
//! program, one async compile. A spawned task holding these views is not
//! evidence here: `chan.spawn` yields a `task.Step(task.Handle)` whose `must`
//! payload still lacks a consuming extraction from its opaque GC owner, so it
//! fails closed (acceptance ledger, row K channel boundary).

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

#[test]
fn scalar_value_views_cross_await() {
    // Views taken at f = 2.5, b = true, d = 30s keep those snapshots while the
    // `var` roots become 9.0, false, 1s between suspensions. Async functions
    // that yield before reading: 2.5 / 2 = 1.25, !true = false, 30s + 30s =
    // 60s.
    assert_async_output("scalar views across await", r#"
import chan

async fn half(v: &Float) -> Float:
    chan.yield_now().await
    (*v) / 2.0

async fn flip(v: &Bool) -> Bool:
    chan.yield_now().await
    !(*v)

async fn twice(v: &Duration) -> Duration:
    chan.yield_now().await
    (*v) + (*v)

async fn main(console: Console):
    var f = 2.5
    let fv = &f
    var b = true
    let bv = &b
    var d = 30s
    let dv = &d
    chan.yield_now().await
    f = 9.0
    b = false
    d = 1s
    chan.yield_now().await
    console.print("${*fv} ${f}")
    console.print("${*bv} ${b}")
    console.print("${(*dv) == 30s} ${d == 1s}")
    let hf = half(fv).await
    console.print("${hf}")
    let fb = flip(bv).await
    console.print("${fb}")
    let dd = twice(dv).await
    console.print("${dd == 60s}")
"#, &["2.5 9.0", "true false", "true true", "1.25", "false", "true"]);
}
