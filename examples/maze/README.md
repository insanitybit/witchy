# maze

Breadth-first shortest path through a grid maze (`#` walls, `S` start, `E` exit).
BFS explores outward from `S` a ring at a time, so it reaches `E` by the shortest
route; a `prev` `Dict` (keyed by an encoded `row*width + col` position) records
how each cell was reached, then the route is walked back and marked with `*`.
Data-only apart from `main` (root footprint: `Console`).

**Shows:** BFS with a queue and a `Dict`, `match`-free grid helpers, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                              # from this directory
witchy examples/maze/src/maze.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/maze
```
