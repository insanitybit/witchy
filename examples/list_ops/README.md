# list_ops

With first-class functions and `push` (which returns a new list with an element
appended), the usual list combinators are just ordinary witchy functions. `map`
and `filter` are generic over their element types, and `sum`/`join` fold a list
down to a single value.

**Shows:** generic functions, first-class function parameters and closures,
`list.push`, and in-rune `test_*` functions. Only `main` touches the `Console`.

## Run

```sh
witchy run                                      # from this directory
witchy examples/list_ops/src/list_ops.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/list_ops
```
