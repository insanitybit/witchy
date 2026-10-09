//! `WITCHY_COUNT_MEMORY_OPS` adds exact allocation, retain and release
//! counters to an admitted module. The counted module must still admit (the
//! independent plan mirrors the counters) and must compute the same result.

use witchy::runtime::{Capabilities, Runtime};

const PROGRAM: &str = r#"
type Box:
    value: Int
fn main() -> Int:
    var total = 0
    var i = 0
    while i < 10:
        let b = Box(i)
        total = total + b.value
        i = i + 1
    total
"#;

#[test]
fn counted_module_admits_and_reports_memory_ops() {
    // nextest runs each test in its own process, so the encode-time flag
    // does not leak into any other test.
    unsafe { std::env::set_var("WITCHY_COUNT_MEMORY_OPS", "1") };
    let checked = witchy::resolve_std_only_checked(PROGRAM).expect("program checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("counted module admits against its plan");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["45".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
    let counts = vm.canonical_memory_op_counts().expect("counted module exports its counters");
    // One Box per iteration at least; whatever else the lowering allocates
    // is released, since the heap audit found nothing live.
    assert!(counts.allocs >= 10, "allocs: {counts:?}");
    assert!(counts.releases >= 1, "releases: {counts:?}");
}

#[test]
fn ordinary_module_exports_no_counters() {
    let checked = witchy::resolve_std_only_checked(PROGRAM).expect("program checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.canonical_memory_op_counts(), None);
}

const NULLARY_PROGRAM: &str = r#"
type Tree:
    Leaf
    Node(Tree, Tree)

fn depth(let t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, r) -> 1 + math.max(depth(l), depth(r))

fn main() -> Int:
    let a = Node(Leaf, Leaf)
    let t = Node(a, Leaf)
    depth(t)
"#;

/// A payload-free variant is one static object per module, not a fresh
/// allocation per occurrence: two `Node`s allocate, three `Leaf`s do not.
#[test]
fn nullary_variants_allocate_nothing() {
    unsafe { std::env::set_var("WITCHY_COUNT_MEMORY_OPS", "1") };
    let checked = witchy::resolve_std_only_checked(NULLARY_PROGRAM).expect("program checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("counted module admits against its plan");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
    let counts = vm.canonical_memory_op_counts().expect("counted module exports its counters");
    assert_eq!(counts.allocs, 2, "allocs: {counts:?}");
}
