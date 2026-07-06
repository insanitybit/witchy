# BUG-533: `comptime:` dropped source `from` imports while executing the block

- **Severity:** MED
- **Status:** FIXED
- **Component:** `comptime`, source `from` imports, RFC-0042 module bindings, `module_types`

## Problem

A module-level `comptime:` block executes as a synthetic `comptime` module. That
module kept the enclosing module's plain std imports, but discarded every
`from X import Y` binding. Source that could write `TypeInfo` after
`from meta import TypeInfo` therefore failed inside `comptime:` with a suggestion
to add an import that was already present.

That made compile-time code a smaller import language than ordinary Witchy
source.

## Resolution

The synthetic comptime program now preserves enclosing `from` imports whose
source module is in the bundled std set. Project-local sibling imports remain
filtered out, preserving the existing zero-capability/std-only comptime
isolation boundary.

The regression `comptime_preserves_std_from_imports_on_both_backends` covers
`from meta import TypeInfo` plus a comptime block that types
`module_types` as `List(TypeInfo)`.

## Verification

- `cargo test comptime_preserves_std_from_imports_on_both_backends -- --nocapture`
- `NEXTEST_TEST_THREADS=2 ./scripts/check.sh`

