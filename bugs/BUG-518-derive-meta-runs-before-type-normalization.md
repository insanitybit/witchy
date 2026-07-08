# BUG-518: Derive/meta uses normalized type facts

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-bug-518-close`
Component: `derive(...)`, `std/meta`, type aliases, implicit generic params, frontend pass order

## Summary

`derive(...)` and comptime reflection now consume normalized type information
instead of the pre-typecheck surface parse.

The normalized `meta.TypeInfo` facts now include:

- alias-expanded field types;
- inferred implicit record type parameters; and
- structured `meta.TypeExpr` field facts, with `meta.type_source(...)` available
  only at the source-generation boundary.

That means derived code and user comptime generators see the same type facts the
checker uses. A field declared through a type alias derives against the expanded
type, and an implicit-generic record derives a parameterized impl with the
expected bounds.

## Evidence

Fixed by `2cde5c17` (`comptime: normalize TypeInfo facts`).

Regression coverage:

- `example_tests::comptime_typeinfo_normalizes_aliases_and_implicit_params_on_both_backends`
  proves `module_types` exposes alias-expanded field types and inferred generic
  params to comptime code on the interpreter and compiled backend.
- `example_tests::derives_use_normalized_typeinfo_on_both_backends` proves
  built-in derives consume the same normalized `TypeInfo`, covering alias-typed
  `Deserialize` fields and implicit-generic `Reflect` impl generation.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy typeinfo -- --nocapture
```

Result: passed, including the two BUG-518 regression tests above.

## Residuals

This closes the pass-order bug. It does not by itself claim that every
user-extensible derive is ergonomically complete; broader TypeInfo design and
derive API quality should stay under the RFC-0067/RFC-0069 coherence work.
