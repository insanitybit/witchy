# wrap

Greedy word wrapping: `wrap` packs as many space-separated words as fit onto each
line within a column width, breaking before a word that would overflow. The wrap
logic is pure (`pub`, no capabilities); only `main` touches the `Console`, so the
program runs identically interpreted, compiled, and inside the capability sandbox.

**Shows:** `for` loops, conditionals, string building, `pub` functions across
modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                              # from this directory
witchy examples/wrap/src/wrap.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/wrap/src/wrap_test.witchy
```
