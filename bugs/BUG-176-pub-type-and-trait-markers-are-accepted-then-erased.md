# BUG-176: Non-function `pub` markers are accepted then erased

Severity: MED
Status: FIXED
Found: 2026-07-06
Fixed: 2026-07-06

## Summary

The parser accepted `pub` before top-level non-function declarations such as
`type`, `sealed type`, `capability`, `trait`, and `impl`, but none of those AST
nodes stored visibility. Formatting erased the marker and docs rendered the
declarations according to their declaration kind, not the accepted `pub` spelling.

## Fix

Top-level `pub` is now rejected unless it starts a function declaration. Public
impl methods remain valid because method visibility is a separate, preserved
surface inside `impl` blocks.

Regression coverage:

- `crates/witchy-syntax/src/parser_tests.rs::pub_only_marks_function_declarations`
