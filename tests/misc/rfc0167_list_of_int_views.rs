//! RFC-0167 row G: a `List(&Int)` holds `&Int` views. It is the `&Int`
//! family's owner list: each element is one retained, authenticated place
//! carrier, so an element read is the view's snapshot even after the root it
//! names is written or grown. Lists of views are built, pushed, indexed,
//! iterated, passed to and returned from functions, and passed to generics.
//!
//! Evidence: an independent expected result and admitted unoptimized Wasm
//! with a zero-leak heap audit. SafeWIR admission is required; the canonical
//! interpreter is not consulted.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

fn assert_refused(source: &str, needle: &str) {
    let error = witchy::resolve_std_only_checked(source).err().map(|error| error.to_string())
        .expect("the program is refused at checking");
    assert!(error.contains(needle), "refusal names the rule: {error}");
}

#[test]
fn a_list_of_int_views_is_built_indexed_and_iterated() {
    // 3 + 4 read through the views; two elements; index 1 reads 4.
    assert_wasm(
        "fn main() -> Int:\n    let a = 3\n    let b = 4\n    let vs = [&a, &b]\n    var t = 0\n    for v in vs:\n        t = t + (*v)\n    t * 100 + vs.length() * 10 + (*(vs[1]))\n",
        724,
    );
    // Pushing onto a `var` list of views: 2 elements summing to 7.
    assert_wasm(
        "fn main() -> Int:\n    let a = 3\n    let b = 4\n    var vs = [&a]\n    vs.push(&b)\n    var t = 0\n    for v in vs:\n        t = t + (*v)\n    vs.length() * 10 + t\n",
        27,
    );
}

#[test]
fn element_views_in_a_list_keep_their_snapshots() {
    // Views of xs[0] (1) and xs[2] (3); xs[0] is then written to 100 and xs
    // grows: the views still read 1 and 3.
    assert_wasm(
        "fn main() -> Int:\n    var xs = [1, 2, 3]\n    let vs = [&xs[0], &xs[2]]\n    xs[0] = 100\n    xs.push(9)\n    (*(vs[0])) * 10000 + (*(vs[1])) * 1000 + xs[0]\n",
        13100,
    );
}

#[test]
fn a_list_of_int_views_crosses_function_boundaries() {
    // Parameter: the first view reads 3.
    assert_wasm(
        "fn first(vs: List(&Int)) -> Int:\n    *(vs[0])\n\nfn main() -> Int:\n    let a = 3\n    first([&a])\n",
        3,
    );
    // Result: views of the slice [6, 7, 8] sum to 21.
    assert_wasm(
        "fn views(xs: &[Int]) -> List(&Int):\n    var out = [&xs[0]]\n    for i in 1..xs.length():\n        out.push(&xs[i])\n    out\n\nfn main() -> Int:\n    let xs = [5, 6, 7, 8]\n    var t = 0\n    for v in views(&xs[1..4]):\n        t = t + (*v)\n    t\n",
        21,
    );
    // A generic `List(a)` parameter instantiated at `&Int`.
    assert_wasm(
        "fn count[a](xs: List(a)) -> Int:\n    xs.length()\n\nfn main() -> Int:\n    let a = 1\n    let b = 2\n    count([&a, &b])\n",
        2,
    );
}

#[test]
fn a_region_born_view_cannot_escape_in_a_list() {
    assert_refused(
        "fn main() -> Int:\n    let vs = region -> List(&Int):\n        let local = 5\n        [&local]\n    *(vs[0])\n",
        "cannot copy out a region aggregate carrying immutable views",
    );
}
