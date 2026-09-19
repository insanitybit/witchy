use super::*;

/// A rooted view callable is proved against an independent expectation, the
/// canonical SafeWIR interpreter, and admitted unoptimized Wasm.  The frozen
/// source tree walker is intentionally absent from this acceptance path.
fn assert_local_rooted_view_callable(source: &str, expected: i64) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("local rooted-view callable type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};
            let entry = artifact.entry().expect("rooted-view callable entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export").parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes local rooted-view callable")
        },
    ).expect_lowered("local rooted-view callable admits through canonical SafeWIR");
    assert_eq!(
        oracle.results,
        vec![witchy_wir::canonical_interpreter::InterpreterValue::Int(expected)],
        "canonical SafeWIR matches the independent expectation",
    );
    assert!(oracle.output.is_empty(), "fixture has no console side channel");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases retained roots");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print_int: true,
        quiet: true,
        ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted local rooted-view callable");
    vm.run().expect("execute admitted local rooted-view callable");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn local_function_value_retains_a_rooted_view_snapshot() {
    let source = r#"
fn relay(view: &Int) -> &Int:
    view

fn main() -> Int:
    var value = 7
    let indirect = relay
    let observed = indirect(&value)
    value = 9
    (*observed) * 10 + value
"#;
    assert_local_rooted_view_callable(source, 79);
}

#[test]
fn local_function_value_rejects_an_unadmitted_rooted_payload() {
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
        refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"),
        "the boundary must reject the unauthenticated carrier rather than erase it: {refusal}",
    );
}

#[test]
fn generic_element_view_capture_reads_its_rooted_snapshot() {
    let source = r#"
fn first[a](items: List(a)) -> &a:
    &items[0]

fn main() -> Int:
    var values = [7, 8]
    let selected: &Int = first(values)
    let read = fn() -> Int:
        *selected
    values = [9, 10]
    read() * 10 + values[0]
"#;
    assert_local_rooted_view_callable(source, 79);
}
