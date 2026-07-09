# BUG-537: DateTime year domain outlives ISO rendering

Severity: LOW
Status: FIXED
Verified: 2026-07-08 fixed on master 2994a19c
Fixed: 2026-07-07
Component: `std/time`, `DateTime` invariants, ISO 8601 formatting/parsing

## Summary

This row is a stale duplicate of
`bugs/BUG-537-datetime-fixed-iso-domain.md`. The bug was real when recorded:
`DateTime` was sealed, but public constructors still accepted years outside the
fixed ISO rendering/parsing domain.

The current implementation chooses the small coherent 0.1.0 contract:
`DateTime` values exposed by `std/time` live in the fixed four-digit CE year
domain `1..9999`.

## Current Behavior

- `std/time.witchy` rejects `time.civil(...)` years outside `1..9999`.
- `std/time.witchy` rejects `time.from_unix(...)` timestamps whose computed
  civil year is outside `1..9999`.
- `time.iso8601(...)` and `time.parse_iso8601(...)` now share the same fixed
  four-digit CE year contract.
- `spec/stdlib.md` documents the `from_unix` fixed ISO domain.

Regression coverage:

- `example_tests::datetime_rejects_years_outside_fixed_iso_domain_on_both_backends`
- `example_tests::stdlib_docs_are_current`

Focused verification on 2026-07-08:

```text
$ CARGO_TARGET_DIR=target-codex-docs cargo test datetime_rejects_years_outside_fixed_iso_domain_on_both_backends -- --nocapture
test example_tests::datetime_rejects_years_outside_fixed_iso_domain_on_both_backends ... ok

$ CARGO_TARGET_DIR=target-codex-docs cargo test stdlib_docs_are_current -- --nocapture
test example_tests::stdlib_docs_are_current ... ok
```

## Source Evidence

- `std/time.witchy:1-7` describes the module as using a proleptic Gregorian
  conversion correct for CE dates and a fixed ISO contract.
- `std/time.witchy:51-57` rejects `from_unix(...)` results outside `1..9999`.
- `std/time.witchy:65-68` rejects `civil(...)` years outside `1..9999`.
- `spec/stdlib.md` repeats the fixed ISO parse/format contract.

## Why This Matters

Sealing `DateTime` was the right direction: users should get time values through
constructors that establish the invariants the rest of the module relies on.
This bug is the next layer down. Once a type is sealed, the public constructors
become the whole trust boundary, so they need to define the value domain as
carefully as the representation was hidden.

That trust boundary now exists. This row can close; keep the duplicate fixed row
as the concise historical record.
