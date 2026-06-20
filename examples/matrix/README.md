# matrix

Integer matrices as a list of rows: `transpose`, `multiply` (the textbook triple
loop — dot each row of A with each column of B), and an `identity`, printed with
right-aligned columns. The math is pure (`pub`, no capabilities); only `main`
touches the `Console`, so the program runs identically on both backends.

**Shows:** nested `while` loops, `List(List(Int))`, `pub` functions across
modules, string padding/joining, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/matrix/src/matrix.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/matrix
```
