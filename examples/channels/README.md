# channels

A producer sends values over a channel and a consumer receives them until the
channel closes. Spawning and the channel are independent — the producer is
`spawn`ed, and the channel is an ordinary value passed to both sides, not a task's
mailbox. Deterministic and byte-identical on both backends.

**Shows:** `async`/`await`, `chan.spawn`, first-class channels
(`Sender`/`Receiver`), `chan.send`/`chan.consume`, and the `Console` capability.

## Run

```sh
witchy run                                          # from this directory
witchy examples/channels/src/channels.witchy        # or by file, from the repo root
witchy parity examples/channels/src/channels.witchy # interpret vs. compile
```
