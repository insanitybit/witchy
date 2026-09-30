//! RFC-0089 functional-in-place source-contract conformance.
//!
//! The FIP kernel's RESOURCE theorem (equal allocator/reclaimer counters at
//! depth) is proven in src/stats.rs; the WIR loop structure in wir_opt_tests.
//! This file pins the CHECKED SOURCE CONTRACT itself (acceptance criterion 5):
//! each rejection class in the RFC's "Checked kernel body" list produces a
//! source-located miss with its documented reason, structurally similar
//! functions that are NOT candidates produce no miss, and the canonical
//! accepted kernel is value-correct at contract depth on admitted Wasm.
//!
//! `analysis::module_fip_misses` is the single analysis every consumer (CLI,
//! browser compile, stats, LSP) shares, so asserting on it here pins all of
//! them; the CLI/browser message formatting is covered by src/lib.rs and
//! src/lsp_tests.rs.

use witchy_lower::analysis;
use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source).expect("check FIP source")
}

/// Assert the module misses the FIP contract in function `function` with a
/// reason containing `reason_fragment`.
fn assert_fip_miss(source: &str, function: &str, reason_fragment: &str) {
    let program = checked(source);
    let misses = analysis::module_fip_misses(program.module());
    assert!(
        misses
            .iter()
            .any(|miss| miss.function.ends_with(function)
                && miss.reason.contains(reason_fragment)),
        "expected a `{function}` miss mentioning {reason_fragment:?}, got: {misses:?}"
    );
}

/// Assert the program does not typecheck, with `reason_fragment` in the error.
/// Used where RFC-0167's affine rule refuses a shape the FIP analysis used to
/// classify: a stronger, earlier refusal than a contract miss.
fn assert_type_error(source: &str, reason_fragment: &str) {
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("the checker must refuse this shape");
    assert!(
        error.to_string().contains(reason_fragment),
        "expected a refusal mentioning {reason_fragment:?}, got: {}",
        error
    );
}

fn assert_no_fip_miss(source: &str) {
    let program = checked(source);
    let misses = analysis::module_fip_misses(program.module());
    assert!(misses.is_empty(), "expected no FIP misses, got: {misses:?}");
}

// The canonical scalar state kernel is accepted and value-correct at the
// contract's checked depth (50,000 transitions) on admitted Wasm.
#[test]
fn canonical_kernel_is_accepted_and_correct_at_depth() {
    let source = r#"
type Cursor:
    offset: Int
    checksum: Int

fn scan(own cursor: unique Cursor, remaining: Int) -> unique Cursor:
    if remaining == 0:
        return cursor
    cursor.checksum = (cursor.checksum * 33 + remaining) % 65521
    cursor.offset = cursor.offset + 1
    scan(cursor, remaining - 1)

fn main(console: Console):
    let done = scan(Cursor(0, 0), 50000)
    console.print("${done.offset} ${done.checksum}")
"#;
    assert_no_fip_miss(source);

    let checked = checked(source);
    let want = vec!["50000 42788".to_string()];

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted FIP kernel lowers");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities { print: true, quiet: true, ..Default::default() };
    let mut actor = runtime.spawn_admitted(&artifact, caps, 256).expect("spawn admitted kernel");
    actor.run().expect("compiled kernel execution");
    assert_eq!(actor.output(), want, "compiled kernel at contract depth");
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
}

// Non-tail recursion: the recursive result is bound and reworked, so the owner
// does not flow back directly and the edge is not the final expression.
#[test]
fn non_tail_recursion_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    var c = scan(cursor, n - 1)
    c.offset = c.offset + 1
    c

fn main(console: Console):
    console.print("${scan(Cursor(0), 2).offset}")
"#;
    assert_fip_miss(source, "scan", "does not return the owned value directly");
    assert_fip_miss(source, "scan", "recursive edge as the function's final expression");
}

// Calls other than the direct recursive edge may allocate or perform effects.
#[test]
fn helper_call_inside_the_kernel_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn bump(x: Int) -> Int:
    x + 1

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    cursor.offset = bump(cursor.offset)
    scan(cursor, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0), 2).offset}")
"#;
    assert_fip_miss(source, "scan", "may allocate or perform an effect");
}

// Aliasing the owner is an escape: only field reads and the tail return may
// consume it.
#[test]
fn owner_escape_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    let alias = cursor
    cursor.offset = alias.offset + 1
    scan(cursor, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0), 2).offset}")
"#;
    // RFC-0167 refuses this before the FIP analysis ever sees it: `let alias =
    // cursor` transfers the unique owner, so the later `cursor.offset` write is
    // a use after transfer. That is the same escape the FIP contract names,
    // caught earlier and by the language rule rather than by a kernel-shape
    // analysis, so this pins the refusal where it now happens.
    assert_type_error(source, "use of unique value `cursor` after it was transferred");
}
