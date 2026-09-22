//! BUG-643: a `var` List parameter mutated inside a loop.
//!
//! The loop entered with the caller's shared token and left each iteration
//! with a fresh replacement, and the checker refused to join the two. The
//! loop now enters with an owned copy, as `var ys = xs` does.
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
fn a_var_list_parameter_grows_in_each_loop_form() {
    assert_canonical_and_wasm(r#"
fn by_range(var xs: List(Int)):
    for i in 0..4:
        xs.push(i)

fn by_while(var xs: List(Int)):
    var i = 0
    while i < 3:
        xs.push(i)
        i = i + 1

fn by_list(var xs: List(Int), extra: List(Int)):
    for x in extra:
        xs.push(x * 10)

fn main() -> Int:
    var a = [7]
    by_range(a)
    var b = [7]
    by_while(b)
    var c = [7]
    by_list(c, [1, 2])
    a.length() * 100 + b.length() * 10 + c[2]
"#, 560);
}

#[test]
fn the_callers_earlier_copy_is_unchanged_by_the_loop() {
    assert_canonical_and_wasm(r#"
fn grow(var xs: List(Int)):
    for i in 0..4:
        xs[0] = xs[0] + 1
        xs.push(i)

fn main() -> Int:
    var xs = [10]
    let before = xs
    grow(xs)
    before.length() * 1000 + before[0] * 10 + xs[0] - xs.length()
"#, 1109);
}
