# jq

A tiny JSON query tool: it decodes a document and walks a dotted path into it,
where each segment is either an object key or (when it parses as a number) an
array index, then renders the selected value. A practical tour of `std/json`:
`decode`, `get` / `index`, and `encode`. `query` is pure (`pub`, no
capabilities); only `main` touches the `Console`.

**Shows:** `std/json` (`decode` / `get` / `index` / `encode`), `Option`
threading, `match`, and in-rune `test_*` functions.

## Run

```sh
witchy run                          # from this directory
witchy examples/jq/src/jq.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/jq/src/jq_test.witchy
```
