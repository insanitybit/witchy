//! RFC-0167 row F: the frozen/view qualifier matrix at the canonical boundary.
//!
//! Every positive fixture uses an independent scalar expectation, the canonical
//! admitted unoptimized Wasm, with a zero-leak audit on
//! both. The source interpreter is not consulted.
//!
//! Scope: rooted views admit the canonical `Int` carrier (row G records that
//! boundary and the `&Float` refusal), so these fixtures are `&Int` shaped. They
//! pin the FROZEN half of the matrix: an outer qualifier on a reference is
//! invalid, `&frozen Int` keeps its payload contract through every carrier
//! position, and a view keeps the value it was taken from after the root moves
//! on.

use super::source::checked;

fn assert_refused(source: &str, fragment: &str) {
    let error = checked(source)
        .err()
        .unwrap_or_else(|| panic!("this shape must be refused: {source}"))
        .to_string();
    assert!(error.contains(fragment), "expected a refusal mentioning {fragment:?}, got: {error}");
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
