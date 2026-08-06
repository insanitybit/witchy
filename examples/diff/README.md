# diff

A line diff by longest common subsequence: fill the classic LCS length table over
the two line lists, then backtrack from the bottom-right corner, emitting unchanged
(`  `), deleted (`- `), and inserted (`+ `) lines. The `diff` function is pure
(`pub`, no capabilities); only `main` touches the `Console`, so it runs identically
interpreted, compiled, and inside the capability sandbox.

**Shows:** `while` loops, nested lists, `match`-free index logic, `pub` functions
across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                              # from this directory
witchy examples/diff/src/diff.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/diff
```
