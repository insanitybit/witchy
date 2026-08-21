# higher_order_sum

"Find the sum of all the squared odd numbers under 1000" — the higher-order
functions example from Rust by Example, done two ways: an imperative loop and a
functional pipeline over `std/list` (`map` / `take_while` / `filter` / `sum`).
Both are data-only (`pub`, no capabilities) and agree on every backend; only `main`
touches the `Console`.

**Shows:** higher-order functions, closures, `std/list` pipelines, `while`
loops, and in-rune `test_*` functions.

## Run

```sh
witchy run                                                      # from this directory
witchy examples/higher_order_sum/src/higher_order_sum.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/higher_order_sum/src/higher_order_sum_test.witchy
```
