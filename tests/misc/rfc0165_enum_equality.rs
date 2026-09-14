//! Enum equality observes source discriminants, never allocation addresses.
use crate::temp_dir::TempDir;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, InterpreterConfig, InterpreterValue,
};
use witchy_wir::canonical_safewir::FunctionId;

fn agree(source: &str, expected: &[String]) {
    let checked = witchy::resolve_std_only_checked(source).expect("enum source checks");
    let root = TempDir::new("canonical-enum-equality");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().unwrap();
            let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
            let arguments = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            let result = interpret_entry_program(validated, function, arguments, &[], InterpreterConfig::default()).unwrap();
            assert_eq!(result.output, expected);
            assert_eq!(result.leaked_objects, 0);
        },
    ).expect_lowered("source-backed enum equality admits");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Capabilities::none()
    }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn enum_equality_respects_custom_partial_eq_dispatch() {
    let source = r#"type State:
    First
    Second
impl PartialEq for State:
    fn eq(self, other: Self) -> Bool:
        true
    fn ne(self, other: Self) -> Bool:
        false
fn main(console: Console):
    console.print("${First == Second} ${First != Second}")
"#;
    agree(source, &["true false".into()]);
}

#[test]
fn enum_equality_all_discriminant_pairs_agree() {
    let source = r#"type State:
    Staged
    Released
    Yanked
    Unknown
fn identity(value: State) -> State:
    value
fn main(console: Console):
    for left in [Staged, Released, Yanked, Unknown]:
        for right in [Staged, Released, Yanked, Unknown]:
            console.print("${identity(left) == identity(right)} ${left != right}")
"#;
    let expected = (0..4).flat_map(|left| (0..4).map(move |right|
        format!("{} {}", left == right, left != right))).collect::<Vec<_>>();
    agree(source, &expected);
}

#[test]
fn enum_equality_snapshots_before_rhs_writeback_and_evaluates_once() {
    let source = r#"type State:
    Staged
    Released
fn advance(var state: State, console: Console) -> State:
    console.print("advance")
    state = Released
    Released
fn observe(console: Console, label: String, state: State) -> State:
    console.print(label)
    state
fn main(console: Console):
    var state = Staged
    let equal = state == advance(state, console)
    console.print("${equal}")
    console.print("${state == Released}")
    console.print("${observe(console, "left", Staged) != observe(console, "right", Released)}")
"#;
    agree(source, &["advance", "false", "true", "left", "right", "true"].map(String::from));
}

#[test]
fn enum_equality_never_ignores_payloads_or_plain_record_fields() {
    // These representations may still refuse until structural equality covers
    // them. Any accepted result must distinguish payloads; tag-only acceptance
    // is a semantic bug. This does not pin future correct support to refusal.
    for source in [
        "type Payload:\n    Value(Int)\n    Empty\nfn main(console: Console):\n    console.print(\"${Value(1) == Value(2)}\")\n",
        "type Record:\n    value: Int\nfn main(console: Console):\n    console.print(\"${Record(1) == Record(2)}\")\n",
    ] {
        let checked = witchy::resolve_std_only_checked(source).expect("payload comparison source checks");
        let mut observed = false;
        let outcome = witchy::codegen::compile_checked_module_artifact_with_observer(
            &checked, |_, _| { observed = true; },
        );
        match outcome {
            witchy::codegen::LoweringOutcome::Unsupported(_) => assert!(!observed),
            witchy::codegen::LoweringOutcome::Lowered(_) => agree(source, &["false".into()]),
            witchy::codegen::LoweringOutcome::Rejected(error) => {
                panic!("unexpected payload equality rejection: {error}");
            }
        }
    }
}
