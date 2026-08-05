# queens

The N-queens puzzle by backtracking. A partial placement is a list where entry
`r` is the column of the queen in row `r`; a queen is safe if it shares no column
and no diagonal with those already placed. `count` totals every solution for the
8x8 board and `solve` returns the first one. The search is pure (`pub`, no
capabilities); only `main` touches the `Console`, so it runs identically
interpreted, compiled, and inside the capability sandbox.

**Shows:** recursion and backtracking, `while` loops, `List(Int)`, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/queens/src/queens.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/queens
```
