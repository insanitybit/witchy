# ownership

`own` transfers ownership (spelled `sink` in Hylo): `into_label` consumes its
argument, so after `into_label(name)` the caller may not use `name` again — the
type checker rejects use-after-move.

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
