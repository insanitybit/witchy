use super::*;

/// RFC-0167 evidence is an independent expected result, then the checked
/// SafeWIR interpreter, then admitted unoptimized Wasm.  The frozen source
/// tree walker deliberately has no role in these cases.
fn assert_rooted_view_transport(source: &str, expected: i64) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("rooted-view fixture type-checks on the canonical route");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};
            let entry = artifact.entry().expect("rooted-view entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export").parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes rooted-view fixture")
        },
    ).expect_lowered("rooted-view fixture admits through canonical SafeWIR");
    assert_eq!(
        oracle.results,
        vec![witchy_wir::canonical_interpreter::InterpreterValue::Int(expected)],
        "canonical SafeWIR matches the independent expectation",
    );
    assert!(oracle.output.is_empty(), "the fixture has no console side channel");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases every retained root");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print_int: true,
        quiet: true,
        ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted rooted-view fixture");
    vm.run().expect("execute admitted rooted-view fixture");
    assert_eq!(
        vm.output(),
        vec![expected.to_string()],
        "admitted unoptimized Wasm matches the independent expectation",
    );
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

/// Cross-mode rooted views are linked source contracts, not source-interpreter
/// behavior.  The independent expected result must agree with the checked
/// SafeWIR oracle and admitted unoptimized Wasm, both with no retained roots.
fn assert_linked_rooted_view_transport(
    sources: &[(&str, &str)],
    entry: &str,
    expected: i64,
) {
    let modules = sources
        .iter()
        .map(|(name, source)| ((*name).to_string(), source.as_bytes().to_vec()))
        .collect();
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        modules,
        entry,
        crate::comptime::expand_compile_time,
    )
        .expect("linked rooted-view source modules type-check on the canonical route");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};
            let entry = artifact.entry().expect("linked rooted-view entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export").parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes linked rooted-view fixture")
        },
    ).expect_lowered("linked rooted-view fixture admits through canonical SafeWIR");
    assert_eq!(
        oracle.results,
        vec![witchy_wir::canonical_interpreter::InterpreterValue::Int(expected)],
        "canonical SafeWIR matches the independent expectation",
    );
    assert!(oracle.output.is_empty(), "the fixture has no console side channel");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases every retained root");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print_int: true,
        quiet: true,
        ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted linked rooted-view fixture");
    vm.run().expect("execute admitted linked rooted-view fixture");
    assert_eq!(
        vm.output(),
        vec![expected.to_string()],
        "admitted unoptimized Wasm matches the independent expectation",
    );
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn rooted_view_direct_return_retains_its_snapshot_across_var_mutation() {
    let source = r#"
fn relay(view: &Int) -> &Int:
    view

fn main() -> Int:
    var value = 7
    let observed = relay(&value)
    value = 9
    (*observed) * 10 + value
"#;
    assert_rooted_view_transport(source, 79);
}

#[test]
fn rooted_view_nominal_field_retains_its_snapshot_across_store_and_return() {
    let source = r#"
mode opt

type Holder:
    value: &Int

fn store(view: &Int) -> Holder:
    Holder(view)

fn main() -> Int:
    var value = 7
    let holder = store(&value)
    value = 9
    (*holder.value) * 10 + value
"#;
    assert_rooted_view_transport(source, 79);
}

#[test]
fn rooted_view_option_some_none_match_preserves_its_authenticated_owner() {
    let source = r#"
fn select(present: Bool, value: &Int) -> Option(&Int):
    if present: Some(value) else: None

fn read(option: Option(&Int)) -> Int:
    match option:
        Some(view) -> *view
        None -> 0

fn main() -> Int:
    let value = 79
    let some = select(true, &value)
    let none = select(false, &value)
    read(some) * 10 + read(none)
"#;
    assert_rooted_view_transport(source, 790);
}

#[test]
fn normal_source_calls_an_opt_rooted_view_relay_without_erasing_its_snapshot() {
    let worker = r#"
mode opt

pub fn relay(view: &Int) -> &Int:
    view
"#;
    let api = r#"
mode opt

import worker

pub fn relay(view: &Int) -> &Int:
    worker.relay(view)
"#;
    let app = r#"
import api

fn main() -> Int:
    var value = 7
    let observed = api.relay(&value)
    value = 9
    (*observed) * 10 + value
"#;
    assert_linked_rooted_view_transport(&[("worker", worker), ("api", api), ("app", app)], "app", 79);
}

#[test]
fn normal_source_does_not_erase_an_unadmitted_opt_aggregate_view_carrier() {
    let api = r#"
mode opt

pub fn relay(view: &List(List(Int))) -> &List(List(Int)):
    view
"#;
    let app = r#"
import api

fn main() -> Int:
    var values = [[7]]
    let observed = api.relay(&values)
    values = [[9]]
    (*observed).length()
"#;
    let modules = [("api", api), ("app", app)]
        .into_iter()
        .map(|(name, source)| (name.to_string(), source.as_bytes().to_vec()))
        .collect();
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        modules,
        "app",
        crate::comptime::expand_compile_time,
    )
        .expect("linker preserves the unadmitted aggregate rooted-view type");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("canonical ABI must reject unadmitted aggregate rooted-view carriers");
    assert!(
        refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"),
        "canonical lowering must fail closed rather than erase the carrier: {refusal}",
    );
}
