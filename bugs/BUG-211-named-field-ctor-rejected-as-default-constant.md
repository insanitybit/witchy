# BUG-211: Named-field record construction is rejected as a default constant

Severity: LOW
Status: FIXED
Found: 2026-07-06
Fixed: 2026-07-06

## Summary

RFC-0056 parameter defaults accepted positional constructors of closed constants
but rejected the equivalent named-field record syntax, e.g. `Pt(x: 0)`, even
though record lowering turns it into the same constructor shape before type
checking and backend lowering.

## Fix

The parser's closed-constant predicate now accepts named-field record
construction when every field value is closed and no spread is present. Spread
remains rejected because it references another value.

Regression coverage:

- `src/example_tests.rs::example_tests::keyword_args_default_accepts_named_field_record_constructor`
