//! RFC-0167 row G: classify a whole Option root whose List child stores a view.

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
fn option_root_with_owned_list_child_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
fn read(view: &Option(List(Int))) -> Int:
    match *view:
        Some(xs) -> xs[0]
        None -> 0

fn main() -> Int:
    var box: Option(List(Int)) = Some([2])
    let old = &box
    box = Some([9])
    read(old) * 10 + (match box:
        Some(xs) -> xs[0]
        None -> 0)
"#,
    )
    .expect("owned List child has a checked Option root");
    let (artifact, ()) =
        codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
            .expect_lowered("Option root has authenticated owned List child");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print_int: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    // Old List begins with 2 while the replacement begins with 9: 29.
    assert_eq!(vm.output(), vec!["29".to_string()]);
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0)
    );
}

#[test]
fn option_root_with_list_of_views_fails_closed_at_storage_classification() {
    let error = witchy::resolve_std_only_checked(
        r#"
fn read(view: &Option(List(&Int))) -> Int:
    match *view:
        Some(xs) -> *xs[0]
        None -> 0

fn main() -> Int:
    let value = 2
    let box: Option(List(&Int)) = Some([&value])
    read(&box)
"#,
    )
    .expect_err("nested view-bearing List has no authenticated Option root carrier");
    assert!(error.to_string().contains("immutable view formation cannot form an immutable view without an authenticated storage carrier"), "{error}");
}
