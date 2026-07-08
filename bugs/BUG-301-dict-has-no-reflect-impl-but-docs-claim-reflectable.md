# BUG-301: `Dict` reflection is JSON-object-only and not a deserialize round trip

Severity: LOW
Status: FIXED
Verified: 2026-07-08 FIXED on worktree-wt-83874-1783508646
Component: `std/reflect.witchy`, generated `spec/stdlib.md`

## Resolution

The original report started as "Dict is documented reflectable but has no
`Reflect` impl"; that part was already fixed. The remaining ambiguity was
whether `Dict` reflection should preserve enough structure for generic
`derive(Deserialize)` round trips.

For 0.1, the policy is now explicit: `Dict` reflection is an encoding/debug
protocol. It reflects to a string-keyed `MRecord` so `json.stringify` emits a
JSON object and `reflect.debug` renders a record-like shape. It is intentionally
not a general reconstruction protocol for arbitrary `Dict(k, v)` values.

That means no `MDict` variant is required for 0.1; a future structured map
mirror would be a feature/design RFC, not this bug.

## Validation

```console
$ CARGO_TARGET_DIR=target-codex cargo test -p witchy stdlib_docs_are_current -- --nocapture
test example_tests::stdlib_docs_are_current ... ok
```
