# guard

`return` exits a function early — handy for guard clauses (`classify` bails out
the moment it knows the answer) and for stopping a loop once it has found one
(`first_even`). Both functions are data-only (`pub`, no capabilities); only `main`
touches the `Console`.

**Shows:** early `return`, guard clauses, `for` loops, and in-rune `test_*`
functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/guard/src/guard.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/guard/src/guard_test.witchy
```
