# opt_mode

A single file under `mode opt`, witchy's fast-path discipline. Heap-typed
parameters must declare an ownership convention (`let` immutable borrow, `var`
mutate-and-write-back, or `own`), and any accumulation that would silently fall
back to O(n²) copying becomes a compile error instead of a quiet slowdown. The
mode is transitive: an `opt` module may only import other `opt` modules (the
bundled std library is exempt).

**Shows:** `mode opt`, `let` heap parameters, in-place list building proven by the
uniqueness analysis, and an `own unique` functional-in-place state kernel whose
tail recursion is allocation-free and constant-stack. See
rfcs/performance-modes.md and RFC-0089 for the full contracts.

## Run

```sh
witchy run                                      # from this directory
witchy examples/opt_mode/src/opt_mode.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/opt_mode
```
