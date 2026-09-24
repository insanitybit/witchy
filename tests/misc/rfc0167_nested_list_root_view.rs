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
    let checked = witchy::resolve_std_only_checked(
        r#"
fn relay(view: &List(unique List(Int))) -> &List(unique List(Int)):
    view

fn main() -> Int:
    let rows: List(unique List(Int)) = [[1, 2], [3]]
    let old = relay(&rows)
    (*old).length()
"#,
    )
    .expect("near-miss source type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("qualified inner list has no admitted root carrier");
    assert!(
        refusal
            .to_string()
            .contains("rooted view payload has no admitted authenticated carrier"),
        "canonical admission must refuse a qualified inner list: {refusal}",
    );
}
