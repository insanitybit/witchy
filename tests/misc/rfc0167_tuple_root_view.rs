//! RFC-0167 row G: an exact whole `(Int, String)` view keeps its tuple root.

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
fn exact_tuple_view_keeps_both_old_fields_after_owner_replacement() {
    // The view reads 2 and the length of "abc" (3), while the replaced
    // owner reads 9: 2 + 3 + 9 = 14, independent of any interpreter.
    let source = r#"
fn read(v: &(Int, String)) -> Int:
    (*v).0 + string.length((*v).1)

fn relay(v: &(Int, String)) -> &(Int, String):
    v

fn main() -> Int:
    var pair: (Int, String) = (2, "abc")
    let v = relay(&pair)
    pair = (9, "q")
    read(v) + pair.0
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("exact tuple view type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["14".to_string()], "hand-computed tuple snapshot");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn a_region_born_tuple_root_cannot_escape_through_the_view() {
    let source = r#"
fn main() -> Int:
    let v = region -> &(Int, String):
        let pair = (2, "abc")
        &pair
    0
"#;
    let error = witchy::resolve_std_only_checked(source).err()
        .map(|error| error.to_string())
        .expect("region-born tuple root cannot escape");
    assert!(error.contains("region-born"), "region refusal names its root: {error}");
}
