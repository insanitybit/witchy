use super::*;

/// The expectation is independent of either backend.  The frozen source
/// walker is deliberately absent: canonical SafeWIR is the executable oracle
/// and admitted Wasm is the production runtime.
fn assert_rooted_slice_length(source: &str, expected: i64) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("rooted slice fixture type-checks on the canonical route");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};
            let entry = artifact.entry().expect("rooted-slice entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export").parse()
                    .expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical interpreter executes rooted-slice fixture")
        },
    ).expect_lowered("rooted slice admits through canonical SafeWIR");
    assert_eq!(oracle.results, vec![witchy_wir::canonical_interpreter::InterpreterValue::Int(expected)]);
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases retained slice root");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print_int: true,
        quiet: true,
        ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted rooted-slice fixture");
    vm.run().expect("execute admitted rooted-slice fixture");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn rooted_int_slice_returns_passes_and_exposes_only_its_projected_length() {
    let source = r#"
fn relay(items: &[Int]) -> &[Int]:
    items

fn main() -> Int:
    let values = [10, 20, 30, 40]
    let middle = relay(&values[1..3])
    middle.length()
"#;
    assert_rooted_slice_length(source, 2);
}

/// The RFC's generic slice-view example must specialize to the same
/// authenticated root-plus-element carrier as its concrete `Int` form.  The
/// expected answer (20) is independent of both executable backends.
#[test]
fn generic_first_returns_a_rooted_element_view_or_none() {
    let source = r#"
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0:
        None
    else:
        Some(&items[0])

fn main() -> Int:
    let values = [10, 20, 30]
    match first(&values[1..3]):
        Some(value) -> *value
        None -> 0
"#;
    assert_rooted_slice_length(source, 20);
}

#[test]
fn generic_first_returns_none_for_an_empty_rooted_slice() {
    let source = r#"
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0:
        None
    else:
        Some(&items[0])

fn main() -> Int:
    let values = [10, 20, 30]
    match first(&values[1..1]):
        Some(_) -> 0
        None -> 1
"#;
    assert_rooted_slice_length(source, 1);
}

/// A generic view result cannot smuggle a region-owned root through its
/// `Option` shell.  This is checked before canonical lowering; the `[Float]`
/// refusal below independently covers the bounded carrier's admission edge.
#[test]
fn generic_first_rejects_a_region_born_root_escape() {
    let source = r#"
fn first[a](items: &[a]) -> Option(&a):
    if items.length() == 0:
        None
    else:
        Some(&items[0])

fn bad() -> Option(&Int):
    region -> Option(&Int):
        let values = [10, 20]
        first(&values[0..1])
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a generic element view retains its region-born list root");
    assert!(
        error.to_string().contains("region aggregate carrying immutable views"),
        "the rejection must identify the region-owned rooted-view escape: {error}",
    );
}

#[test]
fn rooted_slice_rejects_unadmitted_element_payload_at_canonical_boundary() {
    let source = r#"
fn relay(items: &[Float]) -> &[Float]:
    items

fn main() -> Int:
    let values = [1.0, 2.0]
    let middle = relay(&values[0..1])
    middle.length()
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("unadmitted slice payload still type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("only authenticated &[Int] may cross canonical ABI");
    assert!(refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"));
}
