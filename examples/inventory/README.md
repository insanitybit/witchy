# inventory

Iterating a `Dict`: `dict.values` walks the values and `dict.pairs` walks
`(key, value)` entries (destructured with `let (k, v) = ...`), so you can
aggregate over a map's contents — not just look keys up. Results are reported
with string interpolation.

**Shows:** `Dict` iteration with `values` / `pairs`, tuple destructuring,
string interpolation, and a capability-gated `Console`.

## Run

```sh
witchy run                                        # from this directory
witchy examples/inventory/src/inventory.witchy    # or by file, from the repo root
```
