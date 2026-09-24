# commands

A tiny `Inc`/`Dec` command type applied to a running total. A `var` parameter
lets `apply` mutate the caller's variable in place, and a `match` over the ADT
selects the operation. Only `main` touches the `Console`. The ordinary command
runs admitted Wasm; `witchy parity` can compare it with canonical SafeWIR.

**Shows:** an ADT (`type` with variants), `match`, a `var` (mutable) parameter,
and the `Console` capability.

## Run

```sh
witchy run                                    # from this directory
witchy examples/commands/src/commands.witchy  # or by file, from the repo root
```
