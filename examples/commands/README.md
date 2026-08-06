# commands

A tiny `Inc`/`Dec` command type applied to a running total. A `var` parameter
lets `apply` mutate the caller's variable in place, and a `match` over the ADT
selects the operation. Only `main` touches the `Console`, and the program runs
identically interpreted and compiled to WASM.

**Shows:** an ADT (`type` with variants), `match`, a `var` (mutable) parameter,
and the `Console` capability.

## Run

```sh
witchy run                                    # from this directory
witchy examples/commands/src/commands.witchy  # or by file, from the repo root
```
