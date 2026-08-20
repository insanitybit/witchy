# channels

A producer sends values over a capacity-one channel and a consumer receives them
until the channel closes. The capacity forces real backpressure: the producer
must wait while the consumer drains each value. Spawning and the channel are
independent: the producer is `spawn`ed, and the channel is an ordinary typed
value passed to both sides, not a task's mailbox. The result is deterministic on
both backends.

**Shows:** `async`/`await`, `chan.spawn`, first-class channels
(`Sender(Int)`/`Receiver(Int)`), bounded backpressure, `chan.send`/`chan.consume`,
structured `chan.join`, and the `Console` capability.

## Run

```sh
witchy run                                          # from this directory
witchy examples/channels/src/channels.witchy        # or by file, from the repo root
witchy parity examples/channels/src/channels.witchy # interpret vs. compile
```
