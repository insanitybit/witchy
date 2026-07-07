# BUG-175: `pub let` is accepted but has no public semantics

Severity: MED
Status: FIXED
Found: 2026-07-06
Fixed: 2026-07-06

## Summary

The parser accepted `pub let NAME = ...`, but module constants have no visibility
bit, are not importable as public API, and were formatted back to plain `let`.
That made `pub fn` meaningful while `pub let` was silently ignored.

## Fix

Top-level `pub` now only precedes function declarations: `pub fn`,
`pub gen fn`, or `pub async fn`. `pub let` is a parse error with a direct
diagnostic instead of being erased later by formatting.

Regression coverage:

- `crates/witchy-syntax/src/parser_tests.rs::pub_only_marks_function_declarations`
