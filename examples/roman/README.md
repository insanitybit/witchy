# roman

Roman numerals in both directions: `to_roman` (the classic greedy, largest-first
algorithm) and `from_roman` (the subtractive rule — a smaller value before a
larger one is subtracted). The conversions are data-only (`pub`, no capabilities); only
`main` touches the `Console`, so the program runs identically interpreted,
compiled, and inside the capability sandbox.

**Shows:** `while` loops, `match`, string building, `pub` functions across
modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/roman/src/roman.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/roman/src/roman_test.witchy
```
