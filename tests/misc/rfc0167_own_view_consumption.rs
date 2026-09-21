//! RFC-0167 row D: `own` on a view carrier consumes the carrier, not the payload.
//!
//! Positive fixtures use an independent scalar expectation, the canonical
//! SafeWIR interpreter, and admitted unoptimized Wasm, with a zero-leak audit on
//! both. The source interpreter is not consulted.
//!
//! The distinction the row is about: `own value: &T` is an affine hold on the
//! CARRIER binding. Consuming it says nothing about the root the view was taken
//! from, which stays live and can be viewed again.

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

fn refusal(source: &str) -> String {
    witchy::resolve_std_only_checked(source)
        .err()
        .unwrap_or_else(|| panic!("this shape must be refused: {source}"))
        .to_string()
}

const TAKE: &str = "fn take(own value: &Int) -> Int:\n    (*value)\n\n";

/// Consuming a view carrier leaves the root alone: it stays readable, and a
/// second view of the same root is an ordinary thing to take.
#[test]
fn owning_a_view_carrier_does_not_consume_the_root() {
    // Reads the consumed view and the untouched root in one expression.
    assert_canonical_and_wasm(
        &format!("{TAKE}fn main() -> Int:\n    let n = 79\n    let first = take(&n)\n    first - n + 79\n"),
        79,
    );
    // Two independent views of one root, both consumed.
    assert_canonical_and_wasm(
        &format!("{TAKE}fn main() -> Int:\n    let n = 79\n    take(&n) + take(&n) - 79\n"),
        79,
    );
}

/// The same consumption through an indirect call, and through a private return
/// whose view outlives the call that produced it.
#[test]
fn an_owned_view_carrier_survives_indirect_calls_and_private_returns() {
    assert_canonical_and_wasm(
        &format!("{TAKE}fn main() -> Int:\n    let n = 79\n    let action = take\n    action(&n)\n"),
        79,
    );
    assert_canonical_and_wasm(
        "fn forward(own value: &Int) -> &Int:\n    value\n\nfn main() -> Int:\n    let n = 79\n    let out = forward(&n)\n    (*out) - n + 79\n",
        79,
    );
    // The same function with its result annotation omitted: the row requires
    // that a missing annotation cannot change what the call consumes.
    assert_canonical_and_wasm(
        "fn forward(own value: &Int):\n    value\n\nfn main() -> Int:\n    let n = 79\n    let out = forward(&n)\n    (*out) - n + 79\n",
        79,
    );
}

/// The carrier itself IS affine: naming it twice is refused. This is the
/// counterpart to the root staying live - `own` holds the binding, not the
/// payload - and the refusal must not depend on the result annotation.
#[test]
fn a_consumed_view_carrier_cannot_be_named_again_with_or_without_an_annotation() {
    let body = "fn main() -> Int:\n    let n = 79\n    let view = &n\n    let a = forward(view)\n    let b = forward(view)\n    (*a) + (*b) - 79\n";
    let annotated = refusal(&format!("fn forward(own value: &Int) -> &Int:\n    value\n\n{body}"));
    let inferred = refusal(&format!("fn forward(own value: &Int):\n    value\n\n{body}"));
    for error in [&annotated, &inferred] {
        assert!(
            error.contains("use of `view` after it was moved (consumed by an `own` parameter)"),
            "the refusal must name the consumed carrier: {error}"
        );
    }
    assert_eq!(
        annotated, inferred,
        "a missing result annotation cannot change what the call consumes"
    );
}
