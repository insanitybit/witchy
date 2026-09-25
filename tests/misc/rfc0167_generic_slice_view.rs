//! RFC-0167 row O: the generic slice carrier. A `&[T]` view of a `List` of
//! `Float`, `Bool`, `Duration`, or `String` elements is the same zero-copy
//! carrier as `&[Int]`: the retained list root plus a checked `{start, len}`
//! projection. Element reads are bounds-checked against the slice, not the
//! backing list; scalar element views are the value and a String element
//! view is the retained String token.
//!
//! Independent expectation, canonical SafeWIR interpreter, admitted
//! unoptimized Wasm, zero-leak audit on both. The source interpreter is not
//! consulted.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

/// The program must compile, then abort in the canonical interpreter and
/// trap in admitted Wasm.
fn assert_aborts_on_both_engines(source: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
        },
    )
    .expect_lowered("canonical SafeWIR admission");
    assert!(oracle.is_err(), "the canonical interpreter aborts");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    assert!(vm.run().is_err(), "admitted Wasm traps");
}

/// Index 1 exists in the backing list but not in the one-element slice, so
/// the read aborts rather than reading past the projection.
#[test]
fn a_slice_index_past_the_projection_aborts_on_both_engines() {
    assert_aborts_on_both_engines(r#"
fn at(xs: &[Float], i: Int) -> Float:
    xs[i]

fn main() -> Int:
    let xs = [1.5, 2.5, 3.5]
    if at(&xs[1..2], 1) > 0.0: 1 else: 0
"#);
    assert_aborts_on_both_engines(r#"
fn at(xs: &[String], i: Int) -> Int:
    let word: String = xs[i]
    word.length()

fn main() -> Int:
    let xs = ["a", "bb", "ccc"]
    at(&xs[0..1], 1)
"#);
}
