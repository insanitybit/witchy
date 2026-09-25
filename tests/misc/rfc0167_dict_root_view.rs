//! RFC-0167 row G: whole `&Dict(String, Int)`, `&Dict(String, Bool)`, and
//! `&Dict(String, Float)` views retain their exact roots. This witness uses admitted
//! unoptimized Wasm and a zero-leak audit.

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
fn string_scalar_dictionary_views_keep_old_roots_after_owner_mutation() {
    // The Int view has one entry while its owner has two: 1 * 10 + 2 = 12.
    // The Bool view still reads true (100) after its owner replaces it with
    // false (0). The Float view still reads 1.5 (1000) after its owner stores
    // 9.5 (0). The independent result is 12 + 100 + 1000 + 0 = 1112.
    let source = r#"
fn count(v: &Dict(String, Int)) -> Int:
    (*v).length()

fn old_bool(v: &Dict(String, Bool)) -> Int:
    if (*v).at("a"): 100 else: 0

fn old_float(v: &Dict(String, Float)) -> Int:
    if (*v).at("a") == 1.5: 1000 else: 0

fn main() -> Int:
    var d: Dict(String, Int) = dict.new()
    let _ = d.insert("a", 2)
    let v = &d
    let _ = d.insert("b", 3)
    var flags: Dict(String, Bool) = dict.new()
    let _ = flags.insert("a", true)
    let old_flags = &flags
    let _ = flags.insert("a", false)
    var measures: Dict(String, Float) = dict.new()
    let _ = measures.insert("a", 1.5)
    let old_measures = &measures
    let _ = measures.insert("a", 9.5)
    count(v) * 10 + d.length() + old_bool(old_flags) + old_float(old_measures) + (if flags.at("a"): 10000 else: 0) + (if measures.at("a") == 9.5: 0 else: 10000)
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
    assert_eq!(vm.output(), vec!["1112".to_string()], "hand-computed scalar dictionary snapshots");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn other_dictionary_view_payloads_stay_outside_the_exact_carrier() {
    let different_value = r#"
fn count(v: &Dict(String, String)) -> Int:
    (*v).length()

fn main() -> Int:
    var d: Dict(String, String) = dict.new()
    let _ = d.insert("a", "value")
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
    let bool_closure_capture = r#"
fn main() -> Int:
    var d: Dict(String, Bool) = dict.new()
    let _ = d.insert("a", true)
    let view = &d
    let read = fn() -> Int: if (*view).at("a"): 1 else: 0
    read()
"#;
    let float_closure_capture = r#"
fn main() -> Int:
    var d: Dict(String, Float) = dict.new()
    let _ = d.insert("a", 1.5)
    let view = &d
    let read = fn() -> Int: if (*view).at("a") == 1.5: 1 else: 0
    read()
"#;
    for source in [bool_closure_capture, float_closure_capture] {
        let error = witchy::resolve_std_only_checked(source).err()
            .map(|error| error.to_string())
            .expect("dictionary view closure capture stays fail-closed");
        assert!(error.contains("no admitted carrier"), "closure capture refusal: {error}");
    }
}

#[test]
fn a_region_born_dictionary_root_cannot_escape_through_the_view() {
    let int_source = r#"
fn main() -> Int:
    let v = region -> &Dict(String, Int):
        var d: Dict(String, Int) = dict.new()
        let _ = d.insert("a", 2)
        &d
    0
"#;
    let bool_source = r#"
fn main() -> Int:
    let v = region -> &Dict(String, Bool):
        var d: Dict(String, Bool) = dict.new()
        let _ = d.insert("a", true)
        &d
    0
"#;
    let float_source = r#"
fn main() -> Int:
    let v = region -> &Dict(String, Float):
        var d: Dict(String, Float) = dict.new()
        let _ = d.insert("a", 1.5)
        &d
    0
"#;
    for source in [int_source, bool_source, float_source] {
        let error = witchy::resolve_std_only_checked(source).err()
            .map(|error| error.to_string())
            .expect("region-born dictionary root cannot escape");
        assert!(error.contains("region-born"), "region refusal names its root: {error}");
    }
}
