# worker_pool

Three workers share ONE job queue, and whoever is free takes the next job. This
is mpmc — many receivers on one channel — which a per-task mailbox model can't
express. A `Receiver` is an ordinary value, so handing the same one to three
spawned tasks just works; results flow back on a second channel to a printer.
Deterministic and byte-identical on both backends.

**Shows:** `async`/`await`, the `chan` module (`channel`, `spawn`, `send`,
`consume`, `join`), and first-class `Sender`/`Receiver` values.

## Run

```sh
witchy run                                            # from this directory
witchy examples/worker_pool/src/worker_pool.witchy    # or by file, from the repo root
```
