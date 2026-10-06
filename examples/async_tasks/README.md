# async_tasks

Ergonomic async/await: straight-line `async fn` code with `await` that the
compiler lowers to cooperative tasks. `chan.scope` runs both `ticker` tasks
concurrently and returns when both finish, with no channel required, and
`chan.yield_now` hands control back to the scheduler so the two tasks interleave. The round-robin schedule is deterministic, so
the output is byte-identical on the canonical interpreter and admitted Wasm.

**Shows:** `async fn`/`await`, `chan.scope`/`yield_now`, and the `Console`
capability.

## Run

```sh
witchy run                                            # from this directory
witchy examples/async_tasks/src/async_tasks.witchy    # or by file, from the repo root
```
