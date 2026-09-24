//! RFC-0167 row G: `?` on an `Option(&Int)` / `Result(&Int, E)` operand.
//!
//! An aggregate whose payload is an `&Int` view is an opaque owner, so `?`
//! reads its tag and payloads through the owner family (as `match` does)
//! rather than as fields of a plain tagged record. The payload read carries
//! its own token, so release placement no longer loses it.
//! Independent expectation, admitted unoptimized Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

const OPTION_PICK: &str = r#"
fn pick(xs: &[Int], i: Int) -> Option(&Int):
    if i < xs.length(): Some(&xs[i]) else: None

fn get(xs: &[Int], i: Int) -> Option(Int):
    let v = pick(xs, i)?
    Some(*v + 1)
"#;

#[test]
fn try_on_an_option_int_view_propagates_none() {
    // Index 7 is out of range: `?` returns None, `??` yields 5.
    assert_wasm(&format!("{OPTION_PICK}
fn main() -> Int:
    let xs = [10, 20, 30]
    get(&xs[0..3], 7) ?? 5
"), 5);
}

