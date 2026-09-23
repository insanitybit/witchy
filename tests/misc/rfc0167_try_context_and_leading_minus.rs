//! RFC-0167 surface forms that failed on the canonical route.
//!
//! Contextual try (`e ? "msg"`) reached canonical lowering as the unlowered
//! `__try_ctx` intrinsic (`MissingBinding("__try_ctx")`) for every operand.
//! A line-leading `-` continued the previous line as a subtraction, so
//! `let a = 5` / `-a` parsed as `let a = 5 - a`.
//!
//! Independent expected results (worked out from the program text), admitted
//! unoptimized Wasm, zero-leak heap audit.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn run_admitted(source: &str, capabilities: Capabilities) -> Vec<String> {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, capabilities, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
    vm.output()
}

#[test]
fn contextual_try_propagates_option_and_result_with_context() {
    let output = run_admitted(r#"
fn half(n: Int) -> Result(Int, String):
    if n % 2 == 0: Ok(n / 2) else: Err("odd ${n}")

fn first(xs: List(Int)) -> Option(Int):
    if xs.length() == 0: None else: Some(xs[0])

fn use_result(n: Int) -> Result(Int, String):
    let h = half(n) ? "halving"
    Ok(h + 1)

fn use_option(xs: List(Int)) -> Result(Int, String):
    let f = first(xs) ? "empty list"
    Ok(f * 10)

fn outer(n: Int) -> Result(Int, String):
    let v = use_result(n) ? "outer"
    Ok(v * 3)

fn render(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok ${v}"
        Err(e) -> "err ${e}"

fn main(console: Console):
    console.print(render(use_result(8)))
    console.print(render(use_result(7)))
    console.print(render(use_option([4, 5])))
    console.print(render(use_option([])))
    console.print(render(outer(10)))
    console.print(render(outer(9)))
"#, Capabilities { print: true, quiet: true, ..Capabilities::none() });
    assert_eq!(output, vec![
        "ok 5",
        "err halving: odd 7",
        "ok 40",
        "err empty list",
        "ok 18",
        "err outer: halving: odd 9",
    ]);
}

#[test]
fn a_line_leading_minus_is_a_negation() {
    // negate_local = -5; after_let(4) = -(4 * 2) + 1 = -7; shadowed(2) = -2.
    // -5 * 1000 + -7 * 10 + -2 = -5072.
    let output = run_admitted(r#"
fn negate_local() -> Int:
    let a = 5
    -a

fn after_let(b: Int) -> Int:
    let c = b * 2
    -c + 1

fn shadowed(a: Int) -> Int:
    let b = a + 10
    -a

fn main() -> Int:
    negate_local() * 1000 + after_let(4) * 10 + shadowed(2)
"#, Capabilities { print_int: true, quiet: true, ..Capabilities::none() });
    assert_eq!(output, vec!["-5072"]);
}
