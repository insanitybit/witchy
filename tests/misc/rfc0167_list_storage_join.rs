//! Homogeneous list literals join source qualifiers across every element.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn plain_list_elements_prove_deep_var_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    var pair = ([Box(2), Box(4)], 3)
    pair.0[0].value = 9
    pair.0[0].value * 10 + pair.1
"#).expect("plain list elements retain deep var storage proof");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("plain list storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_list_element_qualifies_every_index() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = ([Box(2), cold], 3)
    pair.0[0].value = 9
    0
"#).expect_err("one frozen element removes the plain list-element proof");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn nested_frozen_list_element_joins_through_tuples() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = ([(Box(2), 1), (cold, 2)], 3)
    pair.0[0].0.value = 9
    0
"#).expect_err("a nested frozen element must qualify the homogeneous list slot");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
