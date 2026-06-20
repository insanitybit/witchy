# try

The `?` operator propagates errors: `e?` unwraps an `Ok`/`Some` to its value, or
short-circuits — returning the `Err`/`None` straight out of the enclosing
function, whose return type must match. `Result` is an ordinary generic type
defined in the program here, not a built-in. The logic is pure (`pub`, no
capabilities); only `main` touches the `Console`.

**Shows:** the `?` operator, a user-defined generic `Result` type, `match`, and
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
