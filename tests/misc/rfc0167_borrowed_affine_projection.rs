//! Scalar field access through a borrowed resource-bearing aggregate.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen::LoweringOutcome;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn direct_projection_reads_scalar_under_borrowed_affine_record() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)

type Inner:
    ticket: Ticket
    count: Int

type Outer:
    inner: Inner
    tag: Int

fn inspect(let outer: Outer) -> Int:
    outer.inner.count

fn finish(own outer: Outer) -> Int:
    match move outer:
        Outer(inner, tag) -> match move inner:
            Inner(ticket, count) -> match move ticket:
                Ticket(value) -> value * 100 + count * 10 + tag

fn main() -> Int:
    let outer = Outer(Inner(Ticket(7), 4), 2)
    inspect(outer) * 1000 + finish(outer)
"#).expect("borrowed scalar projection checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed scalar projection admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["4742".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn direct_tuple_projection_reads_scalar_but_cannot_return_affine_child() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
type Inner:
    ticket: Ticket
    count: Int
fn inspect(let pair: (Inner, Int)) -> Int:
    pair.0.count + pair.1
fn finish(own pair: (Inner, Int)) -> Int:
    match move pair:
        (inner, extra) -> match move inner:
            Inner(ticket, count) -> match move ticket:
                Ticket(value) -> value + count + extra
fn main() -> Int:
    let pair = (Inner(Ticket(7), 4), 2)
    inspect(pair) * 100 + finish(pair)
"#).expect("borrowed tuple scalar projection checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed tuple scalar projection admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["613".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));

    let escape = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
type Inner:
    ticket: Ticket
    count: Int
type Outer:
    inner: Inner
fn escape(let outer: Outer) -> Inner:
    outer.inner
fn main() -> Int:
    0
"#);
    if let Ok(checked) = escape {
        assert!(!matches!(witchy::codegen::compile_checked_module_artifact(&checked),
            LoweringOutcome::Lowered(_)),
            "a borrowed affine child must not be returned");
    }
}
