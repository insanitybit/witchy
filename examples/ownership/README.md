# ownership

`own` marks a consuming parameter. After you pass a heap value (`List`,
`Dict`, a record) to an `own` parameter, a later use is a check-time error.

**Shows:** the `own` parameter convention, move semantics enforced at
compile time, `pub` functions across modules.

## Run

```sh
witchy run                                      # from this directory
witchy examples/ownership/src/ownership.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/ownership/src/ownership_test.witchy
```
