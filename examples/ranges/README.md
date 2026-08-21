# ranges

Classify an integer against a span instead of enumerating every value:
`http_class` buckets an HTTP status code by its class, and `grade` maps a 0–100
score to a letter. Both use `match` arms with `if` guards over the bounds. The
classification is data-only (`pub`, no capabilities); only `main` touches the
`Console`, so it runs identically interpreted, compiled, and inside the
capability sandbox.

**Shows:** `match` with `if` guards, range checks, `pub` functions across
modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/ranges/src/ranges.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/ranges
```
