# BUG-338: Tagged-literal escaped dollars resurrect as interpolation

Severity: HIGH
Status: FIXED
Found: 2026-07-06
Fixed: 2026-07-06

## Summary

An escaped `\${...}` inside a tagged literal was treated as static text by the
tag lexer, but if the tag emitted that text into generated Witchy source, the
generated-source parse could see raw `${...}` and turn it into a new interpolation
resolved at the call site. This violated tagged-literal hygiene and affected
security-critical source-emitting tags such as glamour `html`.

## Fix

Tag-generated expression source is now sanitized before the throwaway parse:
raw `${...}` inside generated string literals is escaped as `\${...}`. The
original hole parser is unchanged, so actual call-site holes keep normal Witchy
expression and interpolation semantics.

Regression coverage:

- `crates/witchy-interp/src/tagged.rs::tests::generated_string_interpolation_is_escaped`
- `src/example_tests.rs::example_tests::tagged_literal_escaped_dollar_stays_literal_on_both_backends`
