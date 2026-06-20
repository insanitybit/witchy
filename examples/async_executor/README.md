# async_executor

Cooperative concurrency on the `std/future` substrate: two `ticker` tasks
interleave under a deterministic round-robin executor. `future.join_all` drives
the list of tasks one poll-step per round; each `ticker` yields with
`future.pending` so the other can run. The schedule is fixed and single-threaded,
so the interpreter and compiled WASM produce byte-identical output.

**Shows:** hand-written futures (`future.ready`/`defer`/`and_then`/`pending`/`join_all`),
cooperative yielding, and the `Console` capability.

## Run

```sh
witchy run                                                # from this directory
witchy examples/async_executor/src/async_executor.witchy  # or by file, from the repo root
```
