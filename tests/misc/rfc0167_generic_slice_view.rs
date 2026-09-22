//! RFC-0167 row O: the generic slice carrier. A `&[T]` view of a `List` of
//! `Float`, `Bool`, `Duration`, or `String` elements is the same zero-copy
//! carrier as `&[Int]`: the retained list root plus a checked `{start, len}`
//! projection. Element reads are bounds-checked against the slice, not the
//! backing list; scalar element views are the value and a String element
//! view is the retained String token.
//!
//! Independent expectation, canonical SafeWIR interpreter, admitted
//! unoptimized Wasm, zero-leak audit on both. The source interpreter is not
//! consulted.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_canonical_and_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)],
        "canonical interpreter matches the independent expected result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

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

/// The program must compile, then abort in the canonical interpreter and
/// trap in admitted Wasm.
fn assert_aborts_on_both_engines(source: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
        },
    )
    .expect_lowered("canonical SafeWIR admission");
    assert!(oracle.is_err(), "the canonical interpreter aborts");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    assert!(vm.run().is_err(), "admitted Wasm traps");
}

/// Formation, `.length()`, a by-value read, an element view, and a slice
/// passed to and returned from a call. 6.0 = 2.5 + 3.5 is exact in binary.
#[test]
fn a_float_slice_reads_its_projection_across_calls() {
    assert_canonical_and_wasm(r#"
fn relay(xs: &[Float]) -> &[Float]:
    xs

fn sum(xs: &[Float]) -> Float:
    var total = 0.0
    for i in 0..xs.length():
        total = total + xs[i]
    total

fn main() -> Int:
    let xs = [1.5, 2.5, 3.5, 4.5]
    let s = relay(&xs[1..3])
    let e = &s[1]
    let a = if sum(s) == 6.0: 100 else: 0
    let b = if (*e) == 3.5: 10 else: 0
    a + b + s.length()
"#, 112);
}

/// A String element read yields the retained String token, and a String
/// element view is that token.
#[test]
fn a_string_slice_reads_its_projection_across_calls() {
    assert_canonical_and_wasm(r#"
fn relay(xs: &[String]) -> &[String]:
    xs

fn total_length(xs: &[String]) -> Int:
    var total = 0
    for i in 0..xs.length():
        let word: String = xs[i]
        total = total + word.length()
    total

fn main() -> Int:
    let words = ["a", "bb", "ccc", "dddd"]
    let s = relay(&words[1..4])
    let e: &String = &s[0]
    total_length(s) * 10 + (*e).length() + s.length() * 100
"#, 392);
}

#[test]
fn bool_and_duration_slices_read_their_projection() {
    assert_canonical_and_wasm(r#"
fn count(xs: &[Bool]) -> Int:
    var n = 0
    for i in 0..xs.length():
        if xs[i]:
            n = n + 1
    n

fn main() -> Int:
    let flags = [true, false, true, true]
    let waits = [1s, 2s, 3s]
    let f = &flags[0..3]
    let w = &waits[1..3]
    let d = &w[0]
    let a = count(f) * 100
    let b = if w[1] == 3s: 10 else: 0
    let c = if (*d) == 2s: 1 else: 0
    a + b + c
"#, 211);
}

/// A slice of a slice composes its projection with the parent's: the child
/// reads the backing list at the summed offset, and its length is its own.
#[test]
fn a_slice_of_a_slice_composes_the_projection() {
    assert_canonical_and_wasm(r#"
fn main() -> Int:
    let xs = [1.5, 2.5, 3.5, 4.5, 5.5]
    let words = ["a", "bb", "ccc", "dddd", "eeeee"]
    let s = &xs[1..5]
    let i = 1
    let t = &s[i..3]
    let u = &words[1..5]
    let v = &u[2..4]
    let a = if t[0] == 3.5 && t[1] == 4.5: 100 else: 0
    let b = t.length() * 10
    let c = v[0].length() + v[1].length() + v.length() * 1000
    a + b + c
"#, 2129);
}

/// RFC-0167's generic `first`, instantiated at `Float` and at `String`.
#[test]
fn generic_first_instantiates_at_float_and_string() {
    assert_canonical_and_wasm(r#"
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0:
        None
    else:
        Some(&items[0])

fn main() -> Int:
    let fs = [1.5, 2.5]
    let ws = ["xy", "z"]
    let f = match first(&fs[1..2]):
        Some(v) -> if (*v) == 2.5: 100 else: 0
        None -> 0
    let w = match first(&ws[0..1]):
        Some(v) -> (*v).length() * 10
        None -> 0
    let e = match first(&ws[1..1]):
        Some(_) -> 0
        None -> 1
    f + w + e
"#, 121);
}

/// A view is a value-semantic snapshot: a later push or indexed store to the
/// owner `var` list writes fresh storage, and the retained root keeps what the
/// slice observed at formation.
#[test]
fn a_slice_keeps_its_snapshot_when_the_owner_list_changes() {
    assert_canonical_and_wasm(r#"
fn main() -> Int:
    var xs = [1.5, 2.5, 3.5]
    var words = ["a", "bb", "ccc"]
    let s = &xs[0..2]
    let w = &words[1..3]
    xs[1] = 9.5
    xs.push(7.5)
    words[1] = "zzzzzzzz"
    words.push("q")
    let a = if s[1] == 2.5: 100 else: 0
    let b = if xs[1] == 9.5: 10 else: 0
    let c = w[0].length() + s.length() * 1000 + words.length() * 10000
    a + b + c
"#, 42112);
}

/// Index 1 exists in the backing list but not in the one-element slice, so
/// the read aborts rather than reading past the projection.
#[test]
fn a_slice_index_past_the_projection_aborts_on_both_engines() {
    assert_aborts_on_both_engines(r#"
fn at(xs: &[Float], i: Int) -> Float:
    xs[i]

fn main() -> Int:
    let xs = [1.5, 2.5, 3.5]
    if at(&xs[1..2], 1) > 0.0: 1 else: 0
"#);
    assert_aborts_on_both_engines(r#"
fn at(xs: &[String], i: Int) -> Int:
    let word: String = xs[i]
    word.length()

fn main() -> Int:
    let xs = ["a", "bb", "ccc"]
    at(&xs[0..1], 1)
"#);
}

/// A sub-slice past the parent's end aborts even though the backing list is
/// long enough.
#[test]
fn a_sub_slice_past_the_parent_projection_aborts_on_both_engines() {
    assert_aborts_on_both_engines(r#"
fn main() -> Int:
    let xs = [1.5, 2.5, 3.5, 4.5]
    let s = &xs[0..2]
    let n = 3
    let t = &s[1..n]
    t.length()
"#);
}

/// A slice of a List of Lists has no admitted carrier: its elements are
/// owners, not scalar values or Strings, so it stays fail-closed.
#[test]
fn a_slice_of_nested_lists_stays_fail_closed() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn relay(items: &[List(Int)]) -> &[List(Int)]:
    items

fn main() -> Int:
    let values = [[1], [2, 3]]
    let middle = relay(&values[0..1])
    middle.length()
"#).expect("the slice type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("&[List(Int)] has no admitted carrier");
    assert!(refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"),
        "refused at the canonical boundary: {refusal}");
}
