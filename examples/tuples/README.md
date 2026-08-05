# tuples

Tuples for multiple return values, destructuring, and element access. `pair.0`
reads an element by position — the expression form of `let (a, b) = pair` — and
chains through nesting (`grid.0.1`). The `divmod` helper is pure (`pub`); only
`main` touches the `Console`, so it runs identically interpreted, compiled, and
inside the capability sandbox.

**Shows:** tuple returns, `let` destructuring, positional `.0`/`.1` access
(including off a call and through nesting), and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/tuples/src/tuples.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/tuples
```
