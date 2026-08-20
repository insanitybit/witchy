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

## Cooperative tasks versus worker VMs

The two map APIs have separate runnable examples because they make different
promises:

| Example | API and capability boundary | Cost contract |
|---|---|---|
| [`cooperative_map.witchy`](src/cooperative_map.witchy) | `chan.par_map` takes an async callback, returns a `Task`, and may receive `Console` explicitly because every task stays in the caller's VM. | Cooperative scheduling and task/channel bookkeeping; no worker startup, serialization, or worker host boundary. |
| [`worker_vm_map.witchy`](src/worker_vm_map.witchy) | `vm.par_map` takes a bare top-level pure function over flat values and returns synchronously. Capabilities do not cross implicitly; use a dedicated adapter such as `vm.with_dir` to grant exactly one `Dir`. | Native execution creates isolated worker VMs and copies inputs/results across their memories, so it is for substantial CPU work rather than tiny callbacks. |

`tests/misc/rfc0129_cooperative_worker_boundary.rs` compiles both shapes and
measures the structural boundary: the cooperative module imports zero worker
host interfaces, while eligible scalar `vm.par_map` imports its two-phase
`vm_par_map_run`/`vm_par_map_write` interface. That is a stable boundary-cost
measurement, not a claim that import count predicts elapsed time.

## Run

```sh
witchy run                                          # from this directory
witchy examples/channels/src/channels.witchy        # or by file, from the repo root
witchy parity examples/channels/src/channels.witchy # interpret vs. compile
witchy parity examples/channels/src/cooperative_map.witchy
witchy parity examples/channels/src/worker_vm_map.witchy
```
