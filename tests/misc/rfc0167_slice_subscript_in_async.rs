//! RFC-0167 rows G/K: `s[i]` on a `&[T]` slice view inside an `async fn`.
//! The typed async projection keeps a subscript as a subscript, so it is the
//! checked element read it is in ordinary code, not a call to the user-facing
//! `list.at` (which stays closed to slices). Reads work before and after a
//! suspension, in an awaited task, and in an ordinary function of the same
//! program, and every slice keeps its snapshot while its `var` root is mutated.
//! A spawned child also retains the same authenticated slice snapshot while
//! its handle moves through the scheduler's affine Step carrier.
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

/// `s = &[10, 20, 30, 40][1..4]` = `[20, 30, 40]`; after `ns[3] = 7` and a
/// push the root is `[10, 20, 30, 7, 50]` but `s` still reads 20, 40, length
/// 3, and the task created before the mutation reads `s[2]` = 40; the
/// ordinary `first(s)` = 20. A spawned child later reads `s[1]` = 30.
/// `w = &["ab", "cd", "ef"][1..3]` keeps "cd" + "ef"
/// although index 1 became "zz"; an awaited task reads `w[1]` + "!" = "ef!".
#[test]
fn slice_subscripts_read_elements_inside_async_functions() {
    assert_async_output(
        "slice subscripts in async",
        r#"
import chan

fn first(xs: &[Int]) -> Int:
    xs[0]

async fn pick(xs: &[Int], i: Int) -> Int:
    let head = xs[0]
    chan.yield_now().await
    xs[i] + head - head

async fn word(ws: &[String], i: Int) -> String:
    chan.yield_now().await
    ws[i] + "!"

async fn spawned(console: Console, xs: &[Int]):
    chan.yield_now().await
    console.print("spawn ${xs[1]}")

async fn main(console: Console):
    var ns = [10, 20, 30, 40]
    let s = &ns[1..4]
    var ws = ["ab", "cd", "ef"]
    let w = &ws[1..3]
    let pending = pick(s, 2)
    chan.yield_now().await
    ns[3] = 7
    ns.push(50)
    ws[1] = "zz"
    chan.yield_now().await
    console.print("${s[0]} ${s[2]} ${s.length()} ${ns[3]} ${ns.length()}")
    let picked = pending.await
    console.print("${picked} ${first(s)}")
    console.print(w[0] + w[1])
    let x = word(w, 1).await
    console.print(x)
    let child = chan.spawn(spawned(console, s)).await
    chan.join(child).await
"#,
        &["20 40 3 7 5", "40 20", "cdef", "ef!", "spawn 30"],
    );
}
