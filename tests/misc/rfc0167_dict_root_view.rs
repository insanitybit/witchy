//! RFC-0167 row G: a whole `&Dict(String, Int)` retains its exact dictionary
//! root. This witness uses admitted unoptimized Wasm and a zero-leak audit.

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
fn string_int_dictionary_view_keeps_the_old_root_after_owner_mutation() {
    // The view snapshots one entry. The owner receives a second entry:
    // 1 * 10 + 2 = 12, independent of the interpreter.
    let source = r#"
fn count(v: &Dict(String, Int)) -> Int:
    (*v).length()

fn main() -> Int:
    var d: Dict(String, Int) = dict.new()
    let _ = d.insert("a", 2)
    let v = &d
    let _ = d.insert("b", 3)
    count(v) * 10 + d.length()
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("exact dictionary view type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["12".to_string()], "hand-computed view and owner lengths");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn other_dictionary_view_payloads_stay_outside_the_exact_carrier() {
    let different_value = r#"
fn count(v: &Dict(String, Bool)) -> Int:
    (*v).length()

fn main() -> Int:
    var d: Dict(String, Bool) = dict.new()
    let _ = d.insert("a", true)
    count(&d)
"#;
    let nested_view = r#"
fn count(v: &Dict(String, &Int)) -> Int:
    (*v).length()

fn main() -> Int:
    0
"#;
    for source in [different_value, nested_view] {
        let checked = witchy::resolve_std_only_checked(source)
            .expect("near-miss dictionary view reaches canonical admission");
        let refusal = codegen::compile_checked_module_artifact(&checked)
            .expect_unsupported("other dictionary values need their own authenticated carrier");
        assert!(refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"),
            "near miss names the missing carrier: {refusal}");
    }
}

#[test]
fn a_region_born_dictionary_root_cannot_escape_through_the_view() {
    let source = r#"
fn main() -> Int:
    let v = region -> &Dict(String, Int):
        var d: Dict(String, Int) = dict.new()
        let _ = d.insert("a", 2)
        &d
    0
"#;
    let error = witchy::resolve_std_only_checked(source).err()
        .map(|error| error.to_string())
        .expect("region-born dictionary root cannot escape");
    assert!(error.contains("region-born"), "region refusal names its root: {error}");
}
