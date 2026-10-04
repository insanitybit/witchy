//! Generic call arguments reuse branch result storage captured at scope exit.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn plain_branch_arguments_specialize_generic_results() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    var first = wrap(if true: Box(2) else: Box(4))
    first.0.value = 9
    var second = wrap(match false:
        false -> Box(4)
        _ -> Box(6))
    second.0.value = 8
    first.0.value * 1000 + first.1 * 100 + second.0.value * 10 + second.1
"#).expect("plain branch arguments prove generic result storage");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("generic branch argument storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["9383".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_if_argument_cannot_be_laundered_through_generic_result() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = wrap(if true: Box(2) else: cold)
    pair.0.value = 9
    0
"#).expect_err("frozen arm must qualify generic result");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn shadowed_match_argument_cannot_use_outer_plain_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    let item = Box(2)
    var pair = wrap(match false:
        false ->
            let item: frozen Box = Box(1)
            item
        _ -> item)
    pair.0.value = 9
    0
"#).expect_err("match-local frozen result cannot use outer plain proof");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
