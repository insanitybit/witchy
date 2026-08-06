# eval

A tiny expression evaluator. `Expr` is a recursive algebraic data type
(`Num`/`Add`/`Mul`) and `eval` walks it with pattern matching — pure computation
that needs no capability. Only `main` touches the `Console`, to print the answer.

**Shows:** recursive ADTs, `match`, recursion, `pub` functions across modules, and
in-rune `test_*` functions.

## Run

```sh
witchy run                              # from this directory
witchy examples/eval/src/eval.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/eval
```
