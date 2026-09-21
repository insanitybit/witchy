//! RFC-0167 row J: a view crosses public and callable boundaries intact.
//!
//! Independent scalar expectation, canonical SafeWIR interpreter, admitted
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

fn assert_canonical_and_wasm(source: &str, expected: i64) {
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
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)],
        "canonical interpreter matches the independent expected result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

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

/// The three carriers the row names that a nominal boundary can hold: a record
/// field, a type alias, and a generic argument.
#[test]
fn a_view_crosses_nominal_alias_and_generic_boundaries() {
    assert_canonical_and_wasm(
        "type Holder:\n    value: &Int\n\nfn main() -> Int:\n    let n = 79\n    (*Holder(&n).value)\n",
        79,
    );
    assert_canonical_and_wasm(
        "type View = &Int\n\nfn read(v: View) -> Int:\n    (*v)\n\nfn main() -> Int:\n    let n = 79\n    read(&n)\n",
        79,
    );
    assert_canonical_and_wasm(
        "type Box(a):\n    item: a\n\nfn main() -> Int:\n    let n = 79\n    let b = Box(&n)\n    (*b.item)\n",
        79,
    );
}

// NOT YET A WITNESS: callable capture. RFC-0167 permits it explicitly ("a view
// ... may be ... captured by a callable without naming a lifetime", and "not a
// lexical loan"), and removing the `typeck` guard that refuses it makes the
// program below check, lower, and run correctly end to end:
//
//     fn main() -> Int:
//         let n = 79
//         let view = &n
//         let f = fn() -> Int: (*view)
//         f()
//
// The guard cannot simply be deleted, because two tests rely on it to refuse an
// UNADMITTED carrier crossing a callable boundary
// (`typeck::borrowed_nominal_cannot_escape_through_an_inferred_closure` and
// `pipeline::canonical_closure_witness_rejects_borrowed_capture_authentication`,
// both `&String`). Narrowing it by payload is the fix, but `Ty` at that point is
// not the bare payload - a `&Int` binding does not resolve to `Ty::Int` there -
// so the narrowing needs the declared reference type, not the resolved `Ty`.
// Row J records this with the reproduction.
