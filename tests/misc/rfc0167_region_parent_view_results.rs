//! RFC-0167 §8 / row L (SEC-096): a `region:` result aggregate may carry
//! views whose roots are parent-owned. A record, list, `Option` or tuple
//! leaves the region when every view it holds is proven parent-rooted, per
//! field: formed as `&place` over a root declared outside the region, or read
//! from a binding that is itself parent-rooted. A view of a region-born root,
//! or one whose provenance is unknown (a call result), is still refused.
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
fn a_region_record_of_a_parent_view_copies_out() {
    // The field views `parent`, declared outside the region: reads 7.
    assert_wasm(
        "type Holder:\n    v: &Int\n\nfn main() -> Int:\n    let parent = 7\n    let h = region -> Holder:\n        Holder(v: &parent)\n    *h.v\n",
        7,
    );
}

#[test]
fn a_parent_rooted_slice_view_leaves_a_region() {
    // The region retains the parent list but exposes only elements 2 and 3:
    // 2 + 3 = 5, independently of either execution engine.
    assert_wasm(
        "fn main() -> Int:\n    let xs = [1, 2, 3, 4]\n    let s = region -> &[Int]:\n        &xs[1..3]\n    var total = 0\n    for x in s:\n        total = total + x\n    total\n",
        5,
    );
}

#[test]
fn a_region_born_slice_view_cannot_leave_the_region() {
    assert_refused(
        "fn main() -> Int:\n    let s = region -> &[Int]:\n        let local = [1, 2, 3, 4]\n        &local[1..3]\n    s.length()\n",
        "region-born",
    );
}

#[test]
fn region_lists_options_and_tuples_of_parent_views_copy_out() {
    // [&a, &b] reads 3 and 4: 3 * 10 + 4.
    assert_wasm(
        "fn main() -> Int:\n    let a = 3\n    let b = 4\n    let vs = region -> List(&Int):\n        [&a, &b]\n    (*(vs[0])) * 10 + (*(vs[1]))\n",
        34,
    );
    // Some(&a) reads 9.
    assert_wasm(
        "fn main() -> Int:\n    let a = 9\n    let o = region -> Option(&Int):\n        Some(&a)\n    match o:\n        Some(v) -> *v\n        None -> 0\n",
        9,
    );
    // (&a, &xs[2], 1) reads 5, 30 and 1: 5 * 100 + 30 + 1.
    assert_wasm(
        "fn main() -> Int:\n    let a = 5\n    let xs = [10, 20, 30]\n    let t = region -> (&Int, &Int, Int):\n        (&a, &xs[2], 1)\n    let (x, y, z) = t\n    (*x) * 100 + (*y) + z\n",
        531,
    );
}

#[test]
fn a_view_through_a_parent_rooted_variable_copies_out() {
    // `va` is a parent view; `vb` is a region-local handle over parent `b`:
    // 2 * 10 + 8.
    assert_wasm(
        "type Pair:\n    left: &Int\n    right: &Int\n\nfn main() -> Int:\n    let a = 2\n    let b = 8\n    let va = &a\n    let p = region -> Pair:\n        let vb = &b\n        Pair(left: va, right: vb)\n    (*p.left) * 10 + (*p.right)\n",
        28,
    );
}

#[test]
fn a_region_born_view_beside_a_parent_view_is_refused() {
    assert_refused(
        "type Pair:\n    left: &Int\n    right: &Int\n\nfn main() -> Int:\n    let a = 2\n    let p = region -> Pair:\n        let local = 8\n        Pair(left: &a, right: &local)\n    *p.left\n",
        "per-field parent-root provenance",
    );
    // A shadowing local takes the parent-rooted name: the view is region-born.
    assert_refused(
        "type Holder:\n    v: &Int\n\nfn main() -> Int:\n    let parent = 7\n    let h = region -> Holder:\n        let v = &parent\n        let v = 3\n        Holder(v: &v)\n    *h.v\n",
        "per-field parent-root provenance",
    );
}

#[test]
fn a_view_from_a_call_result_in_a_region_is_refused() {
    assert_refused(
        "type Holder:\n    v: &Int\n\nfn pick(x: &Int) -> &Int:\n    x\n\nfn main() -> Int:\n    let parent = 7\n    let h = region -> Holder:\n        Holder(v: pick(&parent))\n    *h.v\n",
        "per-field parent-root provenance",
    );
}
