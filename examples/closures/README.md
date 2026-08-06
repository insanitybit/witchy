# closures

Higher-order functions and capturing closures that cross the compiled boundary. A
function value is a heap record `[code_index][captured...]`: `apply` calls a
`fn(Int) -> Int` value, `twice` threads one through two layers, and `adder(by)`
returns a closure that captures `by`. The function-building helpers are pure
(`pub`); only `main` touches the `Console`.

**Shows:** function-typed parameters, lambdas, capturing closures, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                    # from this directory
witchy examples/closures/src/closures.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/closures
```
