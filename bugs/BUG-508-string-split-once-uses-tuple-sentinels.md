# BUG-508: `string.split_once` uses tuple sentinels instead of `Option`

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on master 14e9f142
Component: `std/string`, string parsing helpers, RFC-0044 error policy

## Resolution

The original report asked for explicit fallible split helpers and a decision on
the legacy tuple-returning functions. That design has shipped:

- `string.split_once_opt(s, sep) -> Option((String, String))`
- `string.rsplit_once_opt(s, sep) -> Option((String, String))`

The old `split_once` and `rsplit_once` functions remain as documented
compatibility wrappers. That is a deliberate 0.1 surface decision, not an open
sentinel bug: parsers and validators that need to distinguish a missing
separator from a present empty side use the `_opt` variants.

## Evidence

- `std/string.witchy` exposes the `_opt` helpers and documents the tuple helpers
  as compatibility wrappers that should not be used when absence matters.
- `spec/stdlib.md` publishes the same API and recommendation.
- `projects/coven/src/coven_validate.witchy` uses `string.split_once_opt`
  instead of a private `contains` + `split_once` wrapper.
- `src/example_tests.rs` has `string_split_once_option_helpers_backends_agree`,
  which checks both interpreter and compiled backend behavior for missing and
  present-empty separators.

## Validation

```console
$ CARGO_TARGET_DIR=target-codex cargo test string_split_once_option_helpers_backends_agree -- --nocapture
test example_tests::string_split_once_option_helpers_backends_agree ... ok
```
