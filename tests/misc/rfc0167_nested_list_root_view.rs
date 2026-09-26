//! RFC-0167 row G: a whole view of `List(List(Int))` retains the outer root.
//! The outer list owns each inner list, so its last release reclaims both.
//! Independent expected result, admitted unoptimized Wasm, zero leaked roots.

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
fn a_nested_int_list_root_view_keeps_its_owned_children_after_mutation() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
fn relay(rows: &List(List(Int))) -> &List(List(Int)):
    rows

fn main() -> Int:
    var rows = [[1, 2], [3]]
    let old = relay(&rows)
    rows[0][0] = 9
    rows.push([4, 5, 6])
    var sum = 0
    for row in (*old):
        sum = sum + row[0]
    sum * 1000 + (*old).length() * 100 + rows.length() * 10 + rows[0][0]
"#,
    )
    .expect("nested-list view type-checks");
    // The old rows still begin with 1 and 3; their sum is 4. The retained
    // root has 2 rows while the mutated owner has 3, and its first inner value
    // became 9: 4,000 + 200 + 30 + 9 = 4,239.
    let (artifact, ()) =
        codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
            .expect_lowered("canonical SafeWIR admission");
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
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["4239".to_string()]);
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0)
    );
}

#[test]
fn a_closure_capture_retains_the_nested_list_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
mode opt

fn main() -> Int:
    var rows = [[1, 2], [3]]
    let old = &rows
    let action = fn() -> Int:
        (*old)[0][0] + (*old)[1][0]
    rows[0][0] = 9
    action() * 10 + rows[0][0]
"#,
    )
    .expect("exact whole-root carrier survives a closure capture");
    // The closure keeps the old children 1 and 3 while the current first
    // child becomes 9: (1 + 3)*10 + 9 = 49.
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("closure capture has an authenticated nested-list root");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["49".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn a_generic_choice_retains_the_selected_nested_list_root() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
mode opt

fn choose[a](left: &a, right: &a, choose_left: Bool) -> &a:
    if choose_left:
        left
    else:
        right

fn main() -> Int:
    var left = [[1], [2]]
    var right = [[3], [4]]
    let old_left = choose(&left, &right, true)
    let old_right = choose(&left, &right, false)
    left[0][0] = 9
    right[0][0] = 8
    (*old_left)[0][0] * 1000 + (*old_right)[0][0] * 100 + left[0][0] * 10 + right[0][0]
"#,
    )
    .expect("generic choice keeps either authenticated nested-list root");
    // The true and false branches retain the original 1 and 3. The current
    // roots read 9 and 8: 1,000 + 300 + 90 + 8 = 1,398.
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("both generic root choices have authenticated carriers");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["1398".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn a_region_born_nested_list_cannot_escape_through_a_view() {
    let error = witchy::resolve_std_only_checked(
        r#"
fn bad() -> &List(List(Int)):
    region:
        let local = [[1, 2], [3]]
        &local

fn main() -> Int:
    (*bad()).length()
"#,
    )
    .expect_err("a region-born root cannot escape through the whole-list view");
    assert!(error.to_string().contains("region-born"), "{error}");
}

#[test]
fn a_qualified_inner_list_has_no_authenticated_whole_root_carrier() {
    let error = witchy::resolve_std_only_checked(
        r#"
fn relay(view: &List(unique List(Int))) -> &List(unique List(Int)):
    view

fn main() -> Int:
    let rows: List(unique List(Int)) = [[1, 2], [3]]
    let old = relay(&rows)
    (*old).length()
"#,
    )
    .expect_err("qualified inner list has no admitted root carrier");
    assert!(error.to_string().contains("rooted view payload has no admitted authenticated carrier"), "{error}");
}
