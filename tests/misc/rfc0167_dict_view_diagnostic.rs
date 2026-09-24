//! RFC-0167 row G: dictionary values need an admitted collection carrier.
//! `Dict(String, &String)` must fail at canonical admission with a named
//! reason until dictionary operations authenticate retained String view roots.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn owned_string_dictionary_is_admitted_and_releases_its_storage() {
    // Two inserted keys remain after the String values have been stored.
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn main() -> Int:
    var d: Dict(String, String) = dict.new()
    let _ = d.insert("a", "one")
    let _ = d.insert("b", "two")
    d.length()
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("owned dictionary type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2".to_string()], "independent expected dictionary length");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn string_view_dictionary_has_a_named_canonical_refusal() {
    let source = r#"
fn main() -> Int:
    let value = "snapshot"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("key", &value)
    d.length()
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("view dictionary reaches canonical carrier admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("a dictionary of String views has no collection carrier");
    assert!(refusal.to_string().contains("Dict value `&String` has no admitted collection carrier"),
        "refusal names the missing carrier: {refusal}");
}

#[test]
fn string_view_dictionary_nested_in_a_record_stays_refused() {
    let source = r#"
type Holder:
    entries: Dict(String, &String)

fn main() -> Int:
    let value = "snapshot"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("key", &value)
    let holder = Holder(entries: d)
    holder.entries.length()
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("nested dictionary view reaches canonical carrier admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("a record must not erase a dictionary String view carrier");
    assert!(refusal.to_string().contains("Dict value `&String` has no admitted collection carrier"),
        "nested refusal names the missing carrier: {refusal}");
}

#[test]
fn nested_string_view_dictionary_cannot_cross_a_record_abi() {
    let source = r#"
type Holder:
    entries: Dict(String, &String)

pub fn relay(holder: Holder) -> Holder:
    holder

fn main() -> Int:
    0
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("record declaration and relay type-check");
    codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("a record relay must not erase a nested dictionary view");
}
