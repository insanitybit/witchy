# BUG-517: Fieldless types are consistently uninhabited

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-bug-517-close`
Component: fieldless types, exhaustiveness checking, `std/meta`, compile-time reflection, derives

## Summary

Witchy now uses one coherent model for declarations such as `type Marker:`:
they are fieldless, uninhabited types with no constructor.

That resolves the old contradiction where `Marker` / `Marker()` could be treated
as a value even though exhaustiveness checking and `module_types` modeled the
type as having no constructors. Under the chosen model:

- an empty match over `Marker` is exhaustive because the type is uninhabited;
- `Marker()` is rejected with ``type `Marker` is not a value``;
- built-in structural derives reject fieldless types instead of emitting empty
  `match self:` implementations; and
- `meta.TypeInfo.kind == "unit"` for fieldless types matches the source
  semantics instead of hiding a constructible singleton.

## Evidence

Fixed by `9d7cb90b` (`language: pin fieldless types as uninhabited`) and the
earlier `9c2f249` check-time rejection for treating type names as values.

Regression coverage:

- `typeck::tests::fieldless_types_are_uninhabited_and_builtin_derives_reject`
  covers empty-match exhaustiveness for an uninhabited fieldless type, rejection
  of `Marker()`, and rejection of built-in derives `Show`, `PartialEq`, `Eq`,
  `PartialOrd`, `Ord`, `Reflect`, and `Deserialize`.

Validation on 2026-07-08:

```console
CARGO_TARGET_DIR=target-codex cargo test -p witchy-types fieldless_types_are_uninhabited_and_builtin_derives_reject -- --nocapture
```

Result: passed, 1 test.

## Residuals

This closes the model-consistency bug for the fieldless syntax. If Witchy later
wants singleton marker records, that should be a new explicit syntax/design RFC
rather than reviving `type Marker:` as both constructible and uninhabited.
