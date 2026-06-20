# result

A generic `Result` type built in the language, not by it: `Ok`/`Err` carry
inferred type parameters that the checker tracks per use. `safe_div` returns
`Err` on a zero divisor and `show` renders either case with `match`. The
conversions are pure; only `main` touches the `Console`.

**Shows:** generic `type` declarations, `match`, type-parameter inference,
`pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/result/src/result.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/result/src/result_test.witchy
```
