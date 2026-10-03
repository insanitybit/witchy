//! Source result qualifiers remain visible through direct calls.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn plain_direct_call_result_proves_deep_tuple_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn fresh() -> (Box, Int):
    (Box(2), 3)
fn main() -> Int:
    var pair = fresh()
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("direct call return type proves tuple storage");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("direct result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_direct_call_result_cannot_hide_inside_plain_tuple_annotation() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn frozen_box() -> frozen Box:
    Box(1)
fn main() -> Int:
    var pair: (Box, Int) = (frozen_box(), 2)
    pair.0.value = 9
    0
"#).expect_err("direct frozen result cannot become mutable tuple storage");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn nested_frozen_direct_result_cannot_hide_inside_plain_tuple_annotation() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn frozen_pair() -> (frozen Box, Int):
    (Box(1), 2)
fn main() -> Int:
    var pair: (Box, Int) = frozen_pair()
    pair.0.value = 9
    0
"#).expect_err("nested frozen result cannot become mutable tuple storage");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn indirect_result_without_source_shape_cannot_use_plain_annotation_as_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn frozen_pair() -> (frozen Box, Int):
    (Box(1), 2)
fn main() -> Int:
    let make = frozen_pair
    var pair: (Box, Int) = make()
    pair.0.value = 9
    0
"#).expect_err("an indirect call cannot inherit a plain storage proof");
    assert!(error.to_string().contains("cannot verify deep assignment place"), "{error}");
}
