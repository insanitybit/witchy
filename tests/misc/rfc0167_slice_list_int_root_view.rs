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
fn a_qualified_list_child_cannot_borrow_slice_authority() {
    let refusal = witchy::resolve_std_only_checked(
        r#"
fn relay(rows: &[unique List(Int)]) -> &[unique List(Int)]:
    rows

fn main() -> Int:
    let rows: List(unique List(Int)) = [[1], [2]]
    relay(&rows[0..2]).length()
"#,
    )
    .expect_err("qualified child has no exact checked callable ABI");
    assert_eq!(
        refusal.to_string(),
        "type error: canonical callable ABI is invalid: qualified rooted view differs from finalized checked type"
    );
}
