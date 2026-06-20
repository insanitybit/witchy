# sudoku

A backtracking Sudoku solver. The board is a 9x9 `List(List(Int))` (0 is empty);
`solve` finds the first empty cell, tries each digit consistent with its row,
column, and 3x3 box, and recurses — backtracking with `Option`'s `None` when a
cell admits no digit. Boards are immutable: each trial builds a fresh board with
one cell set. The solver is pure (`pub`, no capabilities); only `main` touches
the `Console`.

**Shows:** recursion and backtracking, `Option`/`match`, immutable
list-of-lists, tuple patterns, `pub` functions across modules, and in-rune
`test_*` functions.

## Run

```sh
witchy run                                    # from this directory
witchy examples/sudoku/src/sudoku.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/sudoku
```
