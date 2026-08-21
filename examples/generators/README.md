# generators

Generators: `gen fn` + `yield` for lazy iteration. A `gen fn` runs like an
ordinary imperative function but `yield`s a sequence of values; calling it returns
a lazy `Iter` (std/iter) that computes only what's demanded, so an infinite
generator like `fibs` is fine when something bounds it (`iter.take`). `collatz` is
a finite generator with a branch in its loop. Data-only computation — only `main`
touches the `Console` — and it runs on both the interpreter and the compiled WASM
backend.

**Shows:** `gen fn`/`yield`, lazy `Iter` (`iter.take`/`iter.collect`/`iter.count`),
`while`/`if`, string building, and in-rune `test_*` functions.

## Run

```sh
witchy run                                          # from this directory
witchy examples/generators/src/generators.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/generators
```
