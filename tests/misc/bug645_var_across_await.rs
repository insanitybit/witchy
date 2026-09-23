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

#[test]
fn a_var_reassigned_between_suspensions_is_read_after_them() {
    assert_async_output(
        "String var reassigned between spawn and join",
        "import chan\n\nasync fn worker(console: Console, n: Int):\n    chan.yield_now().await\n    console.print(\"${n}\")\n\nasync fn main(console: Console):\n    var name = \"abc\"\n    let h = chan.spawn(worker(console, 1)).await\n    name = \"zzzzzz\"\n    chan.join(h).await\n    let n = name.length()\n    console.print(\"${n}\")\n",
        &["1", "6"],
    );
    assert_async_output(
        "Int var reassigned between spawn and join",
        "import chan\n\nasync fn worker(console: Console):\n    chan.yield_now().await\n    console.print(\"w\")\n\nasync fn main(console: Console):\n    var n = 1\n    let h = chan.spawn(worker(console)).await\n    n = n + 1\n    chan.join(h).await\n    console.print(\"${n}\")\n",
        &["w", "2"],
    );
    assert_async_output(
        "String var reassigned between two ticks",
        "import chan\n\nasync fn tick():\n    chan.yield_now().await\n\nasync fn main(console: Console):\n    var s = \"abc\"\n    tick().await\n    s = s + \"def\"\n    tick().await\n    console.print(\"${s.length()}\")\n",
        &["6"],
    );
}

#[test]
fn a_carried_list_keeps_its_type_for_method_calls() {
    assert_async_output(
        "List var pushed between spawn and join",
        "import chan\n\nasync fn worker(console: Console):\n    chan.yield_now().await\n    console.print(\"w\")\n\nasync fn main(console: Console):\n    var xs = [1, 2]\n    let h = chan.spawn(worker(console)).await\n    xs.push(3)\n    chan.join(h).await\n    console.print(\"${xs.length()}\")\n",
        &["w", "3"],
    );
    assert_async_output(
        "let List read after an await",
        "import chan\n\nasync fn main(console: Console):\n    let xs = [4, 5]\n    chan.yield_now().await\n    console.print(\"${xs.length()}\")\n",
        &["2"],
    );
}
