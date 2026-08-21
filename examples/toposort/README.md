# toposort

Topological ordering of a dependency graph (Kahn's algorithm): an edge `(a, b)`
means "a must come before b", and we repeatedly emit a node whose prerequisites
are all already emitted. If we get stuck with nodes remaining, the graph has a
cycle and we return an `Err`. The graph is plain data and the logic is data-only
(`pub`, no capabilities); only `main` touches the `Console`.

**Shows:** `while` loops, `match` on `Option`/`Result`, tuple destructuring,
`pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/toposort/src/toposort.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/toposort
```
