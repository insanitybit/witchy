//! RFC-0090 tail-position conformance floor.
//!
//! The spec's tail-position rule (spec/language.md "Recursive proper tail calls
//! use constant control stack") enumerates which source forms are tail positions
//! and which are not. The other rfc0090_* suites prove the ABI-shaped cases
//! (references, var envelopes, indirect cycles, async/loans); this file pins the
//! SOURCE-FORM catalog itself: every claimed tail position runs at a depth far
//! beyond the compiled backend's non-tail frame budget, and every claimed
//! NON-tail form fails gracefully on admitted Wasm instead of being
//! misclassified as a loop (acceptance criteria 3 and 4).
//!
//! These are resource and semantic tests. The positive transition count (300,000)
//! is ~40x the measured depth at which a non-tail cycle exhausts the compiled
//! backend's call stack (traps between 6,000 and 8,000 frames), so a green
//! compiled run is itself the constant-stack proof.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

fn checked(source: &str) -> witchy::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source).expect("check tail-position program")
}

/// Run `source` on admitted Wasm and assert its independent expected output. A program that
/// grows the control stack instead of tail-looping traps on the compiled
/// backend thousands of transitions before the counts used here.
fn assert_constant_stack_output(source: &str, expected: &[&str], pages: usize) {
    let checked = checked(source);
    let want: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("tail-position program lowers");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities { print: true, quiet: true, ..Default::default() };
    let mut actor = runtime.spawn_admitted(&artifact, caps, pages).expect("spawn");
    actor.run().expect("compiled execution");
    assert_eq!(actor.output(), want, "compiled output (constant-stack loop)");
    assert_eq!(
        actor.canonical_heap_leaks().expect("audit canonical heap"),
        Some(0),
        "tail loop must release every owned canonical root",
    );
}

/// Run a NON-tail recursive `source` on admitted Wasm and assert it traps
/// on stack exhaustion. A hang or a success here would
/// mean a non-tail form was misclassified as a proper tail call.
fn assert_graceful_depth_failure(source: &str) {
    let checked = checked(source);
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("non-tail program still lowers");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities { print: true, quiet: true, ..Default::default() };
    let mut actor = runtime.spawn_admitted(&artifact, caps, 256).expect("spawn");
    let wasm_err = format!(
        "{:?}",
        actor
            .run()
            .expect_err("non-tail recursion must exhaust the compiled call stack")
    );
    assert!(
        wasm_err.contains("call stack exhausted"),
        "compiled backend must trap on stack exhaustion, got: {wasm_err}"
    );
}

// The selected fallback of a tail-position `??` is a tail position: the left
// operand has already been inspected and discarded when the fallback runs.
#[test]
fn coalesce_fallback_is_a_tail_position() {
    let source = r#"
fn lookup(n: Int) -> Option(Int):
    if n == 0:
        Some(42)
    else:
        None

fn go(n: Int) -> Int:
    lookup(n) ?? go(n - 1)

fn main(console: Console):
    console.print("${go(300000)}")
"#;
    assert_constant_stack_output(source, &["42"], 256);
}

// Every selected arm body of a tail-position `match` is a tail position, and
// scrutinee construction per step must not accumulate stack or leak.
#[test]
fn match_arm_bodies_are_tail_positions() {
    let source = r#"
type Step:
    Continue(Int)
    Done(Int)

fn next(n: Int) -> Step:
    if n == 0:
        Done(7)
    else:
        Continue(n - 1)

fn go(n: Int) -> Int:
    match next(n):
        Done(v) -> v
        Continue(m) -> go(m)

fn main(console: Console):
    console.print("${go(300000)}")
"#;
    assert_constant_stack_output(source, &["7"], 16384);
}

// Monomorphized generic self recursion participates as a direct edge after
// specialization — for every instantiation of the same generic function.
#[test]
fn generic_self_tail_is_constant_stack_per_instantiation() {
    let source = r#"
fn drain(acc: a, n: Int, keep: a) -> a:
    if n == 0:
        acc
    else:
        drain(keep, n - 1, keep)

fn main(console: Console):
    console.print("${drain(0, 300000, 5)}")
    console.print(drain("x", 300000, "y"))
"#;
    assert_constant_stack_output(source, &["5", "y"], 256);
}

// A trait-bounded generic function's self-tail edge is a direct edge after
// specialization; the bounded trait call in the base case resolves normally.
#[test]
fn trait_bounded_self_tail_is_constant_stack() {
    let source = r#"
fn spin(x: a, n: Int) -> String where a: Show:
    if n == 0:
        show(x)
    else:
        spin(x, n - 1)

fn main(console: Console):
    console.print(spin(11, 300000))
"#;
    assert_constant_stack_output(source, &["11"], 256);
}

// A closure value threaded through the recursion (applied per step, forwarded
// on the tail edge) keeps the edge proper; the application itself is an
// ordinary non-tail call inside the step.
#[test]
fn forwarded_closure_argument_keeps_the_self_edge_proper() {
    let source = r#"
fn go(f: fn(Int) -> Int, n: Int, acc: Int) -> Int:
    if n == 0:
        acc
    else:
        go(f, n - 1, f(acc))

fn main(console: Console):
    let inc = fn(x: Int): x + 1
    console.print("${go(inc, 300000, 0)}")
"#;
    assert_constant_stack_output(source, &["300000"], 256);
}

// An `own` heap aggregate forwards through the loop in its parameter slot,
// with per-step mutation (push + Eq-bounded remove) reclaimed as it goes.
#[test]
fn own_heap_parameter_forwards_through_the_tail_loop() {
    let source = r#"
fn go(own xs: List(Int), n: Int) -> List(Int):
    if n == 0:
        return xs
    xs.push(n % 7)
    if list.length(xs) > 3:
        let dropped = list.remove(xs, xs[0])
    go(xs, n - 1)

fn main(console: Console):
    console.print("${go([], 300000)}")
"#;
    assert_constant_stack_output(source, &["[3, 2, 1]"], 16384);
}

// A sum-typed (Result) return forwards through the self loop, and a `?` on a
// HELPER call earlier in the body leaves the later recursive edge proper: the
// `?` inspects the helper's result, not the recursion's.
#[test]
fn try_on_a_helper_leaves_the_recursive_edge_proper() {
    let source = r#"
fn check(n: Int) -> Result(Int, String):
    if n < 0:
        return Err("neg")
    Ok(n)

fn go(n: Int) -> Result(Int, String):
    if n == 0:
        return Ok(3)
    let v = check(n)?
    go(v - 1)

fn main(console: Console):
    match go(300000):
        Ok(v) -> console.print("${v}")
        Err(e) -> console.print(e)
"#;
    assert_constant_stack_output(source, &["3"], 256);
}

// NEGATIVE (criterion 4): an operator around the recursive call leaves real
// caller work, so the call is not tail and must fail gracefully at depth.
#[test]
fn operator_residual_is_not_tail_and_fails_gracefully() {
    let source = r#"
fn go(n: Int) -> Int:
    if n == 0:
        0
    else:
        go(n - 1) + 0

fn main(console: Console):
    console.print("${go(300000)}")
"#;
    assert_graceful_depth_failure(source);
}

// NEGATIVE: `?` on the recursive call inspects its error, so the call is not
// tail even when it is the textually last call before the final expression.
#[test]
fn try_on_the_recursive_call_is_not_tail() {
    let source = r#"
fn go(n: Int) -> Result(Int, String):
    if n == 0:
        return Ok(1)
    go(n - 1)?
    Ok(2)

fn main(console: Console):
    match go(300000):
        Ok(v) -> console.print("${v}")
        Err(e) -> console.print(e)
"#;
    assert_graceful_depth_failure(source);
}

// NEGATIVE: the LEFT operand of `??` is inspected for None, and the wrapping
// constructor is caller work — neither makes the recursive call tail.
#[test]
fn coalesce_left_operand_is_not_tail() {
    let source = r#"
fn go(n: Int) -> Option(Int):
    if n == 0:
        return Some(1)
    Some(go(n - 1) ?? 0)

fn main(console: Console):
    console.print("${go(300000) ?? -1}")
"#;
    assert_graceful_depth_failure(source);
}
