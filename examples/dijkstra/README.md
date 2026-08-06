# dijkstra

Single-source shortest paths in a weighted directed graph. Nodes are indices and
the graph is an edge list of `(from, to, weight)`; we settle the nearest unsettled
node, relax its outgoing edges, then reconstruct one shortest path from the
predecessor table. `dijkstra` and `path_to` are pure (`pub`, no capabilities); only
`main` touches the `Console`, so it runs identically interpreted, compiled, and
inside the capability sandbox.

**Shows:** `while` loops, tuple destructuring, lists of tuples, `pub` functions
across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/dijkstra/src/dijkstra.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/dijkstra
```
