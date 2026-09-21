//! RFC-0167 row C: the unit and discard surface, and what `let _ =` cannot buy.
//!
//! The positive program runs on the RFC-0167 chain (independent expectation,
//! canonical SafeWIR interpreter, admitted unoptimized Wasm, zero leaks). The
//! rest are refusals, which are the point of this row.

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
    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)]);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

fn refusal(source: &str) -> String {
    witchy::resolve_std_only_checked(source)
        .err()
        .unwrap_or_else(|| panic!("this shape must be refused: {source}"))
        .to_string()
}

/// A unit result stands alone as a statement; a non-unit one must be bound or
/// discarded explicitly. Both spellings of unit behave the same.
#[test]
fn a_unit_result_stands_alone_and_a_value_result_does_not() {
    assert_canonical_and_wasm(
        "fn note(n: Int):\n    ()\n\nfn explicit(n: Int) -> ():\n    ()\n\nfn main() -> Int:\n    note(1)\n    explicit(2)\n    let _ = 7\n    79\n",
        79,
    );
    let error = refusal(
        "fn total(n: Int) -> Int:\n    n\n\nfn main() -> Int:\n    total(1)\n    79\n",
    );
    assert!(
        error.contains("is discarded") && error.contains("let _ ="),
        "the refusal must name the explicit discard: {error}"
    );
}

/// `let _ =` discards a VALUE. It does not discharge an obligation: a
/// must-consume result stays must-consume, so the escape hatch for ordinary
/// results cannot be used to drop one.
#[test]
fn an_explicit_discard_cannot_drop_a_must_consume_result() {
    let error = refusal(
        "import chan\n\nasync fn worker():\n    ()\n\nasync fn main(console: Console):\n    let _ = chan.spawn(worker()).await\n    console.print(\"done\")\n",
    );
    assert!(
        error.contains("must-consume"),
        "discarding a must-consume result must name the obligation: {error}"
    );
}
