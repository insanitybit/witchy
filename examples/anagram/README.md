# anagram

Groups words that are rearrangements of the same letters. Each word gets a
*signature* — its characters sorted and rejoined — so anagrams share one, and
words are bucketed by signature using parallel lists (no `Dict`, so it compiles
on both backends). The grouping is pure (`pub`, `Console` only); only `main`
prints, so it runs identically interpreted and compiled.

**Shows:** `for` loops, sorting with `list.sort_by` and a closure, string
comparison/equality, `pub` functions across modules, and in-rune `test_*`
functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/anagram/src/anagram.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/anagram
```
