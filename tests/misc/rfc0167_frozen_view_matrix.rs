//! RFC-0167 row F: the frozen/view qualifier matrix at the canonical boundary.
//!
//! Every positive fixture uses an independent scalar expectation, the canonical
//! SafeWIR interpreter, and admitted unoptimized Wasm, with a zero-leak audit on
//! both. The source interpreter is not consulted.
//!
//! Scope: rooted views admit the canonical `Int` carrier (row G records that
//! boundary and the `&Float` refusal), so these fixtures are `&Int` shaped. They
//! pin the FROZEN half of the matrix: an outer qualifier on a reference is
//! invalid, `&frozen Int` keeps its payload contract through every carrier
//! position, and a view keeps the value it was taken from after the root moves
//! on.

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

fn assert_refused(source: &str, fragment: &str) {
    let error = witchy::resolve_std_only_checked(source)
        .err()
        .unwrap_or_else(|| panic!("this shape must be refused: {source}"))
        .to_string();
    assert!(error.contains(fragment), "expected a refusal mentioning {fragment:?}, got: {error}");
}

/// A `&frozen Int` keeps its payload contract in every carrier position the row
/// names: a parameter, a nominal aggregate field, a generic, and an indirect
/// call through a function value.
#[test]
fn a_frozen_rooted_view_carries_through_every_admitted_position() {
    assert_canonical_and_wasm(
        "fn read(value: &frozen Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n: frozen Int = 79\n    read(&n)\n",
        79,
    );
    assert_canonical_and_wasm(
        "type Holder:\n    value: &frozen Int\n\nfn main() -> Int:\n    let n: frozen Int = 79\n    let holder = Holder(&n)\n    (*holder.value)\n",
        79,
    );
    assert_canonical_and_wasm(
        "fn pass[a](value: &a) -> &a:\n    value\n\nfn main() -> Int:\n    let n: frozen Int = 79\n    (*pass(&n))\n",
        79,
    );
    assert_canonical_and_wasm(
        "fn read(value: &frozen Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n: frozen Int = 79\n    let action = read\n    action(&n)\n",
        79,
    );
}

/// The view is a snapshot: it keeps the value it was taken from even after the
/// root it came from is assigned a different one. This is the copy-on-write
/// half of the contract, and it is what makes a view storable.
#[test]
fn a_rooted_view_keeps_its_snapshot_after_the_root_moves_on() {
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    var n = 79\n    let view = &n\n    n = 7\n    (*view)\n",
        79,
    );
}

/// An ownership qualifier belongs to the reference TARGET, never outside the
/// reference. `frozen &T` and `unique &T` are not spellings of anything.
#[test]
fn an_outer_qualifier_on_a_reference_is_refused() {
    assert_refused(
        "fn read(value: frozen &Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n = 7\n    read(&n)\n",
        "outer `frozen` reference qualifiers are invalid",
    );
    assert_refused(
        "fn read(value: unique &Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n = 7\n    read(&n)\n",
        "outer `unique` reference qualifiers are invalid",
    );
}

/// The two refusals that keep the frozen payload contract: a view is immutable,
/// and a frozen value is replaced whole rather than mutated in place.
#[test]
fn a_view_cannot_write_and_a_frozen_payload_cannot_be_mutated_in_place() {
    assert_refused(
        // Unparenthesised on purpose: this is the retired write-through
        // spelling, and it is the spelling the diagnostic is written for.
        "fn main() -> Int:\n    var n = 7\n    let view = &n\n    *view = 9\n    n\n",
        "write-through dereference is retired",
    );
    assert_refused(
        "type Box:\n    value: Int\n\nfn main() -> Int:\n    var b: frozen Box = Box(1)\n    b.value = 2\n    b.value\n",
        "cannot mutate through frozen value",
    );
}

/// The positive counterpart: whole replacement of a frozen root is the
/// supported mutation, and it is observable.
#[test]
fn a_frozen_root_is_replaced_whole() {
    assert_canonical_and_wasm(
        "type Box:\n    value: Int\n\nfn main() -> Int:\n    var b: frozen Box = Box(1)\n    b = Box(79)\n    b.value\n",
        79,
    );
}
