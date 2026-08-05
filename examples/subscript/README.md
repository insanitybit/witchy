# subscript

Subscript indexing: `xs[i]` reads the i-th element of a list (sugar for
`list.at(xs, i)`) and chains for nested lists (`grid[r][c]`). A dot product and a
matrix diagonal sum read everything by index. The computations are pure (`pub`,
no capabilities); only `main` touches the `Console`, so the program runs
identically interpreted, compiled, and inside the capability sandbox.

**Shows:** subscript and chained subscript indexing, `while` loops, nested
lists, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                          # from this directory
witchy examples/subscript/src/subscript.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/subscript
```
