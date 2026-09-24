//! RFC-0167 row G: an exact `&[List(Int)]` slice retains its outer list root.
//! The nested List element is an owned child; the slice may read a cloned
//! child only through its checked range and the backing root's live bounds.

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
fn an_int_list_slice_retains_owned_children_across_nested_mutation() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(
        r#"
fn relay(rows: &[List(Int)]) -> &[List(Int)]:
    rows

fn main() -> Int:
    var rows = [[1, 2], [3]]
    let old = relay(&rows[0..2])
    rows[0][0] = 9
    rows.push([4])
    (old[0][0] + old[1][0]) * 100 + rows[0][0] * 10 + rows.length()
"#,
    )
    .expect("exact nested Int-list slice type-checks");
    // The retained slice still reads 1 and 3. The current first child reads
    // 9 and the current outer list has 3 rows: 4*100 + 9*10 + 3 = 493.
    let (artifact, ()) =
        codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
            .expect_lowered("exact slice child layout and SafeWIR authority");
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
    assert_eq!(vm.output(), vec!["493".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn a_third_list_shell_is_not_a_slice_element_carrier() {
    let checked = witchy::resolve_std_only_checked(
        r#"
fn relay(rows: &[List(List(Int))]) -> &[List(List(Int))]:
    rows

fn main() -> Int:
    let rows = [[[1]], [[2]]]
    relay(&rows[0..2]).length()
"#,
    )
    .expect("deeper slice type-checks before canonical admission");
    let refusal = codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("a third List shell has no slice carrier");
    assert!(refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"), "{refusal}");
}

#[test]
fn a_qualified_list_child_cannot_borrow_slice_authority() {
    for qualifier in ["unique", "frozen"] {
        let source = format!(
            "fn relay(rows: &[{qualifier} List(Int)]) -> &[{qualifier} List(Int)]:\n    rows\n\n\
             fn main() -> Int:\n    let rows: List({qualifier} List(Int)) = [[1], [2]]\n    relay(&rows[0..2]).length()\n"
        );
        let checked = witchy::resolve_std_only_checked(&source)
            .expect("qualified slice source type-checks before canonical admission");
        let refusal = codegen::compile_checked_module_artifact(&checked)
            .expect_unsupported("qualified child has no exact slice carrier");
        assert!(refusal.to_string().contains("rooted view payload has no admitted authenticated carrier"), "{qualifier}: {refusal}");
    }
}

#[test]
fn must_is_not_an_element_type_qualifier() {
    let error = witchy::resolve_std_only_checked(
        "fn relay(rows: &[must List(Int)]) -> Int:\n    rows.length()\n",
    )
    .expect_err("must is a parameter convention, not a slice element type qualifier");
    assert!(error.to_string().contains("must"), "{error}");
}

#[test]
fn a_region_born_outer_list_cannot_escape_through_the_slice() {
    let error = witchy::resolve_std_only_checked(
        r#"
fn bad() -> &[List(Int)]:
    region:
        let rows = [[1], [2]]
        &rows[0..2]

fn main() -> Int:
    bad().length()
"#,
    )
    .expect_err("region-born outer root cannot escape through the slice");
    assert!(error.to_string().contains("region-born"), "{error}");
}
