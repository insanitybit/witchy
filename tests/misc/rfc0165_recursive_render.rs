//! Native structural rendering must terminate preparation, preserve source
//! protocol choices, and execute with balanced ownership on all three legs.

#[test]
fn structural_render_reserves_recursive_int_float_and_mutual_nominal_identities() {
    assert_both_modes(r#"
type Chain:
    End
    Link(a, Chain(a))

type Left:
    StopLeft
    CrossRight(Right)

type Right:
    StopRight
    CrossLeft(Left)

type Envelope:
    first: Chain(Int)
    second: Chain(Float)
    mutual: Left

fn main(console: Console):
    let ints = Link(1, Link(2, End))
    let floats = Link(1.5, Link(2.25, End))
    let mutual = CrossRight(CrossLeft(CrossRight(StopRight)))
    console.print("${Envelope(ints, floats, mutual)}")
    console.print("${[ints, End]}")
    console.print("${[floats, End]}")
    console.print("${(StopLeft, StopRight)}")
"#, &[
        "Envelope(Link(1, Link(2, End)), Link(1.5, Link(2.25, End)), CrossRight(CrossLeft(CrossRight(StopRight))))",
        "[Link(1, Link(2, End)), End]",
        "[Link(1.5, Link(2.25, End)), End]",
        "(StopLeft, StopRight)",
    ]);
}

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
fn structural_render_keeps_native_children_separate_from_resolved_show() {
    assert_both_modes(r#"
type Label:
    text: String

impl Show for Label:
    fn show(self) -> String:
        "<" + self.text + ">"

type Poison:
    Poison

impl Show for Poison:
    fn show(self) -> String:
        fail("native structural children must not call Show")
        "unreachable"

type Wrapped:
    Empty
    Payload(Label)
    Dangerous(Poison)
    Recur(Wrapped)

fn main(console: Console):
    let label = Label("x")
    console.print("${label}")
    console.print("${[label]}")
    console.print(show.render(label))
    console.print("${Recur(Payload(label))}")
    console.print("${[Empty, Dangerous(Poison), Payload(Label(""))]}")
"#, &[
        "<x>", "[<x>]", "<x>", "Recur(Payload(Label(x)))",
        "[Empty, Dangerous(Poison), Payload(Label())]",
    ]);
}

#[test]
fn structural_render_evaluates_operands_once_in_order_and_retains_snapshots() {
    assert_both_modes(r#"
fn observe(var values: List(Int), console: Console, label: String, next: Int) -> List(Int):
    console.print(label)
    list.push(values, next)
    values

fn main(console: Console):
    var values: List(Int) = []
    console.print("${(observe(values, console, "left", 1), observe(values, console, "right", 2))}")
    console.print("${values}")
"#, &["left", "right", "([1], [1, 2])", "[1, 2]"]);
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

#[test]
fn structural_render_closes_authenticated_existential_show_adapter_roots() {
    assert_both_modes(r#"
type Tree:
    Leaf(Int)
    Node(Tree, Tree)

type Display:
    Display(Tree)

impl Show for Display:
    fn show(self) -> String:
        match self:
            Display(tree) -> "${tree}"

fn invoke(value: dyn Show) -> String:
    value.show()

fn main(console: Console):
    let leaf: dyn Show = Display(Leaf(1))
    let tree: dyn Show = Display(Node(Leaf(2), Node(Leaf(3), Leaf(4))))
    console.print(invoke(leaf))
    console.print(invoke(tree))
"#, &["Leaf(1)", "Node(Leaf(2), Node(Leaf(3), Leaf(4)))"]);
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
