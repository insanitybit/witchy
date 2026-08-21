# regions

> **Unstable:** `region:` is an experimental performance feature. The compiler
> warns when it is used, and its syntax or performance contract may change or be
> removed. Measure it against ownership annotations and inferred reclamation.

`region:` gives short-term allocations an explicit lifetime: everything
allocated inside is reclaimed at the block's end, and the block's value is what
escapes — deep-copied out, except values from outside the region, which are
shared rather than copied. A region never changes behavior, only when memory is
freed, so it runs identically interpreted and compiled.

**Shows:** `region ->` blocks, `let`-borrow parameters, `List(String)` building,
and `pub` functions across modules with in-rune `test_*` functions.

## Run

```sh
witchy run                                    # from this directory
witchy examples/regions/src/regions.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/regions
```
