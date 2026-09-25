//! RFC-0167 row D: `own` on a view carrier consumes the carrier, not the payload.
//!
//! Positive fixtures use an independent scalar expectation, the canonical
//! admitted unoptimized Wasm, with a zero-leak audit on
//! both. The source interpreter is not consulted.
//!
//! The distinction the row is about: `own value: &T` is an affine hold on the
//! CARRIER binding. Consuming it says nothing about the root the view was taken
//! from, which stays live and can be viewed again.

fn refusal(source: &str) -> String {
    witchy::resolve_std_only_checked(source)
        .err()
        .unwrap_or_else(|| panic!("this shape must be refused: {source}"))
        .to_string()
}

/// The carrier itself IS affine: naming it twice is refused. This is the
/// counterpart to the root staying live - `own` holds the binding, not the
/// payload - and the refusal must not depend on the result annotation.
#[test]
fn a_consumed_view_carrier_cannot_be_named_again_with_or_without_an_annotation() {
    let body = "fn main() -> Int:\n    let n = 79\n    let view = &n\n    let a = forward(view)\n    let b = forward(view)\n    (*a) + (*b) - 79\n";
    let annotated = refusal(&format!("fn forward(own value: &Int) -> &Int:\n    value\n\n{body}"));
    let inferred = refusal(&format!("fn forward(own value: &Int):\n    value\n\n{body}"));
    for error in [&annotated, &inferred] {
        assert!(
            error.contains("use of `view` after it was moved (consumed by an `own` parameter)"),
            "the refusal must name the consumed carrier: {error}"
        );
    }
    assert_eq!(
        annotated, inferred,
        "a missing result annotation cannot change what the call consumes"
    );
}
