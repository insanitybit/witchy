//! Generic constructor arguments retain source qualifiers at call results.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn generic_constructor_source_specializes_plain_result_storage() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Envelope(a):
    Envelope(a)
fn extract[a](packet: Envelope(a)) -> a:
    match packet:
        Envelope(value) -> value
fn main() -> Int:
    var pair = (extract(Envelope(Box(2))), 3)
    pair.0.value = 9
    pair.0.value * 10 + pair.1
"#).expect("generic constructor retains plain payload source type");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("constructor result storage admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["93".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn generic_constructor_cannot_launder_frozen_payload() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int
type Envelope(a):
    Envelope(a)
fn extract[a](packet: Envelope(a)) -> a:
    match packet:
        Envelope(value) -> value
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = (extract(Envelope(cold)), 3)
    pair.0.value = 9
    0
"#).expect_err("generic constructor cannot erase frozen payload source");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}

#[test]
fn repeated_constructor_parameter_joins_frozen_payload_in_either_position() {
    for arguments in ["Box(2), cold", "cold, Box(2)"] {
        let source = format!(r#"
type Box:
    value: Int
type Twin(a):
    Twin(a, a)
fn select[a](packet: Twin(a)) -> a:
    match packet:
        Twin(_, value) -> value
fn main() -> Int:
    let cold: frozen Box = Box(1)
    var pair = (select(Twin({arguments})), 3)
    pair.0.value = 9
    0
"#);
        let error = witchy::resolve_std_only_checked(&source)
            .expect_err("each constructor field binding must retain frozen storage");
        assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
    }
}
