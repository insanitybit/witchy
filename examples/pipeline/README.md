# pipeline

A small data pipeline composed from the `list` module's transforms: sum the
squares of the even numbers in `0..10`, then double and comma-join the first
five. Data-only, so it runs identically interpreted and compiled to WASM.

**Shows:** the `list` module (`range`, `filter`, `map`, `sum`), closures,
`list.join`, string interpolation.

## Run

```sh
witchy run                                      # from this directory
witchy examples/pipeline/src/pipeline.witchy    # or by file, from the repo root
```
