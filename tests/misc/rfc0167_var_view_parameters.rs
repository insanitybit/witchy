//! RFC-0167 rows D/H: `var view: &T` replaces the caller's view carrier by
//! write-back; it never mutates the viewed value.
//!
//! `&Int` places, `&[T]` slices, and `Option(&Int)` are opaque GC carriers.
//! The callee rebinds the carrier and the caller observes the new view after
//! the call, while earlier snapshots and the viewed roots stay unchanged.
//! Inside `region:` such a write-back into an outer binding is refused, so a
//! `var` lane is never an escape path for a region-born view (BUG-648).
//! Independent expectation, admitted unoptimized Wasm, zero-leak heap audit.

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

#[test]
fn var_int_view_rebinds_the_callers_carrier() {
    // The caller sees 20 through `v`; the old snapshot still reads 1, and so
    // does `a`: 20*100 + 1*10 + 1.
    assert_wasm(r#"
fn retarget(var v: &Int, other: &Int):
    v = other

fn main() -> Int:
    let a = 1
    let b = 20
    var v: &Int = &a
    let before = v
    retarget(v, &b)
    *v * 100 + *before * 10 + a
"#, 2011);
}

