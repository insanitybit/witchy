# try

The `?` operator propagates errors: `e?` unwraps an `Ok`/`Some` to its value, or
short-circuits — returning the `Err`/`None` straight out of the enclosing
function, whose return type must match. `Result` and `Option` are standard
prelude ADTs. The logic is data-only (`pub`, no capabilities); only `main` touches
the `Console`.

**Shows:** the `?` operator, standard generic `Result`, `match`, and
in-rune `test_*` functions.

## Run

```sh
witchy run                            # from this directory
witchy examples/try/src/try.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/try
```
