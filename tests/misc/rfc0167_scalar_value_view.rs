//! RFC-0167 row G: views of `Float`, `Bool`, and `Duration` values.
//!
//! An immutable scalar read at formation is exactly what its view observes,
//! so these views are carried as the value (`ScalarValue`), the RFC's
//! proven-equivalent representation. `&Int` keeps its cell carrier.
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
fn a_float_view_crosses_a_call() {
    assert_canonical_and_wasm(r#"
fn keep(x: &Float) -> &Float:
    x

fn main() -> Int:
    let x = 2.5
    let v = keep(&x)
    if (*v) > 2.0: 1 else: 0
"#, 1);
}

#[test]
fn a_bool_view_keeps_its_snapshot_when_the_owner_is_reassigned() {
    assert_canonical_and_wasm(r#"
fn main() -> Int:
    var b = true
    let v = &b
    b = false
    let old = if (*v): 10 else: 0
    let new = if b: 1 else: 0
    old + new
"#, 10);
}

#[test]
fn scalar_field_views_read_through_a_generic() {
    assert_canonical_and_wasm(r#"
type Reading:
    level: Float
    ok: Bool
    wait: Duration

fn id[a](x: a) -> a:
    x

fn main() -> Int:
    let r = Reading(level: 1.5, ok: true, wait: 30s)
    let lv = &r.level
    let okv = id(&r.ok)
    let wv = &r.wait
    let a = if (*lv) > 1.0: 100 else: 0
    let b = if (*okv): 10 else: 0
    let c = if (*wv) == 30s: 1 else: 0
    a + b + c
"#, 111);
}
