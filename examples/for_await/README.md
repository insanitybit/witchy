# for_await

`for await x in rx:` — a receive loop over a channel that runs its body for each
message until the channel closes (and the body may itself `await`). A `producer`
sends a few numbers over a `Sender`, a spawned `worker` consumes them with
`for await`, and both forms of `for` appear: the producer iterates a *list*, the
worker iterates a *receiver*.

**Shows:** `async`/`await`, `chan.channel`/`chan.send`/`chan.spawn`, `for await`
receive loops, and the `Console` capability.

## Run

```sh
witchy run                                      # from this directory
witchy examples/for_await/src/for_await.witchy  # or by file, from the repo root
```
