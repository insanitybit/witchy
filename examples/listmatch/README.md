# listmatch

List patterns destructure a list by shape: `[]` is empty, `[only]` is exactly
one element, and `[head, ..tail]` splits off the front (binding the rest as a
list). This makes recursive list processing natural — `sum` recurses over the
tail with no indexing.

**Shows:** list patterns (`[]`, `[only]`, `[head, ..tail]`), `match`, recursion,
and in-rune `test_*` functions. Only `main` touches the `Console`.

## Run

```sh
witchy run                                        # from this directory
witchy examples/listmatch/src/listmatch.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/listmatch
```
