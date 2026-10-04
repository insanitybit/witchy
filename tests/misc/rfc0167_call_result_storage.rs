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
fn indirect_frozen_result_retains_its_source_qualifier() {
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
"#).expect_err("an indirect frozen result must retain its source qualifier");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn plain_indirect_result_proves_deep_tuple_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn fresh() -> (Box, Int):
    (Box(2), 3)
fn main() -> Int:
    let make = fresh
    var pair = make()
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("indirect call return type proves tuple storage");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("indirect result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn mutable_callable_rebinding_cannot_reuse_a_plain_result_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn fresh() -> (Box, Int):
    (Box(2), 3)
fn frozen_pair() -> (frozen Box, Int):
    (Box(1), 2)
fn main() -> Int:
    var make = fresh
    make = frozen_pair
    var pair = make()
    pair.0.value = 9
    0
"#).expect_err("rebound callable result cannot use its stale plain proof");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn conditional_callable_rebinding_cannot_reuse_a_plain_result_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn fresh() -> (Box, Int):
    (Box(2), 3)
fn frozen_pair() -> (frozen Box, Int):
    (Box(1), 2)
fn main() -> Int:
    var make = fresh
    if true:
        make = frozen_pair
    var pair = make()
    pair.0.value = 9
    0
"#).expect_err("conditional callable rebinding must join result storage");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn plain_generic_result_proves_deep_tuple_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    var pair = wrap(Box(2))
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("generic result retains the checked plain argument shape");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("generic result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_generic_argument_cannot_hide_in_plain_result_annotation() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    let item: frozen Box = Box(1)
    var pair: (Box, Int) = wrap(item)
    pair.0.value = 9
    0
"#).expect_err("generic result must retain the argument's frozen qualifier");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn repeated_generic_parameter_joins_frozen_argument_in_either_position() {
    for call in ["choose(Box(2), item)", "choose(item, Box(2))"] {
        let source = format!(r#"
type Box:
    value: Int
fn choose[a](left: a, right: a) -> a:
    right
fn main() -> Int:
    let item: frozen Box = Box(1)
    var pair = ({call}, 3)
    pair.0.value = 9
    0
"#);
        let error = witchy::resolve_std_only_checked(&source)
            .expect_err("every binding of a result parameter must retain frozen storage");
        assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
    }
}

#[test]
fn nested_generic_list_result_retains_a_frozen_element() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap_list[a](value: List(a)) -> (List(a), Int):
    (value, 3)
fn main() -> Int:
    let item: frozen Box = Box(1)
    var pair = wrap_list([item])
    pair.0[0].value = 9
    0
"#).expect_err("nested generic result must retain a frozen element");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn generic_function_value_specializes_a_plain_result() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    let make = wrap
    var pair = make(Box(2))
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("generic function value retains result storage provenance");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("generic function value admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn generic_function_value_retains_a_frozen_argument_in_its_result() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
fn wrap[a](value: a) -> (a, Int):
    (value, 3)
fn main() -> Int:
    let item: frozen Box = Box(1)
    let make = wrap
    var pair: (Box, Int) = make(item)
    pair.0.value = 9
    0
"#).expect_err("generic function value must retain frozen result storage");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
