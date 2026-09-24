//! RFC-0167 row G: reading one element of a `&[Int]` view by value.
//!
//! `xs[i]` on a slice view is the read through its checked element view, so
//! the index is bounds-checked against the slice, not the backing list.
//! Independent expectation, canonical SafeWIR interpreter, admitted Wasm, zero leaks.

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

#[test]
fn a_slice_element_is_read_by_value_in_a_search() {
    assert_canonical_and_wasm(r#"
fn find(xs: &[Int], target: Int) -> Option(&Int):
    for i in 0..xs.length():
        if xs[i] == target:
            return Some(&xs[i])
    None

fn main() -> Int:
    let xs = [4, 5, 6]
    match find(&xs[0..3], 5):
        Some(v) -> (*v)
        None -> 0
"#, 5);
}

#[test]
fn slice_elements_sum_over_the_projection_only() {
    assert_canonical_and_wasm(r#"
fn sum(xs: &[Int]) -> Int:
    var total = 0
    for i in 0..xs.length():
        total = total + xs[i]
    total

fn main() -> Int:
    let xs = [1, 2, 3, 4, 5]
    sum(&xs[1..4]) * 100 + sum(&xs[0..0])
"#, 900);
}

/// Index 1 exists in the backing list but not in the one-element slice, so
/// the read aborts in both canonical engines rather than reading past it.
#[test]
fn a_slice_index_past_the_projection_aborts_on_both_engines() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn at(xs: &[Int], i: Int) -> Int:
    xs[i]

fn main() -> Int:
    let xs = [1, 2, 3]
    at(&xs[1..2], 1)
"#;
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
    assert!(oracle.is_err(), "the canonical interpreter aborts the out-of-slice read");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    assert!(vm.run().is_err(), "admitted Wasm traps on the out-of-slice read");
}
