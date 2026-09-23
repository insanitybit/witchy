//! BUG-647: a view or closure over a `var` List keeps its snapshot when the
//! List is mutated inside a loop. Forming an element or slice view (or a
//! closure) retains the root in an owner object; the loop must then carry the
//! List as a shared lane that a push copies, not claim it unique.
//!
//! Independent expectation, canonical SafeWIR interpreter, admitted
//! unoptimized Wasm, zero-leak audit on both.

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
fn a_slice_view_keeps_its_snapshot_across_a_mutating_loop() {
    // Three pushes of 10, 20, 30: length 6, xs[5] = 30, and the view still
    // sums the original 1 + 2 + 3.
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var xs = [1, 2, 3]\n    let s = &xs[0..3]\n    var i = 0\n    while i < s.length():\n        xs.push(s[i] * 10)\n        i = i + 1\n    xs.length() * 1000 + xs[5] * 10 + s[0] + s[1] + s[2]\n",
        6306,
    );
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var xs = [1, 2, 3]\n    let s = &xs[0..3]\n    for i in 0..3:\n        xs.push(s[i] * 10)\n    xs.length() * 1000 + xs[5] * 10 + s.length()\n",
        6303,
    );
}

#[test]
fn an_element_view_keeps_its_snapshot_across_a_mutating_loop() {
    // xs[0] is replaced each iteration; the view still reads the original 3.
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var xs = [1, 2, 3]\n    let v = &xs[2]\n    for i in 0..3:\n        xs.push((*v) * 10)\n        xs[2] = i\n    xs.length() * 1000 + xs[2] * 100 + (*v)\n",
        6203,
    );
}

#[test]
fn a_closure_keeps_its_snapshot_across_a_mutating_loop() {
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var xs = [1, 2, 3]\n    let f = fn() -> Int: xs.length()\n    for i in 0..3:\n        xs.push(i)\n    xs.length() * 10 + f()\n",
        63,
    );
}

#[test]
fn a_captured_string_is_read_and_rebound_inside_a_loop() {
    // The loop reads 2, 3, 4 (sum 9) while rebinding `s`; the closure keeps "ab".
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var s = \"ab\"\n    let f = fn() -> Int: s.length()\n    var n = 0\n    for i in 0..3:\n        n = n + s.length()\n        s = s + \"c\"\n    n * 10 + f()\n",
        92,
    );
}
