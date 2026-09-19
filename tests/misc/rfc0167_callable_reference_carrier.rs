//! RFC-0167 callable rooted-view admission matrix.
//!
//! Callable `&T` carriers are checked against an independent expected result,
//! the canonical SafeWIR interpreter, and admitted unoptimized Wasm.  The
//! retired source tree walker is intentionally absent from this acceptance
//! path.  The unadmitted payload negative remains fail-closed here.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

/// A local rooted-view callable retains its authenticated root through an
/// indirect call.  The independent oracle is deliberately a plain scalar
/// expectation; canonical SafeWIR then provides the executable reference
/// semantics before the Wasm backend is exercised.
fn assert_rooted_view_callable(source: &str, expected: i64) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("rooted-view callable type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

            let entry = artifact.entry().expect("rooted-view callable entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry
                    .export()
                    .strip_prefix('f')
                    .expect("numeric entry export")
                    .parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry
                .capabilities()
                .iter()
                .copied()
                .map(InterpreterValue::entry_capability)
                .collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes rooted-view callable")
        },
    )
    .expect_lowered("rooted-view callable admits through canonical SafeWIR");

    assert_eq!(
        oracle.results,
        vec![witchy_wir::canonical_interpreter::InterpreterValue::Int(expected)],
        "canonical SafeWIR matches the independent expectation",
    );
    assert!(oracle.output.is_empty(), "fixture has no console side channel");
    assert_eq!(
        oracle.leaked_objects, 0,
        "canonical interpreter releases retained roots",
    );

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .expect("spawn admitted rooted-view callable");
    vm.run().expect("execute admitted rooted-view callable");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "admitted Wasm has no canonical heap leaks",
    );
}

#[test]
fn local_function_value_retains_an_immutable_rooted_view_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(
        opt::active_schema_key().enabled_mask(),
        0,
        "rooted-view callable baseline uses no optional Wasm optimizations",
    );
    assert_rooted_view_callable(
        r#"
fn relay(view: &Int) -> &Int:
    view

fn main() -> Int:
    var value = 7
    let indirect = relay
    let observed = indirect(&value)
    value = 9
    (*observed) * 10 + value
"#,
        79,
    );
}

#[test]
fn rooted_view_callable_rejects_an_unadmitted_payload() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn relay(view: &Float) -> &Float:
    view

fn main() -> Int:
    let indirect = relay
    let value = 7.0
    let observed = indirect(&value)
    0
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("unadmitted payload still type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("payload-erased callable must not cross the canonical boundary");
    assert!(
        refusal
            .to_string()
            .contains("rooted view payload has no admitted authenticated carrier"),
        "boundary must reject the unauthenticated carrier rather than erase it: {refusal}",
    );
}
