# calc

A tiny arithmetic evaluator: it tokenizes a string, parses it with operator
precedence (recursive descent), and evaluates the tree, reporting the first error
as a value. A compact tour of witchy's data side — recursive enums (`Token`,
`Expr`), exhaustive pattern matching, recursion, and `Result`-typed errors — all
its root footprint is just `Console`, so it runs identically interpreted,
compiled, and sandboxed.

**Shows:** recursive enums, `match`, recursion, `Result`/`Option`, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/calc/src/calc.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/calc
```
