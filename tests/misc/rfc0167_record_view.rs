//! RFC-0167 row G: a view of a whole user record or sum.
//!
//! `&p` is carried as the retained record object: a record is an immutable
//! value shared by retain, so the token is the snapshot (`RecordRoot`).
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
fn a_record_view_keeps_its_snapshot_when_the_owner_is_reassigned() {
    assert_canonical_and_wasm(r#"
type Point:
    x: Int
    y: Int

fn main() -> Int:
    var p = Point(x: 3, y: 4)
    let v = &p
    p = Point(x: 7, y: 9)
    (*v).x * 1000 + (*v).y * 100 + p.x * 10 + p.y
"#, 3479);
}

