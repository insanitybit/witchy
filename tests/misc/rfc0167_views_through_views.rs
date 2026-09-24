//! RFC-0167 §5: a view formed through another view.
//!
//! `&(*b).f` and `&(*v)[i]` project through a dereferenced view. The result
//! retains the original root carried by that view and composes the checked
//! projection; it still reads the formation-time snapshot after the owner
//! `var` is reassigned. Independent hand-computed expectation, admitted
//! unoptimized Wasm, zero-leak heap audit.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

/// Compile to admitted unoptimized Wasm and run it. `Ok` carries the printed
/// output after a zero-leak audit; `Err` carries the runtime failure.
fn run_admitted(source: &str) -> Result<Vec<String>, String> {
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
    vm.run().map_err(|error| format!("{error:?}"))?;
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
    Ok(vm.output())
}

#[test]
fn element_view_through_a_view_is_bounds_checked_when_formed() {
    let failure = run_admitted(r#"
fn at(v: &List(Int), i: Int) -> &Int:
    &(*v)[i]

fn main() -> Int:
    let xs = [1, 2, 3]
    let view = at(&xs, 3)
    0
"#).expect_err("an out-of-bounds element view traps at formation");
    assert!(!failure.is_empty());
}
