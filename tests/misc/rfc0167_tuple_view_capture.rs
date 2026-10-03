//! RFC-0167 row G: a callable retains a structural tuple-root snapshot.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

fn run(source: &str, expected: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("structural tuple view capture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("structural tuple capture admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn callable_keeps_simple_tuple_root_after_replacement() {
    run(r#"
fn main() -> Int:
    var pair: (Int, String) = (2, "abc")
    let old = &pair
    let read = fn() -> Int: (*old).0 + string.length((*old).1)
    pair = (9, "q")
    read() * 10 + pair.0
"#, "59");
}

#[test]
fn callable_keeps_nested_graph_tuple_root_after_replacement() {
    run(r#"
fn main() -> Int:
    let n = 2
    var pair: (List(&Int), Int) = ([&n], 3)
    let old = &pair
    let read = fn() -> Int: (*(*old).0[0]) * 10 + (*old).1
    pair = ([&n], 9)
    read() * 10 + pair.1
"#, "239");
}

#[test]
fn inferred_mutable_root_keeps_its_captured_snapshot() {
    run(r#"
fn main() -> Int:
    let n = 2
    var pair = ([&n], 3)
    let old = &pair
    let read = fn() -> Int: (*old).1
    pair = ([&n], 9)
    read() * 10 + pair.1
"#, "39");
}

#[test]
fn qualified_assignment_preserves_a_later_capture() {
    run(r#"
fn make(v: &Int) -> (List(&Int), Int):
    ([v], 9)

fn main() -> Int:
    let n = 2
    var pair: (List(&Int), Int) = ([&n], 3)
    pair = make(&n)
    let old = &pair
    let read = fn() -> Int: (*old).1
    pair = ([&n], 4)
    read() * 10 + pair.1
"#, "94");
}
