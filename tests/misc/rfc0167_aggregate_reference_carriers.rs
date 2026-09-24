//! RFC-0167 structural rooted-view aggregate carriers.

const IMMUTABLE_AGGREGATE_VIEW: &str = r#"
mode opt

type Pair:
    left: &Int
    right: &Int

fn pair(left: &Int, right: &Int) -> Pair:
    Pair(left: left, right: right)

fn tuple(left: &Int, right: &Int) -> (&Int, &Int):
    (left, right)

fn read_tuple(value: (&Int, &Int)) -> Int:
    match value:
        (view, _) -> *view

fn main() -> Int:
    var left = 4
    var right = 9
    let named = pair(&left, &right)
    (*named.left) * 100 + (*named.right) * 10 + read_tuple(tuple(&left, &right))
"#;

#[test]
fn nominal_and_tuple_immutable_views_execute_through_exact_owner_families() {
    let checked = witchy::resolve_std_only_checked(IMMUTABLE_AGGREGATE_VIEW)
        .expect("aggregate rooted-view source type-checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nominal and tuple carriers must preserve rooted payload identities");
    let entry = artifact.entry().expect("aggregate carrier entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("numeric entry export").parse().expect("numeric entry function"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("aggregate carrier SafeWIR validates");
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        Vec::new(),
        Default::default(),
    )
    .expect("canonical interpreter executes aggregate carriers");
    assert!(
        matches!(oracle.results.as_slice(), [witchy_wir::canonical_interpreter::InterpreterValue::Int(494)]),
        "canonical interpreter reads each retained aggregate field",
    );
    assert_eq!(oracle.leaked_objects, 0, "aggregate carriers release their roots");

    let mut runtime = witchy::runtime::Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, witchy::runtime::Capabilities::none(), 128)
        .expect("spawn admitted aggregate carriers");
    vm.run().expect("execute admitted aggregate carriers");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn aggregate_immutable_views_reject_region_owned_roots_during_checking() {
    let source = r#"
type Held:
    Held(&Int)

fn bad() -> Held:
    region:
        let local = 7
        Held(&local)
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("an aggregate view may not smuggle a region-owned root");
    assert!(
        error.to_string().contains("per-field parent-root provenance")
            || error.to_string().contains("region-born view"),
        "region-owned aggregate rejection should retain a fail-closed diagnostic: {error}"
    );
}
