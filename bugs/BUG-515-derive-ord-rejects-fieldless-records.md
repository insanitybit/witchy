# BUG-515: `derive(Ord)` should not accept fieldless types

Severity: LOW
Status: REJECTED
Verified: 2026-07-08 OBSOLETE on `worktree-wt-bug-515-close`
Component: `std/meta`, `derive(Ord)`, `derive(PartialOrd)`, fieldless type semantics

## Summary

The original report assumed `type Marker:` was a zero-field singleton record and
therefore should derive `PartialOrd`/`Ord` with every value comparing equal.

That premise is no longer valid. Witchy now treats `type Marker:` as a
fieldless, uninhabited type with no constructor. Since there is no `Marker()`
value to compare, accepting structural derives for that shape would reintroduce
the exact model split BUG-517 closed.

The current behavior is therefore intentional:

- `Marker()` is rejected because `Marker` is a type, not a value;
- an empty match over `Marker` is exhaustive because the type is uninhabited;
- built-in structural derives reject fieldless types with a direct diagnostic;
- singleton marker records, if desired later, need an explicit syntax/design
  separate from `type Marker:`.

## Evidence

Superseded by the fieldless-type decision fixed in `9d7cb90b`
(`language: pin fieldless types as uninhabited`).

Regression coverage:

- `typeck::tests::fieldless_types_are_uninhabited_and_builtin_derives_reject`
  proves the fieldless syntax has no constructor and rejects built-in derives,
  including `PartialOrd` and `Ord`.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy-types fieldless_types_are_uninhabited_and_builtin_derives_reject -- --nocapture
```

Result: passed, 1 test.

## Residuals

None for this bug. The broader product question is whether Witchy wants an
explicit singleton/marker-record syntax in the future; that would be new design
work, not a bug in `derive(Ord)`.
