# bst

A binary search tree built as a recursive algebraic data type (`Leaf` / `Node`)
and walked with pattern matching and recursion. Inserting a list and reading it
back in order is a tree sort. The data functions are data-only (`pub`, no
capabilities); only `main` touches the `Console`, so the program runs identically
interpreted, compiled, and inside the capability sandbox.

**Shows:** recursive enums, `match`, recursion, closures (`list.map`), `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                              # from this directory
witchy examples/bst/src/bst.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/bst
```
