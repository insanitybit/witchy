# result

A generic prelude `Result`: `Ok`/`Err` carry inferred type parameters that the
checker tracks per use. `safe_div` returns `Err` on a zero divisor and `show`
renders either case with `match`. The conversions are pure; only `main` touches
the `Console`.

**Shows:** standard `Result`, `match`, type-parameter inference,
`pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/result/src/result_demo.witchy # or by file, from the repo root
```

## Test

```sh
witchy test examples/result/src/result_test.witchy
```
