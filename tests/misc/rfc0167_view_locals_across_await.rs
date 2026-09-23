//! RFC-0167 row K: immutable view locals are held across `await`. Async
//! lowering carries a view local into the continuation segment typed as the
//! view (`&place` of a typed local is `&T`), so reads through it resolve after
//! the suspension, and the view keeps its snapshot while its `var` root is
//! mutated between suspensions.
//!
//! Independent expectation, canonical SafeWIR interpreter, admitted
//! unoptimized Wasm, zero-leak audit on both. One program, one async compile.

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
fn view_locals_are_held_across_await() {
    // `&Int` 41 + 1; `&List(Int)` length 3; `&String` "abcdef" length 6;
    // record view 3 + 4; a view of a `var` List keeps its 3-element snapshot
    // while the List grows to 4 between suspensions; a view passed into an
    // async function still reads 3.
    assert_async_output(
        "view locals across await",
        "import chan\n\ntype P:\n    x: Int\n    y: Int\n\nasync fn count(v: &List(Int)) -> Int:\n    chan.yield_now().await\n    (*v).length()\n\nasync fn main(console: Console):\n    let n = 41\n    let iv = &n\n    let xs = [1, 2, 3]\n    let lv = &xs\n    let s = \"abc\" + \"def\"\n    let sv = &s\n    let p = P(x: 3, y: 4)\n    let pv = &p\n    var ys = [1, 2, 3]\n    let yv = &ys\n    chan.yield_now().await\n    ys.push(4)\n    chan.yield_now().await\n    console.print(\"${(*iv) + 1}\")\n    console.print(\"${(*lv).length()}\")\n    console.print(\"${(*sv).length()}\")\n    console.print(\"${(*pv).x + (*pv).y}\")\n    console.print(\"${(*yv).length()} ${ys.length()}\")\n    let c = count(lv).await\n    console.print(\"${c}\")\n",
        &["42", "3", "6", "7", "3 4", "3"],
    );
}
