# dedup

Removes consecutive duplicates from a sequence — a user-defined iterator transform
built from `split_first` (peek the head) and `drop_while` (skip the equal run after
it), driven by `iter.unfold`. The transform is data-only (`pub`, no capabilities); only
`main` touches the `Console`, so it runs identically interpreted, compiled, and
inside the capability sandbox.

**Shows:** lazy `Iter` values, closures, `match` on `Option` and tuples, and
in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/dedup/src/dedup.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/dedup
```
