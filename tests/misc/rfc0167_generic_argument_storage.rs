//! Checked nested calls retain source qualifiers through generic results.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn nested_call_argument_proves_writable_generic_result_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn fresh() -> Box:
    Box(2)
fn identity[a](value: a) -> a:
    value
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    var pair = wrap(identity(fresh()))
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("nested checked calls retain the concrete plain source result");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nested generic result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn nested_call_argument_keeps_frozen_generic_result_storage() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn frozen_box() -> frozen Box:
    Box(1)
fn identity[a](value: a) -> a:
    value
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    var pair = wrap(identity(frozen_box()))
    pair.0.value = 9
    0
"#).expect_err("nested checked calls cannot erase a frozen result");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
