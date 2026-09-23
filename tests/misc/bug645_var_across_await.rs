//! BUG-645: a `var` local composes with suspension in an async function.
//!
//! Async lowering carries each live local into the continuation segment as a
//! parameter. Two defects broke `var` locals there: the parameter had no type,
//! so a method on a carried `List` could not resolve, and a `var` reassigned in
//! one segment and captured by the next continuation was refused at canonical
//! admission. Independent expectation, canonical SafeWIR interpreter, admitted
//! unoptimized Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_async_output(label: &str, source: &str, expected: &[&str]) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} type-checks: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let arguments = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_program(validated, function, arguments, Default::default())
                .unwrap_or_else(|error| panic!("{label} canonical SafeWIR execution: {error}"))
        },
    )
    .expect_lowered(&format!("{label} admits through canonical SafeWIR"));

    assert_eq!(oracle.output, expected, "{label} canonical interpreter matches the expectation");
    assert_eq!(oracle.leaked_objects, 0, "{label} canonical interpreter releases all roots");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    vm.run().unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(vm.output(), expected, "{label} admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "{label} admitted Wasm releases all roots");
}

/// One program, one async compile: every case shares the module so the test
/// stays within the gate's per-test time budget.
#[test]
fn var_and_let_locals_compose_with_suspension() {
    // Each worker prints its number when joined; main then prints its local.
    // A String var reassigned between spawn and join (1, 6); an Int var
    // reassigned the same way (7, 2); a String reassigned between two ticks
    // (6); a List var pushed between spawn and join (9, 3); a let List read
    // after an await (2).
    assert_async_output(
        "var and let locals across suspension",
        "import chan\n\nasync fn worker(console: Console, n: Int):\n    chan.yield_now().await\n    console.print(\"${n}\")\n\nasync fn tick():\n    chan.yield_now().await\n\nasync fn main(console: Console):\n    var name = \"abc\"\n    let first = chan.spawn(worker(console, 1)).await\n    name = \"zzzzzz\"\n    chan.join(first).await\n    console.print(\"${name.length()}\")\n    var n = 1\n    let second = chan.spawn(worker(console, 7)).await\n    n = n + 1\n    chan.join(second).await\n    console.print(\"${n}\")\n    var s = \"abc\"\n    tick().await\n    s = s + \"def\"\n    tick().await\n    console.print(\"${s.length()}\")\n    var xs = [1, 2]\n    let third = chan.spawn(worker(console, 9)).await\n    xs.push(3)\n    chan.join(third).await\n    console.print(\"${xs.length()}\")\n    let ys = [4, 5]\n    tick().await\n    console.print(\"${ys.length()}\")\n",
        &["1", "6", "7", "2", "6", "9", "3", "2"],
    );
}
