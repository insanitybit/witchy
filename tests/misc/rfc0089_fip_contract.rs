//! RFC-0089 functional-in-place source-contract conformance.
//!
//! The FIP kernel's RESOURCE theorem (equal allocator/reclaimer counters at
//! depth) is proven in src/stats.rs; the WIR loop structure in wir_opt_tests.
//! This file pins the CHECKED SOURCE CONTRACT itself (acceptance criterion 5):
//! each rejection class in the RFC's "Checked kernel body" list produces a
//! source-located miss with its documented reason, structurally similar
//! functions that are NOT candidates produce no miss, and the canonical
//! accepted kernel is value-correct at contract depth on both backends.
//!
//! `analysis::module_fip_misses` is the single analysis every consumer (CLI,
//! browser compile, stats, LSP) shares, so asserting on it here pins all of
//! them; the CLI/browser message formatting is covered by src/lib.rs and
//! src/lsp_tests.rs.

use witchy_lower::analysis;
use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

fn linked(source: &str) -> witchy::ast::Module {
    let parsed = parser::parse_module(source).expect("parse FIP program");
    let linked =
        pipeline::link(vec![("main".into(), parsed)], "main").expect("link FIP program");
    typeck::check(&linked).expect("typecheck FIP program");
    linked
}

/// Assert the module misses the FIP contract in function `function` with a
/// reason containing `reason_fragment`.
fn assert_fip_miss(source: &str, function: &str, reason_fragment: &str) {
    let misses = analysis::module_fip_misses(&linked(source));
    assert!(
        misses
            .iter()
            .any(|miss| miss.function.ends_with(function)
                && miss.reason.contains(reason_fragment)),
        "expected a `{function}` miss mentioning {reason_fragment:?}, got: {misses:?}"
    );
}

fn assert_no_fip_miss(source: &str) {
    let misses = analysis::module_fip_misses(&linked(source));
    assert!(misses.is_empty(), "expected no FIP misses, got: {misses:?}");
}

// The canonical scalar state kernel is accepted and value-correct at the
// contract's checked depth (50,000 transitions) on both backends.
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

    let linked = linked(source);
    let want = interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret");
    assert_eq!(want, vec!["50000 42788".to_string()]);

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("kernel lowers");
    let mut runtime = Runtime::batch().expect("runtime");
    let caps = Capabilities { print: true, quiet: true, ..Default::default() };
    let mut actor = runtime.spawn(&wasm, caps, 256).expect("spawn");
    actor.run().expect("compiled kernel execution");
    assert_eq!(actor.output(), want, "compiled kernel parity at contract depth");
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

// A replacement exit returns a freshly constructed record instead of the owner:
// both the allocation and the non-owner return are misses.
#[test]
fn replacement_exit_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return Cursor(99)
    cursor.offset = cursor.offset + 1
    scan(cursor, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0), 2).offset}")
"#;
    assert_fip_miss(source, "scan", "aggregate construction allocates");
    assert_fip_miss(source, "scan", "does not return the owned value directly");
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
    assert_fip_miss(source, "scan", "escapes outside a field read or tail return");
}

// Loops and ranges are outside the initial kernel shape.
#[test]
fn loops_inside_the_kernel_are_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    var i = 0
    while i < 2:
        cursor.offset = cursor.offset + 1
        i = i + 1
    scan(cursor, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0), 2).offset}")
"#;
    assert_fip_miss(source, "scan", "loops and ranges are outside");
}

// The owned record's stored fields must all be scalar.
#[test]
fn heap_valued_record_field_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int
    name: String

fn scan(own cursor: unique Cursor, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    cursor.offset = cursor.offset + 1
    scan(cursor, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0, "x"), 2).offset}")
"#;
    assert_fip_miss(source, "scan", "record whose stored fields are all scalar");
}

// Every auxiliary parameter must be scalar.
#[test]
fn heap_valued_auxiliary_parameter_is_rejected() {
    let source = r#"
type Cursor:
    offset: Int

fn scan(own cursor: unique Cursor, let tag: String, n: Int) -> unique Cursor:
    if n == 0:
        return cursor
    cursor.offset = cursor.offset + 1
    scan(cursor, tag, n - 1)

fn main(console: Console):
    console.print("${scan(Cursor(0), "t", 2).offset}")
"#;
    assert_fip_miss(source, "scan", "auxiliary parameter `tag` is not scalar");
}

// A non-recursive consume-and-return helper with the same ownership shape does
// NOT opt in: direct recursion identifies the kernel.
#[test]
fn non_recursive_owner_shape_is_not_a_candidate() {
    let source = r#"
type Cursor:
    offset: Int

fn tweak(own cursor: unique Cursor) -> unique Cursor:
    cursor.offset = cursor.offset + 1
    cursor

fn main(console: Console):
    console.print("${tweak(Cursor(0)).offset}")
"#;
    assert_no_fip_miss(source);
}

// A Result-wrapped return is not `unique T` of the owner's type, so the
// function is outside the contract even though it recurses with a `?` helper.
#[test]
fn result_wrapped_owner_shape_is_not_a_candidate() {
    let source = r#"
type Cursor:
    offset: Int

fn peek(n: Int) -> Result(Int, String):
    Ok(n)

fn scan(own cursor: unique Cursor, n: Int) -> Result(unique Cursor, String):
    if n == 0:
        return Ok(cursor)
    let v = peek(n)?
    cursor.offset = cursor.offset + v
    scan(cursor, n - 1)

fn main(console: Console):
    match scan(Cursor(0), 2):
        Ok(c) -> console.print("${c.offset}")
        Err(e) -> console.print(e)
"#;
    assert_no_fip_miss(source);
}
