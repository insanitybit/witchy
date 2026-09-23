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

fn assert_refused(source: &str, needle: &str) {
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .unwrap_or_else(|| panic!("fixture must be refused: {source}"));
    let message = format!("{error:?}");
    assert!(message.contains(needle), "expected `{needle}` in: {message}");
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
fn var_slice_view_narrows_in_a_loop() {
    // Drop the first element until empty: 3+5+7+11 = 26 over four calls; the
    // backing list keeps all four elements and the snapshot keeps its length.
    assert_wasm(r#"
fn drop_first(var s: &[Int]):
    s = &s[1..s.length()]

fn main() -> Int:
    let xs = [3, 5, 7, 11]
    var s: &[Int] = &xs[0..4]
    let whole = s
    var total = 0
    var calls = 0
    while s.length() > 0:
        total = total + s[0]
        drop_first(s)
        calls = calls + 1
    total * 1000 + calls * 100 + xs.length() * 10 + whole.length()
"#, 26444);
}

#[test]
fn var_slice_view_recurses_and_string_slices_narrow() {
    // [1, 2, 9, 3] skipping leading values below 5 leaves [9, 3]: 2*100 +
    // 9*10; the String slice drops "a" and keeps 2 elements.
    assert_wasm(r#"
fn skip_small(var s: &[Int]):
    if s.length() > 0:
        if s[0] < 5:
            s = &s[1..s.length()]
            skip_small(s)

fn drop_word(var s: &[String]):
    s = &s[1..s.length()]

fn main() -> Int:
    let xs = [1, 2, 9, 3]
    var s: &[Int] = &xs[0..4]
    skip_small(s)
    let ws = ["a", "bc", "d"]
    var w: &[String] = &ws[0..3]
    drop_word(w)
    s.length() * 100 + s[0] * 10 + w.length()
"#, 292);
}

#[test]
fn var_slice_view_writes_back_across_early_exits() {
    // [1, 2, 300, 4] sums to 303 and returns early with one element left;
    // [1, 2] drains, then `?` propagates Err (-1) with the empty slice
    // written back: 303*1000 + 1*10 - 1 + 0.
    assert_wasm(r#"
fn step(var s: &[Int]) -> Result(Int, String):
    if s.length() == 0:
        return Err("empty")
    let head = s[0]
    s = &s[1..s.length()]
    Ok(head)

fn sum(var s: &[Int]) -> Result(Int, String):
    var total = 0
    while true:
        let h = step(s)?
        total = total + h
        if h > 100:
            return Ok(total)
    Ok(total)

fn main() -> Int:
    let xs = [1, 2, 300, 4]
    var s: &[Int] = &xs[0..4]
    let r = sum(s) ?? -1
    let ys = [1, 2]
    var t: &[Int] = &ys[0..2]
    let e = sum(t) ?? -1
    r * 1000 + s.length() * 10 + e + t.length()
"#, 303009);
}

#[test]
fn var_option_view_is_set_from_none_to_some() {
    // None becomes Some(&a): 7*10, plus 1 for the None observed beforehand.
    assert_wasm(r#"
fn set(var o: Option(&Int), v: &Int):
    o = Some(v)

fn read(o: Option(&Int)) -> Int:
    match o:
        Some(v) -> *v * 10
        None -> 1

fn main() -> Int:
    let a = 7
    var o: Option(&Int) = None
    let before = read(o)
    set(o, &a)
    read(o) + before
"#, 71);
}

#[test]
fn var_int_view_is_called_in_a_loop() {
    // i = 0..4 alternates a, b, a, b, a: 3+4+3+4+3.
    assert_wasm(r#"
fn pick(var v: &Int, a: &Int, b: &Int, i: Int):
    v = if i % 2 == 0: a else: b

fn main() -> Int:
    let a = 3
    let b = 4
    var v: &Int = &a
    var total = 0
    var i = 0
    while i < 5:
        pick(v, &a, &b, i)
        total = total + *v
        i = i + 1
    total
"#, 17);
}

#[test]
fn var_view_write_back_cannot_escape_a_region() {
    let refusal = "inside `region:`: it is declared outside the region and can hold a view";
    // A `var &Int` rebind to a region-born local.
    assert_refused(r#"
fn retarget(var v: &Int, other: &Int):
    v = other

fn main() -> Int:
    let a = 1
    var v: &Int = &a
    let _ = region -> ():
        let local = 5
        retarget(v, &local)
    *v
"#, refusal);
    // BUG-648: a region-born view pushed through a `var List(&Int)`.
    assert_refused(r#"
fn add(var vs: List(&Int), v: &Int):
    vs.push(v)

fn main() -> Int:
    var vs: List(&Int) = []
    let _ = region -> ():
        let local = 5
        add(vs, &local)
    0
"#, refusal);
}
