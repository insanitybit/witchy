# generic_stack

Parametric polymorphism: one data structure, any element type. `Stack(a)` is a
recursive generic ADT — the type parameter `a` flows through the constructors
(`Push(a, Stack(a))`) and `size`/`peek`/`reverse` work for any `a`, fully
type-checked, with `peek` returning a generic `Option(a)`. The same operations run
on a `Stack(Int)` and a `Stack(String)` alike. Data-only (only `main` touches the
`Console`), so it runs identically interpreted, compiled, and inside the
capability sandbox.

**Shows:** generic recursive ADTs, type parameters, `match`, recursion,
`Option(a)`, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                                  # from this directory
witchy examples/generic_stack/src/generic_stack.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/generic_stack
```
