# conventions

A tour of witchy's parameter conventions made concrete: `var` for in-place
mutation (`bump`), `let` for a read-only borrow (`sum_from`), `own` for taking
ownership (`drain`, called with `move`), and the same threading through an async
accumulator server where each batch arrives over a channel (channels move values,
so the task owns each one).

**Shows:** `let`/`var`/`own` parameters and `move`, recursion, an ADT with an
`impl` method, `chan.serve`/`spawn`/`send`/`join`, `async`/`await`, and the
`Console` capability.

## Run

```sh
witchy run                                          # from this directory
witchy examples/conventions/src/conventions.witchy  # or by file, from the repo root
```
