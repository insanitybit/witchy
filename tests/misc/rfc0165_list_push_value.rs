//! Public list mutators must retain the value semantics of their append result.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn both_modes(source: &str, expected: &[&str]) {
    for source in [source.to_owned(), format!("mode opt\n{source}")] {
        let checked = witchy::resolve_std_only_checked(&source).expect("list push source checks");
        let output = witchy::interpreter::run_module_args(
            checked.module().clone(), ".", Vec::new(), Vec::new(),
        ).expect("source list push executes");
        assert_eq!(output, expected, "independent source expectation");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("list push source admits");
        let entry = artifact.entry().expect("source entry");
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked)
            .expect("independent canonical validation");
        let arguments = entry.capabilities().iter().copied()
            .map(InterpreterValue::entry_capability).collect();
        let oracle = interpret_program(&validated, function, arguments, InterpreterConfig {
            max_steps: 20_000_000_000, ..Default::default()
        }).expect("canonical list push executes");
        assert_eq!(oracle.output, expected, "independent canonical expectation");
        assert_eq!(oracle.leaked_objects, 0, "canonical ownership balance");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted list push instance");
        vm.run().expect("Wasm list push executes");
        assert_eq!(vm.output(), expected, "independent Wasm expectation");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
    }
}

#[test]
fn list_push_value_scalar_field_snapshots_and_public_call_forms() {
    both_modes(r#"
import list

type Buf:
    items: List(Int)

fn appended(buf: Buf, value: Int) -> Buf:
    var copy = buf
    copy.items.push(value)
    copy

fn main(console: Console):
    var b = Buf([1])
    let snapshot = b
    b.items.push(2)
    list.push(b.items, 3)
    let result = appended(b, 4)
    console.print("${snapshot.items}")
    console.print("${b.items}")
    console.print("${result.items}")
    var xs = [5]
    xs.push(6)
    list.push(xs, 7)
    console.print("${xs}")
"#, &["[1]", "[1, 2, 3]", "[1, 2, 3, 4]", "[5, 6, 7]"]);
}

#[test]
fn list_push_value_string_field_snapshots_and_retained_child() {
    both_modes(r#"
import list

type Buf:
    items: List(String)

fn text(value: String) -> String:
    value + "!"

fn appended(buf: Buf, value: String) -> Buf:
    var copy = buf
    list.push(copy.items, value)
    copy

fn main(console: Console):
    var b = Buf([text("old")])
    let snapshot = b
    let retained = b.items[0]
    b.items.push(retained)
    let middle = b
    list.push(b.items, text("new"))
    let result = appended(b, text("tail"))
    console.print(snapshot.items.join("|"))
    console.print(middle.items.join("|"))
    console.print(b.items.join("|"))
    console.print(result.items.join("|"))
    console.print(retained)
"#, &["old!", "old!|old!", "old!|old!|new!", "old!|old!|new!|tail!", "old!"]);
}

#[test]
fn list_push_value_evaluates_receiver_then_element_once_without_hidden_writeback() {
    both_modes(r#"
type Buf:
    items: List(String)

fn receiver(buf: Buf, console: Console) -> Buf:
    console.print("receiver")
    buf

fn element(var buf: Buf, console: Console) -> String:
    console.print("element")
    buf = Buf(["changed"])
    "tail"

fn appended(buf: Buf, value: String) -> Buf:
    var copy = buf
    copy.items.push(value)
    copy

fn main(console: Console):
    var b = Buf(["old"])
    let snapshot = b
    let result = appended(receiver(b, console), element(b, console))
    console.print(result.items.join("|"))
    console.print(b.items.join("|"))
    console.print(snapshot.items.join("|"))
    let independent = appended(b, "again")
    console.print(b.items.join("|"))
    console.print(independent.items.join("|"))
"#, &["receiver", "element", "old|tail", "changed", "old", "changed", "changed|again"]);
}

#[test]
fn list_push_value_direct_element_effect_preserves_sibling_writeback_and_snapshot() {
    both_modes(
        r#"
import list

type Buf:
    items: List(String)
    label: String

fn element(var label: String, console: Console) -> String:
    console.print("element:${label}")
    label = label + "!"
    "tail:" + label

fn appended(buf: Buf, console: Console) -> Buf:
    var copy = buf
    copy.items.push(element(copy.label, console))
    list.push(copy.items, element(copy.label, console))
    copy

fn main(console: Console):
    let b = Buf(["old"], "start")
    let snapshot = b
    let result = appended(b, console)
    console.print(result.items.join("|"))
    console.print(result.label)
    console.print(b.items.join("|"))
    console.print(b.label)
    console.print(snapshot.items.join("|"))
    console.print(snapshot.label)
"#,
        &[
            "element:start",
            "element:start!",
            "old|tail:start!|tail:start!!",
            "start!!",
            "old",
            "start",
            "old",
            "start",
        ],
    );
}

#[test]
fn list_push_value_rejects_later_argument_replacing_reserved_receiver_root() {
    let source = r#"
type Buf:
    items: List(String)

fn replace(var buf: Buf) -> String:
    buf = Buf(["changed"])
    "tail"

fn main(console: Console):
    var b = Buf(["old"])
    b.items.push(replace(b))
    console.print(b.items.join("|"))
"#;
    for source in [source.to_owned(), format!("mode opt\n{source}")] {
        let error = witchy::resolve_std_only_checked(&source)
            .err()
            .expect("later argument must not replace the reserved receiver root");
        let message = error.to_string();
        assert!(message.contains("reserves `var` place rooted in `b`"), "{message}");
        assert!(message.contains("later argument 2 writes back to an overlapping place"), "{message}");
    }
}
