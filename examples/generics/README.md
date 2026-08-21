# generics

`swap` is generic over any pair `(a, b)` and returns `(b, a)`. The type
parameters are instantiated fresh at each call site, so the same function works
on a `(Int, String)` here and any other pair elsewhere. `swap` is data-only (`pub`,
no capabilities); only `main` touches the `Console`.

**Shows:** generic type parameters, tuple pattern matching, `let`
destructuring, and in-rune `test_*` functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/generics/src/generics.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/generics/src/generics_test.witchy
```
