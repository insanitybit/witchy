# async_tasks

Ergonomic async/await: straight-line `async fn` code with `await` that the
compiler lowers to cooperative tasks. `chan.spawn` starts a concurrent task with
no channel required, and `chan.yield_now` hands control back to the scheduler so
the two `ticker` tasks interleave. The round-robin schedule is deterministic, so
the output is byte-identical on the interpreter and compiled WASM.

**Shows:** `async fn`/`await`, `chan.spawn`/`yield_now`/`join`, and the `Console`
capability.

## Run

```sh
witchy run                                            # from this directory
witchy examples/async_tasks/src/async_tasks.witchy    # or by file, from the repo root
```
