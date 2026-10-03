//! An inferred tuple retains source storage qualifiers for deep `var` writes.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn inferred_tuple_allows_deep_scalar_write_without_losing_must_sibling() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
type Inner:
    ticket: Ticket
    count: Int
fn finish(own pair: (Inner, Int)) -> Int:
    match move pair:
        (inner, extra) -> match move inner:
            Inner(ticket, count) -> match move ticket:
                Ticket(value) -> value * 100 + count * 10 + extra
fn main() -> Int:
    var pair = (Inner(Ticket(7), 1), 2)
    pair.0.count = 9
    finish(pair)
"#).expect("inferred tuple deep update checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("inferred tuple deep update admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["792".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn inferred_tuple_preserves_frozen_element_through_alias() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let item: frozen Box = Box(1)
    var pair = (item, 2)
    pair.0.value = 9
    0
"#).expect_err("inferred tuple must retain the frozen element's qualifier");
    assert!(error.to_string().contains("frozen")
        || error.to_string().contains("cannot verify deep assignment place"), "{error}");
}
