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

#[test]
fn var_option_view_rebinds_without_changing_the_old_snapshot() {
    assert_wasm(r#"
fn retarget(var v: Option(&Int), other: &Int):
    v = Some(other)

fn read(v: Option(&Int)) -> Int:
    match v:
        Some(item) -> *item
        None -> 0

fn main() -> Int:
    let a = 1
    let b = 20
    var v: Option(&Int) = Some(&a)
    let before = v
    let change = retarget
    change(v, &b)
    read(v) * 100 + read(before) * 10 + a
"#, 2011);
}

#[test]
fn var_slice_view_rebinds_without_changing_the_old_projection() {
    assert_wasm(r#"
fn retarget(var v: &[Int], other: &[Int]):
    v = other

fn main() -> Int:
    let a = [1, 2, 3]
    let b = [4, 5, 6]
    var v: &[Int] = &a[0..2]
    let before = v
    let change = retarget
    change(v, &b[1..2])
    v.length() * 100 + before.length() * 10 + a.length()
"#, 123);
}

#[test]
fn indirect_var_option_view_cannot_escape_a_region_born_root() {
    let source = r#"
fn retarget(var v: Option(&Int), other: &Int):
    v = Some(other)

fn main() -> Int:
    let root = 1
    var v: Option(&Int) = Some(&root)
    let change = retarget
    let _ = region -> ():
        let local = 5
        change(v, &local)
    0
"#;
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .map(|error| error.to_string())
        .expect("region-born view is refused at checking");
    assert!(
        error.contains("cannot write back into `v` inside `region:`")
            && error.contains("can hold a view"),
        "refusal names the region write-back rule: {error}"
    );
}
