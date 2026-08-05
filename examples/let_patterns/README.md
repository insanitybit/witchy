# let_patterns

`if let` and `while let`: ergonomic pattern matching for the common case of
"do something only when a value matches one shape." `if let PAT = e:` runs its
body (with the pattern's bindings in scope) only on a match, with an optional
`else`; `while let PAT = e:` loops while the scrutinee keeps matching, perfect
for draining a source until it runs dry. Both desugar to `match`.

**Shows:** `if let` / `while let`, `Option`, list patterns (`[h, ..t]`), tuple
destructuring, and in-rune `test_*` functions. Only `main` touches the
`Console`.

## Run

```sh
witchy run                                              # from this directory
witchy examples/let_patterns/src/let_patterns.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/let_patterns
```
