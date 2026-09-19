//! RFC-0167 opt higher-order contracts through the production source linker.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

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

fn assert_canonical_execution(
    checked: &witchy_types::pipeline::CheckedModule,
    expected: &[&str],
) {
    let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("linked opt-callable fixture lowers canonically");
    let entry = artifact.entry().expect("canonical linked entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(checked)
        .expect("linked opt-callable fixture validates for the canonical oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        arguments,
        Default::default(),
    )
    .expect("canonical interpreter executes linked opt-callable fixture");
    assert_eq!(oracle.output, expected, "canonical interpreter matches the independent expectation");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter heap audit");

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn admitted linked opt-callable fixture");
    vm.run().expect("run admitted linked opt-callable fixture");
    assert_eq!(vm.output(), expected, "admitted Wasm matches the independent expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

const WORKER: &str = r#"mode opt

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn relay(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    callback
"#;

const API: &str = r#"mode opt

import worker

pub fn apply(adapter: fn(fn(Int) -> Int) -> fn(Int) -> Int, value: Int) -> Int:
    let returned = adapter(worker.plus_one)
    let alias: fn(Int) -> Int = returned
    alias(value)
"#;

const GENERIC: &str = r#"mode opt

pub fn identity(value: a) -> a:
    value
"#;

#[test]
fn normal_callers_may_receive_and_call_an_opt_function_through_its_ordinary_abi() {
    let main = r#"import worker

fn main(console: Console):
    let callback = worker.plus_one
    console.print("${callback(41)}")
"#;
    let checked = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect("normal code may receive and invoke a reference-free opt export");
    assert_canonical_execution(&checked, &["42"]);
}

#[test]
fn normal_higher_order_code_may_receive_an_opt_leaf_callable() {
    let main = r#"import worker

fn apply(callback: fn(Int) -> Int, value: Int) -> Int:
    callback(value)

fn main(console: Console):
    console.print("${apply(worker.plus_one, 41)}")
"#;
    let checked = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect("a reference-free opt leaf remains callable through a normal HOF ABI");
    assert_canonical_execution(&checked, &["42"]);
}

#[test]
fn opt_generic_identity_accepts_non_callable_values() {
    let main = r#"import generic

fn main(console: Console):
    console.print("${generic.identity(41)}")
"#;
    let checked = check_bundle(vec![("generic", GENERIC), ("main", main)])
        .expect("non-callable generic instantiation preserves the opt contract");
    assert_canonical_execution(&checked, &["41"]);
}

#[test]
fn opt_callable_provenance_survives_return_alias_and_nested_higher_order_application() {
    let main = r#"import api
import worker

fn main(console: Console):
    console.print("${api.apply(worker.relay, 41)}")
"#;
    let checked = check_bundle(vec![("worker", WORKER), ("api", API), ("main", main)])
        .expect("opt declaration contracts carry nested callable provenance across modules");
    assert_canonical_execution(&checked, &["42"]);
}

#[test]
fn normal_callable_cannot_enter_opt_higher_order_parameter() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    console.print("${worker.relay(ordinary)(41)}")
"#;
    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("normal function cannot satisfy opt callback precondition");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

#[test]
fn normal_nested_callable_cannot_enter_opt_adapter_parameter() {
    let main = r#"import api

fn ordinary(value: Int) -> Int:
    value + 1

fn normal_adapter(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    ordinary

fn main(console: Console):
    console.print("${api.apply(normal_adapter, 41)}")
"#;
    let error = check_bundle(vec![("worker", WORKER), ("api", API), ("main", main)])
        .expect_err("normal adapter cannot erase nested opt callable contract");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

#[test]
fn normal_trampoline_cannot_erase_an_opt_higher_order_precondition() {
    let api = r#"mode opt

pub fn apply_direct(callback: fn(Int) -> Int, value: Int) -> Int:
    callback(value)
"#;
    let main = r#"import api

fn ordinary(value: Int) -> Int:
    value + 1

fn erase(apply: fn(fn(Int) -> Int, Int) -> Int, callback: fn(Int) -> Int) -> Int:
    apply(callback, 1)

fn main(console: Console):
    console.print("${erase(api.apply_direct, ordinary)}")
"#;
    let error = check_bundle(vec![("api", api), ("main", main)])
        .expect_err("ordinary trampoline must not erase an opt HOF precondition");
    assert!(error.to_string().contains("cannot erase an opt-compatible callable"), "{error}");
}

#[test]
fn opt_generic_identity_cannot_erase_a_higher_order_contract() {
    let main = r#"import generic
import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let erased = generic.identity(worker.relay)
    console.print("${erased(ordinary)(1)}")
"#;
    let error = check_bundle(vec![("generic", GENERIC), ("worker", WORKER), ("main", main)])
        .expect_err("opt generic identity must not erase a higher-order callable contract");
    assert!(error.to_string().contains("cannot erase an opt-compatible callable"), "{error}");
}

#[test]
fn nominal_callable_field_cannot_erase_an_opt_higher_order_precondition() {
    let main = r#"import worker

type Holder:
    apply: fn(fn(Int) -> Int) -> fn(Int) -> Int

fn main(console: Console):
    let _ = Holder(worker.relay)
"#;
    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("nominal field must not erase an opt HOF contract");
    assert!(error.to_string().contains("cannot erase an opt-compatible callable"), "{error}");
}
