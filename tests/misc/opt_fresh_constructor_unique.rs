//! In `mode opt`, a freshly constructed record is its only reference, so it
//! satisfies an `own ... unique` parameter both when passed inline and when
//! first bound to a `unique` local. Ordinary aliases stay rejected by the
//! checker's opt repair tests.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const RUN: &str = "mode opt\n\ntype State:\n    count: Int\n\n\
fn run(own state: unique State, n: Int) -> unique State:\n    if n == 0:\n        return state\n    state.count = state.count + 1\n    run(state, n - 1)\n\n";

fn output(main: &str) -> Vec<String> {
    let source = format!("{RUN}{main}");
    let checked = witchy::resolve_std_only_checked(&source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    actor.output().to_vec()
}

#[test]
fn an_inline_constructor_satisfies_a_unique_owned_parameter() {
    let out = output("fn main(console: Console):\n    let done = run(State(0), 50000)\n    console.print(\"${done.count}\")\n");
    assert_eq!(out, ["50000"]);
}

#[test]
fn a_constructor_bound_to_a_unique_local_satisfies_a_unique_owned_parameter() {
    let out = output("fn main(console: Console):\n    let start: unique State = State(0)\n    let done = run(start, 50000)\n    console.print(\"${done.count}\")\n");
    assert_eq!(out, ["50000"]);
}
