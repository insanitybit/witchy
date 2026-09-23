//! RFC-0167 row G: `for x in s` iterates a `&[T]` view by value. The loop
//! counts `0..s.length()` and reads each element through the same checked
//! projection as `s[i]`; the carrier is immutable, so it is not loop state.
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
fn an_int_slice_iterates_its_projection() {
    // 2 + 3: only the projected elements, not the backing list.
    assert_canonical_and_wasm(
        "fn total(xs: &[Int]) -> Int:\n    var t = 0\n    for x in xs:\n        t = t + x\n    t\n\nfn main() -> Int:\n    let xs = [1, 2, 3, 4]\n    total(&xs[1..3])\n",
        5,
    );
}

#[test]
fn a_string_slice_iterates_retained_strings() {
    // "cd" + "ef" = "cdef": length 4, and equality adds 1.
    assert_canonical_and_wasm(
        "fn join(xs: &[String]) -> String:\n    var out = \"\"\n    for x in xs:\n        out = out + x\n    out\n\nfn main() -> Int:\n    let words = [\"ab\", \"cd\", \"ef\", \"gh\"]\n    let joined = join(&words[1..3])\n    joined.length() * 10 + (if joined == \"cdef\": 1 else: 0)\n",
        41,
    );
}

#[test]
fn break_and_continue_leave_a_slice_loop() {
    // 1.0 and 2.0 precede the first element above 2.5.
    assert_canonical_and_wasm(
        "fn until_big(xs: &[Float]) -> Int:\n    var n = 0\n    for x in xs:\n        if x > 2.5:\n            break\n        n = n + 1\n    n\n\nfn main() -> Int:\n    let xs = [1.0, 2.0, 3.0, 4.0]\n    until_big(&xs[0..4])\n",
        2,
    );
    // Two of the three Bools are true; `continue` skips the false one.
    assert_canonical_and_wasm(
        "fn trues(xs: &[Bool]) -> Int:\n    var n = 0\n    for b in xs:\n        if !b:\n            continue\n        n = n + 1\n    n\n\nfn main() -> Int:\n    let xs = [true, false, true]\n    trues(&xs[0..3])\n",
        2,
    );
}

#[test]
fn a_generic_slice_loop_and_nested_loops() {
    // 3 Bools, 3 Ints, and an empty slice: 3*100 + 3*10 + 0.
    assert_canonical_and_wasm(
        "fn count[a](xs: &[a]) -> Int:\n    var n = 0\n    for _ in xs:\n        n = n + 1\n    n\n\nfn main() -> Int:\n    let xs = [true, false, true]\n    let ys = [1, 2, 3, 4, 5]\n    count(&xs[0..3]) * 100 + count(&ys[2..5]) * 10 + count(&ys[1..1])\n",
        330,
    );
    // (2 + 3) * (2 + 3).
    assert_canonical_and_wasm(
        "fn pairs(xs: &[Int]) -> Int:\n    var t = 0\n    for a in xs:\n        for b in xs:\n            t = t + a * b\n    t\n\nfn main() -> Int:\n    let xs = [1, 2, 3, 4]\n    pairs(&xs[1..3])\n",
        25,
    );
}
