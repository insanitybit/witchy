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
fn option_root_with_list_of_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
fn read(view: &Option(List(&Int))) -> Int:
    match *view:
        Some(xs) -> *xs[0]
        None -> 0

fn main() -> Int:
    let old_value = 2
    let new_value = 9
    var box: Option(List(&Int)) = Some([&old_value])
    let old = &box
    box = Some([&new_value])
    read(old) * 10 + read(&box)
"#,
    )
    .expect("nested view-bearing List has a checked Option root carrier");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("Option root preserves the nested view carrier");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["29".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn option_root_with_string_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(view: &Option(List(&String))) -> Int:
    match *view:
        Some(xs) -> (*xs[0]).length()
        None -> 0

fn main() -> Int:
    let first = "ab"
    let second = "xyz"
    var box: Option(List(&String)) = Some([&first])
    let old = &box
    box = Some([&second])
    read(old) * 10 + read(&box)
"#).expect("nested String views have a checked root");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nested String views admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["23".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn tuple_root_with_list_of_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(view: &(List(&Int), Int)) -> Int:
    let pair = *view
    *(pair.0[0]) * 10 + pair.1

fn main() -> Int:
    let a = 1
    let b = 2
    var pair: (List(&Int), Int) = ([&a], 1)
    let old = &pair
    pair = ([&b], 2)
    read(old) * 100 + read(&pair)
"#).expect("tuple nested-view root checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("tuple nested-view root admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["1122".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn tuple_root_with_string_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(view: &(List(&String), Int)) -> Int:
    let pair = *view
    (*pair.0[0]).length()

fn main() -> Int:
    let a = "ab"
    let b = "xyz"
    var pair: (List(&String), Int) = ([&a], 1)
    let old = &pair
    pair = ([&b], 2)
    read(old) * 10 + read(&pair)
"#).expect("tuple String-view root checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("tuple String-view root admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["23".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn tuple_graph_cannot_return_a_region_born_view_root() {
    let error = witchy::resolve_std_only_checked(r#"
fn main() -> Int:
    let old = region -> &(List(&Int), Int):
        let n = 5
        let pair: (List(&Int), Int) = ([&n], 1)
        &pair
    *(*old).0[0]
"#).expect_err("region-born tuple view must not escape");
    assert!(error.to_string().contains("cannot copy out region-born reference `pair` from `region:`"),
        "refusal names the region-born tuple root: {error}");
}

#[test]
fn nominal_root_with_list_of_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Holder:
    views: List(&Int)

fn read(view: &Holder) -> Int:
    let h = *view
    *(h.views[0])

fn main() -> Int:
    let a = 2
    let b = 9
    var h = Holder(views: [&a])
    let old = &h
    h = Holder(views: [&b])
    read(old) * 10 + read(&h)
"#).expect("nominal nested-view root checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nominal nested-view root admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["29".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn region_born_root_cannot_escape_in_nested_view_graph() {
    let error = witchy::resolve_std_only_checked(r#"
type Holder:
    views: List(&Int)

fn main() -> Int:
    let view = region -> &Holder:
        let local = 5
        let h = Holder(views: [&local])
        &h
    *(*view).views[0]
"#).expect_err("region-born nested view graph must not escape");
    assert!(error.to_string().contains("cannot copy out region-born reference `h` from `region:`"),
        "refusal names the region-born root: {error}");
}

#[test]
fn nested_view_graph_does_not_hide_a_must_obligation() {
    let error = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)

type Holder:
    views: List(&Int)
    ticket: Ticket

fn main() -> Int:
    let value = 5
    let holder = Holder(views: [&value], ticket: Ticket(1))
    let snapshot = &holder
    0
"#).expect_err("a must-bearing aggregate cannot acquire a persistent view");
    assert!(error.to_string().contains("must-consume"),
        "refusal retains the must-consume obligation: {error}");
}
