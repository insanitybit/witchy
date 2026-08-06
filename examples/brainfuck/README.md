# brainfuck

A complete interpreter for the eight-instruction language. The tape is a list of
integer cells with a moving data pointer; `[`/`]` are matched by scanning with a
depth counter (no precomputed jump table). Output builds a string, turning each
cell value into a character by indexing a literal of printable ASCII. Pure
(`pub`, `Console` only); identical on both backends.

**Shows:** `while` loops, `if`/`else if` chains, list building, string indexing
for character conversion, `pub` functions across modules, and in-rune `test_*`
functions.

## Run

```sh
witchy run                                      # from this directory
witchy examples/brainfuck/src/brainfuck.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/brainfuck
```
