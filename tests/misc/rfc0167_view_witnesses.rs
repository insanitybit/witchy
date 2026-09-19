//! Focused RFC-0167 rooted-view witnesses at the canonical execution boundary.
//!
//! These fixtures use an independent scalar expectation, the canonical SafeWIR
//! interpreter, and admitted unoptimized Wasm. They deliberately do not invoke
//! the source interpreter or optional optimizers.

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

fn assert_canonical_and_wasm_79(source: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
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
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(
        oracle.results,
        vec![InterpreterValue::Int(79)],
        "canonical interpreter matches the independent expected result",
    );
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

    let mut runtime = Runtime::batch().expect("runtime");
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
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["79"], "admitted Wasm matches the expectation");
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "admitted Wasm releases all roots",
    );
}

#[test]
fn trait_mediated_rooted_int_view_returns_the_authenticated_snapshot() {
    assert_canonical_and_wasm_79(
        r#"
trait Reader:
    fn project(self: Self, value: &Int) -> &Int

type Identity:
    Identity

impl Reader for Identity:
    fn project(self: Identity, value: &Int) -> &Int:
        value

fn apply[a](reader: a, value: &Int) -> &Int where a: Reader:
    reader.project(value)

fn main() -> Int:
    var value = 7
    let observed = apply(Identity, &value)
    value = 9
    (*observed) * 10 + value
"#,
    );
}

#[test]
fn direct_var_nominal_view_aggregate_replacement_preserves_old_snapshot() {
    assert_canonical_and_wasm_79(
        r#"
type Holder:
    value: &Int

fn main() -> Int:
    var first = 7
    var second = 9
    var holder = Holder(&first)
    let old = holder.value
    holder = Holder(&second)
    (*old) * 10 + (*holder.value)
"#,
    );
}

#[test]
fn opt_import_of_arbitrary_normal_module_rejects_at_transitive_mode_boundary() {
    let normal = r#"
pub fn relay(view: &Int) -> &Int:
    view
"#;
    let opt_entry = r#"
mode opt

import normal

fn main() -> Int:
    var value = 7
    let observed = normal.relay(&value)
    value = 9
    (*observed) * 10 + value
"#;
    let error = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("normal".to_owned(), normal.as_bytes().to_vec()),
            ("app".to_owned(), opt_entry.as_bytes().to_vec()),
        ],
        "app",
        witchy_interp::comptime::expand_compile_time,
    )
    .expect_err("opt caller must reject an arbitrary normal dependency");
    let message = error.to_string();
    assert!(
        message.contains("not `mode opt`") || message.contains("optimization mode is transitive"),
        "the rejection must identify the transitive mode boundary: {message}",
    );
}
