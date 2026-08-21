# option_std

Uses the bundled `option` module: `first` returns the head of a list as an
`Option(Int)` (or `None` when empty), and `option.unwrap_or` supplies a default.
Data-only (only `main` touches the `Console`), so it runs identically interpreted,
compiled, and inside the capability sandbox.

**Shows:** the `option` module, the `Option` type with `Some`/`None`, `match`
over list patterns, `pub` functions across modules.

## Run

```sh
witchy run                                          # from this directory
witchy examples/option_std/src/option_std.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/option_std/src/option_std_test.witchy
```
