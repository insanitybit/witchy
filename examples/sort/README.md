# sort

Sorts a list two ways with `list.sort_by` — a generic sort that takes an
"is-less-than" closure, so the same call yields ascending or descending order
depending on the comparator passed in. The rendering is data-only (`pub`, no
capabilities); only `main` touches the `Console`.

**Shows:** generic `list.sort_by`, comparator closures, `list.map` plus
`list.join`, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                               # from this directory
witchy examples/sort/src/sort.witchy     # or by file, from the repo root
```

## Test

```sh
witchy test examples/sort
```
