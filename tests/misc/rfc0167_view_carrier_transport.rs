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

/// Callable capture, which RFC-0167 permits in as many words: a view "may be
/// returned, stored in an aggregate, transported through a generic, and
/// captured by a callable without naming a lifetime", because the carrier
/// retains the root while any copy of the view is live.
#[test]
fn a_callable_captures_a_view_without_naming_a_lifetime() {
    assert_canonical_and_wasm(
        "fn main() -> Int:\n    let n = 79\n    let view = &n\n    let f = fn() -> Int: (*view)\n    f()\n",
        79,
    );
}

/// The explicitly written `let` convention on a view parameter, in both modes.
///
/// `Convention::Borrow` is the written `let`; `Convention::Let` is the default
/// one. Both are non-owning reads of the same GC-carried view, but only the
/// latter was listed in the ABI's ownership rule, so `fn read(let value: &Int)`
/// was refused as a signature mismatch - in ordinary mode as well as `mode opt`,
/// which is why this was never the opt-mode blocker it was recorded as.
#[test]
fn a_written_let_convention_carries_a_view_in_both_modes() {
    assert_canonical_and_wasm(
        "fn read(let value: &Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n = 79\n    read(&n)\n",
        79,
    );
    assert_canonical_and_wasm(
        "mode opt\n\nfn read(let value: &Int) -> Int:\n    (*value)\n\nfn main() -> Int:\n    let n = 79\n    read(&n)\n",
        79,
    );
}

fn assert_bundle_canonical_and_wasm(sources: Vec<(&str, &str)>, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        sources.into_iter().map(|(name, src)| (name.to_owned(), src.as_bytes().to_vec())).collect(),
        "main",
        witchy_interp::comptime::expand_compile_time,
    )
    .expect("cross-mode bundle checks");
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
    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)]);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

/// Trait-mediated transport: the view crosses a trait method and comes back.
#[test]
fn a_view_crosses_a_trait_boundary() {
    assert_canonical_and_wasm(
        "trait Reader:\n    fn project(self: Self, value: &Int) -> &Int\n\ntype Identity:\n    Identity\n\nimpl Reader for Identity:\n    fn project(self: Identity, value: &Int) -> &Int:\n        value\n\nfn main() -> Int:\n    let n = 79\n    (*Identity.project(&n))\n",
        79,
    );
}

/// Cross-mode transport: an ordinary caller hands a view to an opt callee.
#[test]
fn a_view_crosses_a_mode_boundary() {
    assert_bundle_canonical_and_wasm(
        vec![
            ("helper", "mode opt\n\npub fn read(value: &Int) -> Int:\n    (*value)\n"),
            ("main", "import helper\n\nfn main() -> Int:\n    let n = 79\n    helper.read(&n)\n"),
        ],
        79,
    );
}
