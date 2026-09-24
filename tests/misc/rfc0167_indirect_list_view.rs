//! RFC-0167 row G: an indirect call transports the exact `List(&Int)` owner
//! family, and an indirect `var` call cannot carry a region-born view out.
//!
//! The positive result is hand-computed and runs as admitted unoptimized Wasm
//! with a zero-leak heap audit. The source interpreter is not an oracle.

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
fn a_list_of_views_crosses_an_indirect_call_with_retained_snapshots() {
    // The two views retain 3 and 4 after nums[0] changes to 9. `sum` is
    // called through `f`, so the independent result is (3 + 4) * 100 + 9 = 709.
    let source = r#"
fn sum(vs: List(&Int)) -> Int:
    (*(vs[0])) + (*(vs[1]))

fn apply(f: fn(List(&Int)) -> Int, vs: List(&Int)) -> Int:
    f(vs)

fn main() -> Int:
    var nums = [3, 4]
    let views = [&nums[0], &nums[1]]
    let f = sum
    nums[0] = 9
    apply(f, views) * 100 + nums[0]
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
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
    assert_eq!(
        vm.output(),
        vec!["709".to_string()],
        "admitted Wasm matches the hand-computed result"
    );
    assert_eq!(
        vm.canonical_heap_leaks().expect("canonical heap audit"),
        Some(0)
    );
}

#[test]
fn an_indirect_var_list_call_cannot_escape_a_region_born_view() {
    let source = r#"
fn add(var vs: List(&Int), v: &Int):
    vs.push(v)

fn main() -> Int:
    var vs: List(&Int) = []
    let f = add
    let _ = region -> ():
        let local = 5
        f(vs, &local)
    0
"#;
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .map(|error| error.to_string())
        .expect("region-born view is refused at checking");
    assert!(
        error.contains("cannot write back into `vs` inside `region:`")
            && error.contains("can hold a view"),
        "refusal names the region write-back rule: {error}"
    );
}
