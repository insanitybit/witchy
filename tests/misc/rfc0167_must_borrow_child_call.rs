//! A match borrows an affine child for one checked scalar-result call.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn borrowed_affine_field_enters_checked_scalar_call_without_leaving_parent() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Good(Int)
    Bad(Int)
must type Box:
    Box(Ticket)

fn inspect(let ticket: Ticket) -> Int:
    match ticket:
        Good(value) -> value + 1
        Bad(value) -> value + 2

fn observe(let box: Box) -> Int:
    match box:
        Box(ticket) -> inspect(ticket)

fn finish(own box: Box) -> Int:
    match move box:
        Box(ticket) ->
            match move ticket:
                Good(value) -> value
                Bad(value) -> value

fn main() -> Int:
    let box = Box(Good(7))
    let first = observe(box)
    let second = observe(box)
    first * 100 + second * 10 + finish(box)
"#).expect("borrowed child call checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed child call admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["887".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn borrowed_affine_field_cannot_escape_as_match_result() {
    let error = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Good(Int)
must type Box:
    Box(Ticket)

fn leak(let box: Box) -> Ticket:
    match box:
        Box(ticket) -> ticket

fn main() -> Int:
    0
"#).expect_err("borrowed child must not escape");
    assert!(error.to_string().contains("cannot return a borrowed must-consume child"),
        "unexpected diagnostic: {error}");
}

#[test]
fn borrowed_unique_child_cannot_escape_the_pattern_scope() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: unique List(Int)

fn leak(let box: Box) -> Int:
    match box:
        Box(value) -> value.length()

fn main() -> Int:
    0
"#).expect_err("borrowed unique child must not escape");
    assert!(error.to_string().contains("requires `move`"),
        "unexpected diagnostic: {error}");
}
