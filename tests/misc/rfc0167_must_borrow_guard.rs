//! A scalar guard may inspect an affine pattern without transferring its owner.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn guarded_borrowed_nested_sum_keeps_root_for_fallthrough_and_later_consumption() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Good(Int)
    Bad(Int)
must type Box:
    Box(Ticket)

fn inspect(let box: Box, limit: Int) -> Int:
    match box:
        Box(Good(value)) if value > limit -> value + 1
        Box(Good(value)) -> value * 10
        Box(Bad(value)) if value > limit -> value + 2
        Box(Bad(value)) -> value * 100

fn finish(own box: Box) -> Int:
    match move box:
        Box(ticket) ->
            match move ticket:
                Good(value) -> value
                Bad(value) -> value

fn main() -> Int:
    let good = Box(Good(3))
    let bad = Box(Bad(7))
    let first = inspect(good, 2)
    let second = inspect(good, 5)
    let third = inspect(bad, 8)
    first * 10000 + second * 100 + third + finish(good) + finish(bad)
"#).expect("borrowed guarded sum checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed guarded sum admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["43710".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn guarded_borrowed_match_rejects_root_transfer_before_fallthrough() {
    let error = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Good(Int)
    Bad(Int)

fn finish(own ticket: Ticket) -> Bool:
    match move ticket:
        Good(_) -> true
        Bad(_) -> false

fn main() -> Int:
    let ticket = Good(3)
    match ticket:
        Good(value) if finish(ticket) -> value
        Good(value) -> value + 1
        Bad(value) -> value
"#).expect_err("a guard cannot transfer the borrowed root before fallthrough");
    assert!(error.to_string().contains("match guard cannot consume borrowed affine root"),
        "unexpected diagnostic: {error}");
}
