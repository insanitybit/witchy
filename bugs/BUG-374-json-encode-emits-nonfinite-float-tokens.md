# BUG-374: JSON encode silently maps non-finite Float values to null

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on worktree-wt-3311-1783509246
Component: `std/json`, reflective JSON encoding, application/json helpers

## Resolution

`std/json` now treats non-finite floats as a loud JSON-boundary contract error.
`JsonFloat(NaN)`, `JsonFloat(inf)`, `JsonFloat(-inf)`, and reflective
`Float` fields all route through `encode_float`, which fails with:

```text
json.encode: non-finite Float cannot be encoded as JSON
```

This is the coherent 0.1 policy: JSON has no NaN/Infinity tokens, and `null`
already represents intentional JSON null / `Option.None`, so the strict encoder
must not erase numeric failure into `null`.

## Evidence

- `std/json.witchy`'s `encode_float` checks `is_finite` and calls `fail(...)`
  for non-finite values.
- `json.stringify` reflects `MFloat` to `JsonFloat`, so reflective serialization
  uses the same strict path.
- `server.json_value` and `server.send` route through `json.encode` /
  `json.stringify`, so application/json helpers inherit the strict boundary.
- `src/example_tests.rs` has
  `json_nonfinite_float_encoding_aborts_on_both_backends`, covering direct NaN,
  direct infinity, and reflective record stringify on the interpreter and
  compiled WASM backend.

## Validation

```console
$ CARGO_TARGET_DIR=target-codex cargo test -p witchy json_nonfinite_float_encoding_aborts_on_both_backends -- --nocapture
test example_tests::json_nonfinite_float_encoding_aborts_on_both_backends ... ok
```
