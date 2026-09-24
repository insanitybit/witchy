//! RFC-0167 row H: a `var` aggregate written back through a function value.
//!
//! An indirect call that tail-forwards a reallocated `var` result must write
//! back exactly as a direct call does, leaving earlier snapshots intact.
//! Independent expectation, canonical SafeWIR interpreter, admitted Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_canonical_and_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)],
        "canonical interpreter matches the independent expected result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

#[test]
fn a_var_record_is_written_back_through_a_function_value() {
    assert_canonical_and_wasm(r#"
type Acc:
    total: Int
    count: Int

fn add(var acc: Acc, k: Int):
    acc = Acc(total: acc.total + k, count: acc.count + 1)

fn apply(f: fn(var Acc, Int) -> (), var acc: Acc):
    f(acc, 3)
    f(acc, 4)

fn main() -> Int:
    var acc = Acc(total: 0, count: 0)
    apply(add, acc)
    acc.total * 10 + acc.count
"#, 72);
}

#[test]
fn a_var_list_grows_through_a_function_value_and_keeps_its_snapshot() {
    assert_canonical_and_wasm(r#"
fn add(var xs: List(Int), k: Int):
    xs.push(k)

fn apply(f: fn(var List(Int), Int) -> (), var xs: List(Int)):
    f(xs, 3)
    f(xs, 4)

fn main() -> Int:
    var xs = [1]
    let before = xs
    apply(add, xs)
    xs.length() * 10 + before.length()
"#, 31);
}

#[test]
fn a_var_record_holding_a_list_is_written_back_through_a_function_value() {
    assert_canonical_and_wasm(r#"
type Acc:
    total: Int
    items: List(Int)

fn add(var acc: Acc, n: Int):
    var items = acc.items
    items.push(n)
    acc = Acc(total: acc.total + n, items: items)

fn apply(f: fn(var Acc, Int) -> (), var acc: Acc):
    f(acc, 3)
    f(acc, 4)

fn main() -> Int:
    var acc = Acc(total: 0, items: [])
    let before = acc
    apply(add, acc)
    acc.total * 100 + acc.items.length() * 10 + before.items.length()
"#, 720);
}
