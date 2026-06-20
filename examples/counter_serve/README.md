# counter_serve

A counter server written with `chan.serve` — the stateful server loop as a
combinator. It receives a message, runs the handler to get the next state, and
repeats, threading the count through every message. The server holds the
`Receiver`; clients hold a `Sender`. A `Get` carries a reply `Sender`, so the
answer comes back on a channel the client made.

**Shows:** `chan.serve`/`spawn`/`send`/`recv`/`join`, request/reply over channels,
an ADT with `match`, `async`/`await`, and the `Console` capability.

## Run

```sh
witchy run                                              # from this directory
witchy examples/counter_serve/src/counter_serve.witchy  # or by file, from the repo root
```
