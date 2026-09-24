//! Native structural rendering must terminate preparation, preserve source
//! protocol choices, and execute with balanced ownership on all three legs.

#[test]
fn structural_render_preserves_dictionary_insertion_and_record_field_order() {
    assert_normal_then_opt_refusal(r#"
type Tree:
    Tip
    Branch(String, List(Tree))

type Record:
    zebra: String
    alpha: List(Tree)

fn main(console: Console):
    var trees: Dict(String, Tree) = dict.new()
    trees["z"] = Branch("", [Tip])
    trees["a"] = Tip
    let snapshot = trees
    trees["z"] = Branch("new", [])
    console.print("${snapshot}")
    console.print("${trees}")
    console.print("${(Record("first", [Tip, Branch("last", [])]), snapshot)}")
    let empty: Dict(String, Tree) = dict.new()
    console.print("${empty}")
    console.print("${.{alpha: "", beta: [Tip]}}")
"#, &[
        "{z: Branch(, [Tip]), a: Tip}",
        "{z: Branch(new, []), a: Tip}",
        "(Record(first, [Tip, Branch(last, [])]), {z: Branch(, [Tip]), a: Tip})",
        "{}", ".{alpha: , beta: [Tip]}",
    ]);
}

#[test]
fn structural_render_closes_equality_exposed_dictionary_and_generic_dependencies() {
    assert_both_modes(r#"
import cmp
import dict

type Key derive(Eq):
    id: Int
    cache: Int

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        self.id == other.id

type Envelope:
    Envelope(a)

type Box:
    Box(a)

impl PartialEq for Box(a) where a: PartialEq:
    fn eq(self, other: Self) -> Bool:
        var seen: Dict(Key, Int) = dict.new()
        seen[Key(1, 10)] = 7
        let shown = "${Envelope(seen)}"
        let found = shown == "Envelope({Key(1, 10): 7})" && seen.get_or(Key(1, 99), 0) == 7
        match self:
            Box(left) ->
                match other:
                    Box(right) -> found && left == right

fn main(console: Console):
    console.print("${[Box(1)] == [Box(1)]}")
    console.print("${[Box(1)] == [Box(2)]}")
    console.print("${[Box(1.5)] == [Box(1.5)]}")
    console.print("${[Box(1.5)] == [Box(2.25)]}")
"#, &["true", "false", "true", "false"]);
}

fn assert_both_modes(source: &str, expected: &[&str]) {
    assert_three_legs(source, expected);
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

/// (RFC-0167 §4) In `mode opt` a proven owner that was aliased by an ordinary
/// binding is demoted; a later `unique`-requiring mutation is refused rather
/// than repaired by a hidden copy. The normal-mode leg keeps proving the
/// snapshot semantics; the opt leg pins the refusal.
fn assert_normal_then_opt_refusal(source: &str, expected: &[&str]) {
    assert_three_legs(source, expected);
    let error = witchy::resolve_std_only_checked(&format!("mode opt\n{source}"))
        .expect_err("opt mode refuses mutating an aliased owner")
        .to_string();
    assert!(
        error.contains("cannot repair ordinary storage into a `unique` parameter"),
        "{error}"
    );
}

fn assert_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("render source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("source interpreter renders");
    assert_eq!(source_output, expected, "independent source expectation");

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("checked rendering admits");
    let entry = artifact.entry().expect("source entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export").parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("independent canonical validation");
    let arguments = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, arguments, Default::default(),
    ).expect("canonical interpreter renders");
    assert_eq!(oracle.output, expected, "independent canonical expectation");
    assert_eq!(oracle.leaked_objects, 0, "canonical ownership balance");

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted render instance");
    vm.run().expect("Wasm renders");
    assert_eq!(vm.output(), expected, "independent Wasm expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}
