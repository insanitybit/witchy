# BUG-282: obsolete `cmp` list helpers survived after Eq-bound `list` helpers

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: std/cmp, std/list, std/set, API consistency, RFC-0046 cleanup

## Problem

`cmp.member`, `cmp.index_of`, `cmp.count`, and `cmp.unique` were temporary list
helpers kept only while `list.*` could not reliably carry `Eq` bounds through
compiled trait dispatch. After RFC-0046 and the later container equality fixes,
the canonical `list.contains`, `list.index_of`, `list.count`, and `list.unique`
helpers are content-correct on both backends.

Keeping the old quartet made the stdlib feel split-brained, and
`cmp.index_of -> Int` continued to teach the pre-RFC-0044 `-1` sentinel shape
next to `list.index_of -> Option(Int)`.

## Fix

The obsolete `cmp` list quartet is deleted. `std/list` owns the Eq-bound lookup,
count, and dedupe APIs; `std/set` and the equality example now use the canonical
`list` APIs, and release-facing docs no longer advertise `cmp.member` as a public
generic helper.

Validation:
- `cargo run --quiet -- check std/cmp.witchy`
- `cargo run --quiet -- check std/set.witchy`
- `cargo run --quiet -- check examples/equality/src/equality.witchy`
- `cargo run --quiet -- run examples/equality/src/equality.witchy`
- `cargo test stdlib_docs_are_current -- --nocapture`
- `cargo test all_std_modules_type_check -- --nocapture`
