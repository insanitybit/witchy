# pascal

Pascal's triangle as an infinite generator: each `yield` emits a row, and the
next row is built from the last (sum of adjacent entries, 1s on the ends). A
`gen fn` carrying a `List(Int)` as state — the imperative shape, lazy result
(only the rows you `take` are built). Runs on both backends.

**Shows:** `gen fn`/`yield`, the `iter` module (`take`, `collect`), `while`
loops, the `list` module (`push`, `at`, `length`), `string.join`.

## Run

```sh
witchy run                                # from this directory
witchy examples/pascal/src/pascal.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/pascal/src/pascal_test.witchy
```
