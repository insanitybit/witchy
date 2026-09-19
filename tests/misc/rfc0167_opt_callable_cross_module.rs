//! RFC-0167 exported opt-callable contracts across separately parsed source modules.
//!
//! These are deliberately linked-source tests.  Positive rows use the
//! RFC-0167 triad: independent output, canonical SafeWIR interpretation, and
//! admitted unoptimized Wasm.  The source tree-walker interpreter is not an
//! acceptance oracle for this export-signature boundary.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

struct OptReset;

impl Drop for OptReset {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn check_bundle(
    sources: Vec<(&str, &str)>,
) -> Result<witchy_types::pipeline::CheckedModule, witchy_types::pipeline::PipelineError> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        sources
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect(),
        "main",
        witchy_interp::comptime::expand_compile_time,
    )
}

fn assert_admitted_bundle(
    label: &str,
    sources: Vec<(&str, &str)>,
    expected: &[&str],
) {
    let _reset = OptReset;
    opt::set_for_tests(Some(OptSet::none()));
    assert_eq!(
        opt::active_schema_key().enabled_mask(),
        0,
        "{label} uses the unoptimized Wasm baseline",
    );

    let checked = check_bundle(sources)
        .unwrap_or_else(|error| panic!("{label} linked source checks: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("linked callable entry");
            let function = FunctionId::new(
                entry
                    .export()
                    .strip_prefix('f')
                    .expect("numeric entry export")
                    .parse()
                    .expect("numeric entry function"),
            );
            let arguments = entry
                .capabilities()
                .iter()
                .copied()
                .map(InterpreterValue::entry_capability)
                .collect();
            interpret_program(validated, function, arguments, Default::default())
                .unwrap_or_else(|error| panic!("{label} canonical SafeWIR execution: {error}"))
        },
    )
    .expect_lowered(&format!("{label} admits through canonical SafeWIR"));

    assert_eq!(
        oracle.output, expected,
        "{label} canonical SafeWIR matches the independent expectation",
    );
    assert_eq!(
        oracle.leaked_objects, 0,
        "{label} canonical SafeWIR releases retained roots",
    );

    let mut runtime = Runtime::batch().expect("create compiled runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    vm.run()
        .unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(
        vm.output(), expected,
        "{label} admitted unoptimized Wasm matches the independent expectation",
    );
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0),
        "{label} admitted Wasm releases retained roots",
    );
}

const WORKER: &str = r#"mode opt

pub fn increment(value: Int) -> Int:
    value + 1

pub fn relay(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    callback

type Packet:
    value: Int

trait Apply:
    fn apply(self, callback: fn(Int) -> Int) -> Int

impl Apply for Packet:
    fn apply(self, callback: fn(Int) -> Int) -> Int:
        callback(self.value)

pub fn apply(packet: Packet, callback: fn(Int) -> Int) -> Int:
    packet.apply(callback)
"#;

const API: &str = r#"mode opt

import worker

pub fn nested(adapter: fn(fn(Int) -> Int) -> fn(Int) -> Int, value: Int) -> Int:
    let returned = adapter(worker.increment)
    let alias: fn(Int) -> Int = returned
    alias(value)

pub fn generic_identity(value: a) -> a:
    value
"#;

#[test]
fn linked_opt_exports_retain_callable_contracts_across_direct_return_alias_and_eta_edges() {
    let main = r#"import api
import worker

fn main(console: Console):
    let direct = worker.increment
    let returned = worker.relay(direct)
    console.print("${api.nested(worker.relay, returned(40))}")
"#;

    assert_admitted_bundle(
        "direct-return-alias callable contract",
        vec![("worker", WORKER), ("api", API), ("main", main)],
        &["42"],
    );
}

#[test]
fn linked_opt_trait_method_requires_an_opt_compatible_callback() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let packet = worker.Packet(value: 41)
    console.print("${worker.apply(packet, ordinary)}")
"#;

    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("normal callbacks must not enter an opt trait-method contract");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

#[test]
fn linked_opt_trait_method_accepts_an_exported_opt_callback() {
    let main = r#"import worker

fn main(console: Console):
    let packet = worker.Packet(value: 41)
    console.print("${worker.apply(packet, worker.increment)}")
"#;

    assert_admitted_bundle(
        "exported opt trait callback",
        vec![("worker", WORKER), ("main", main)],
        &["42"],
    );
}

#[test]
fn linked_opt_trait_method_dispatch_itself_requires_an_opt_compatible_callback() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let packet = worker.Packet(value: 41)
    console.print("${packet.apply(ordinary)}")
"#;

    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("normal callbacks must not enter an imported opt trait method directly");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

#[test]
fn linked_opt_trait_method_dispatch_accepts_an_exported_opt_callback() {
    let main = r#"import worker

fn main(console: Console):
    let packet = worker.Packet(value: 41)
    console.print("${packet.apply(worker.increment)}")
"#;

    assert_admitted_bundle(
        "direct imported opt trait dispatch",
        vec![("worker", WORKER), ("main", main)],
        &["42"],
    );
}

#[test]
fn linked_opt_generic_cannot_erase_a_returned_higher_order_contract() {
    let main = r#"import api
import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let erased = api.generic_identity(worker.relay)
    console.print("${erased(ordinary)(1)}")
"#;

    let error = check_bundle(vec![("worker", WORKER), ("api", API), ("main", main)])
        .expect_err("generic transport cannot erase an imported opt higher-order contract");
    assert!(error.to_string().contains("cannot erase an opt-compatible callable"), "{error}");
}

#[test]
fn normal_cross_module_trampoline_cannot_erase_an_opt_callback_precondition() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn trampoline(
    relay: fn(fn(Int) -> Int) -> fn(Int) -> Int,
    callback: fn(Int) -> Int,
) -> Int:
    relay(callback)(41)

fn main(console: Console):
    console.print("${trampoline(worker.relay, ordinary)}")
"#;

    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("a normal callable slot cannot erase an imported opt HOF precondition");
    assert!(error.to_string().contains("cannot erase an opt-compatible callable"), "{error}");
}
