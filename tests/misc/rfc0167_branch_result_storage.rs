//! Aggregate results from completing control-flow arms retain source qualifiers.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn plain_if_and_match_results_admit_deep_var_writes() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    var conditional = if true:
        (Box(2), 3)
    else:
        (Box(4), 5)
    conditional.0.value = 9
    var selected = match false:
        false -> (Box(4), 5)
        _ -> (Box(6), 7)
    selected.0.value = 8
    conditional.0.value * 1000 + conditional.1 * 100 + selected.0.value * 10 + selected.1
"#).expect("all completing arms have plain aggregate storage");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("branch result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["9385".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_if_arm_prevents_deep_var_write() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = if true:
        (Box(2), 3)
    else:
        (cold, 4)
    pair.0.value = 9
    0
"#).expect_err("a frozen completing arm must keep the result frozen");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn frozen_match_arm_prevents_deep_var_write() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = match false:
        false -> (Box(2), 3)
        _ -> (cold, 4)
    pair.0.value = 9
    0
"#).expect_err("a frozen match arm must keep the result frozen");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn shadowed_if_arm_cannot_borrow_outer_plain_storage_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let item = Box(2)
    var pair = if true:
        let item: frozen Box = Box(1)
        (item, 3)
    else:
        (item, 4)
    pair.0.value = 9
    0
"#).expect_err("the arm-local frozen binding must win over its plain shadow");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn shadowed_match_arm_cannot_borrow_outer_plain_storage_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let item = Box(2)
    var pair = match false:
        false ->
            let item: frozen Box = Box(1)
            (item, 3)
        _ -> (item, 4)
    pair.0.value = 9
    0
"#).expect_err("the match-arm frozen binding must win over its plain shadow");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
