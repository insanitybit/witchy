//! RFC-0167 row G: a view instantiates a plain type parameter.
//!
//! `id[a](x: a)` called with `&n` binds `a = &Int` (a view is a type, not an
//! ownership qualifier), while `id[a](x: &a)` still binds `a = Int`.
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
fn a_view_passes_through_a_plain_generic_identity() {
    assert_canonical_and_wasm(r#"
fn id[a](x: a) -> a:
    x

fn main() -> Int:
    let n = 5
    let v = id(&n)
    (*v)
"#, 5);
}

#[test]
fn a_generic_choice_between_two_views_returns_the_chosen_one() {
    assert_canonical_and_wasm(r#"
fn pick[a](flag: Bool, x: a, y: a) -> a:
    if flag: x else: y

fn main() -> Int:
    let n = 5
    let m = 7
    (*pick(false, &n, &m)) * 10 + (*pick(true, &n, &m))
"#, 75);
}

#[test]
fn a_generic_over_an_option_payload_takes_a_view() {
    assert_canonical_and_wasm(r#"
fn or_else[a](o: Option(a), fallback: a) -> a:
    match o:
        Some(value) -> value
        None -> fallback

fn main() -> Int:
    let n = 5
    let m = 7
    (*or_else(Some(&n), &m)) * 10 + (*or_else(None, &m))
"#, 57);
}

#[test]
fn a_view_pattern_still_binds_the_payload_type() {
    assert_canonical_and_wasm(r#"
fn keep[a](x: &a) -> &a:
    x

fn main() -> Int:
    let n = 9
    (*keep(&n))
"#, 9);
}
