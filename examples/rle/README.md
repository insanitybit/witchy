# rle

Run-length encoding and its inverse: a maximal run of one character is written
as its length followed by the character (`"aaab"` -> `"3a1b"`), and decoding
reverses that. The pair forms a true round-trip — `decode(encode(s)) == s` for
every sample. Data-only string processing over the `Console` only, so it runs
identically on both backends.

**Shows:** `while` loops, character scanning, string building, `pub` functions
across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                            # from this directory
witchy examples/rle/src/rle.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/rle/src/rle_test.witchy
```
