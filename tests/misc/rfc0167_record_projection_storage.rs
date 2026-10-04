//! Generic record field replacement updates checked root storage qualifiers.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn plain_generic_record_replacement_retains_deep_var_proof() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Holder(a):
    item: a
    mark: Int
fn main() -> Int:
    var holder = Holder((Box(2), 3), 4)
    holder.item = (Box(5), 6)
    holder.item.0.value = 9
    holder.item.0.value * 10 + holder.item.1 + holder.mark
"#).expect("plain replacement retains generic record source proof");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("record replacement admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["100".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn frozen_generic_record_replacement_revokes_deep_var_proof() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Holder(a):
    item: a
    mark: Int
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var holder = Holder((Box(2), 3), 4)
    holder.item = (cold, 6)
    holder.item.0.value = 9
    0
"#).expect_err("frozen replacement must qualify the generic record field");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn nongeneric_record_cannot_encode_a_new_frozen_field() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Holder:
    item: (Box, Int)
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var holder = Holder((Box(2), 3))
    holder.item = (cold, 6)
    holder.item.0.value = 9
    0
"#).expect_err("record root without source type arguments cannot encode frozen replacement");
    assert!(error.to_string().contains("cannot verify deep assignment place"), "{error}");
}

#[test]
fn shared_record_parameter_qualifies_sibling_after_replacement() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Pair(a):
    left: a
    right: a
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = Pair(Box(2), Box(3))
    pair.left = cold
    pair.right.value = 9
    0
"#).expect_err("shared generic source parameter must qualify the sibling field");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
