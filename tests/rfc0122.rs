//! Negative parser census for syntax retired by RFC-0167.
//!
//! The executable immutable rooted-view corpus lives with the RFC-0167
//! examples. Keep these inputs here only to ensure the canonical parser does
//! not silently accept the superseded lifetime, mutable-reference, or
//! `View(T, 'a)` spellings.

use witchy_syntax::parser::parse_module;

#[test]
fn retired_reference_spellings_remain_rejected_by_the_canonical_parser() {
    for (label, source) in [
        ("explicit reference lifetime", "fn read(value: &'a Int) -> Int:\n    *value"),
        ("mutable reference", "fn read(value: &mut Int) -> Int:\n    1"),
        ("nominal lifetime view", "type Holder('a):\n    value: View(Int, 'a)"),
        ("lifetime-bearing let type", "fn read(value: let('a) Int) -> Int:\n    value"),
    ] {
        let error = parse_module(source).expect_err(label);
        assert!(
            error.message.contains("retired") || error.message.contains("unsupported"),
            "{label} produced an unclassified diagnostic: {error:?}"
        );
    }
}
