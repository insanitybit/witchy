# life

Conway's Game of Life on a fixed grid. The board is rows of cells
(`List(List(Bool))`); each generation applies the B3/S23 rule (a cell is born
with exactly 3 live neighbours and survives with 2 or 3). A glider drifts down
and to the right.

**Shows:** nested `while` loops, `List(List(Bool))` grids, boolean logic, string
building, and in-rune `test_*` functions. Data-only apart from `main` (root footprint: `Console`), so it runs
identically interpreted and compiled to WASM.

## Run

```sh
witchy run                              # from this directory
witchy examples/life/src/life.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/life
```
