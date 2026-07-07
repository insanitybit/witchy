# BUG-316: Record spread discards constructor name

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Component: records lowering, type checker, nominal typing

## Summary

Named record spread now carries the written record name through lowering. The
surface form `P(x: 5, ..base)` lowers to a `RecordUpdate` with target `P`, while
internal updates such as field-assignment desugaring keep an anonymous
same-as-base update.

The type checker now requires a named spread's base to have the same nominal
record type as the written constructor and keeps same-type spread valid. A
cross-type spread such as `P(x: 5, ..big)` where `big: Big` is rejected with a
direct diagnostic instead of silently producing a `Big`.

## Regression Coverage

- `record_spread_base_must_match_named_record` accepts same-type spread and
  rejects cross-type spread.
- Command-level repro now reports:
  `` `P(..base)` requires a `P` base, found `Big` ``.

## Validation

- `cargo check --workspace`
- `cargo test -p witchy-types record_spread_base_must_match_named_record -- --nocapture`
- `cargo test -p witchy-types`
- `cargo run --quiet -- check /tmp/witchy-record-spread-bad.witchy` (expected failure)
- `git diff --check`
