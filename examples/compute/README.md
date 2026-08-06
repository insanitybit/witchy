# compute

A few small numeric functions — `double`, recursive `fib`, and `fact` via
`match` — summed and printed. The computations are pure (`pub`, no capabilities);
only `main` touches the `Console`, so the program runs identically interpreted,
compiled, and inside the capability sandbox.

**Shows:** recursion, `if`/`else` and `match` expressions, `pub` functions across
modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/compute/src/compute.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/compute
```
