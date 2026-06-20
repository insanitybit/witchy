---
status: implemented
note: Imported from docs/ under RFC-0001. Frozen design record — current behavior lives in spec/ and the code.
---

# Concurrency design: async/await + spawn + channels

witchy's concurrency is **stackless async/await** with **`spawn`** for concurrency
and **first-class channels** for communication — the Go/CSP family, shaped by
witchy's value semantics. Spawning and channels are independent: you can spawn a
task with no channel, and a channel is an ordinary value you create and pass
around, not a task's mailbox. The whole executor is ordinary witchy code, so a
concurrent run is byte-identical on the interpreter and the compiled WebAssembly.

## Surface

- `async fn` / `await` — an async function suspends at `await`; calling it yields
  a lazy task that does nothing until driven. `async fn main` is the entry point.
- `chan.spawn(task) -> Handle` / `chan.join(h)` — start a task concurrently; join
  waits for it. No channel required.
- `chan.channel(cap) -> (Sender(m), Receiver(m))`, `chan.unbounded()` — create a
  channel. `Sender`/`Receiver` are ordinary values (copyable handles), so sharing
  a `Receiver` across N tasks is mpmc (a worker pool); sharing a `Sender` is mpsc.
- `await chan.send(tx, x)` — send; a *bounded* channel blocks the sender when full
  (backpressure), an unbounded one never does. Send is always awaited (see below).
- `await chan.recv(rx) -> Option(m)` — the next message, or `None` once the channel
  is closed. `chan.consume(rx, f)` / `chan.serve(rx, state, handler)` write the
  receive loop (stateless / state-threading); `for await x in rx:` is the sugar.
- `chan.select(a, b) -> First/Second/Closed` — take from whichever receiver is
  ready first (a tie favours the first).

## Why this shape (lessons from async Rust)

withoutboats' *Why async Rust?* retrospective makes the tradeoffs explicit, and
several of Rust's costs were *forced by constraints witchy does not share*:

| Rust cost | Why Rust paid it | witchy |
|---|---|---|
| **`Pin`** | a future can hold a reference to its own state across `await`, and Rust structs are always movable | **Gone by construction.** witchy has value semantics: a continuation captures *owned values*, never internal references, so it is never self-referential. No `Pin`, no `Unpin`. |
| **Poll/zero-cost futures** | CPS futures needed refcounting + allocation, unacceptable for zero-cost | witchy isn't allocation-phobic, so it picks the *simplest* model — **CPS via closures**, which witchy's closures already support. |
| **A native runtime (tokio)** | Rust ships no executor; one is bolted on per app | witchy's executor is **pure witchy** (see below), so it is part of the language's deterministic, parity-checked core. |

## The executor

The executor is an **effect protocol** — its core (`Task`/`Step` and the `run`
scheduler) lives in `std/task`, and `std/chan` builds first-class channels on the
same protocol. A `Task(m, a)` is a thunk that, when polled,
yields a `Step`: `Done`, `Yield`, `Fork` (spawn), `Open` (make a channel), `Push`
(send), `Pull` (recv), `PullAny` (select), or `Wait` (join). The executor (`run`)
owns the world — a growable list of task slots and a list of channel buffers — and
threads it functionally through a deterministic round-robin schedule. `await`
lowers to `task.and_then`, which sequences tasks by threading the continuation
through every `Step`. No scheduler state lives in the host runtime, and nothing is
shared mutably, so both backends produce identical interleavings.

A parked task is a `Slot` holding its continuation: `WaitRecv`, `WaitSend`,
`WaitAny`, or `WaitJoin`. Each is retried when the scheduler reaches it. A channel
is `(buffer, capacity)`; a bounded `Push` parks the sender when the buffer is full.

**Closing without destructors.** witchy has no RAII, so a channel can't close "when
the last `Sender` is dropped." Instead, close is **quiescence-based**: when a whole
scheduling round makes no progress (every live task is parked), each parked
receiver is resumed with `None` (its channel is closed), blocked senders and
joiners are released, and the run either makes progress again or stops. This needs
no sender refcounting and is fully deterministic — it is also what ends a
`for await` loop or a `consume`/`serve` server.

## Constraints (and why)

The executor stays pure witchy to preserve the parity contract, which forces two
limits — both acceptable, and documented at the API:

- **One message type per program.** The executor is monomorphic over a single
  message type `m` (its channel buffers are `List(List(m))`). Heterogeneous
  per-channel types would need type erasure, which witchy doesn't have; union into
  a sum type if you need several shapes.
- **Spawned tasks return `Nil`; results flow over channels** (the Go model). A
  typed `JoinHandle(T)` would require the executor to store differently-typed task
  results — again erasure — so a task reports a result by sending it.

Lifting either would mean moving the scheduler and channel buffers into a native
(Rust) runtime exposed through host imports, trading the pure-witchy, byte-identical
executor for tokio-style typed handles. That trade isn't worth the parity loss.

**Send is always awaited.** Because the channel buffer is executor-owned, sending
is necessarily an effect, so `send` is always `await chan.send(tx, x)` even on an
unbounded channel (where it resolves immediately). The bounded/unbounded choice
controls *backpressure*, not whether `send` is awaited.

## Lowering

`src/async_lower.rs` performs the CPS transform before type-checking, so the rest
of the compiler never sees `async`/`await`. `await E` becomes `task.and_then(E,
fn(x): <rest>)`; the whole body is wrapped in `task.lazy`; an async `main` becomes
`task.run(<body>)` (a single root that may spawn more). `await` may appear as a
`let`/`let (a, b)` value, a bare statement, in tail position (including `if`/`match`
branches), or inside a `for` loop body — `for x in xs:` lowers to `task.for_each`,
and `for await x in rx:` to `chan.consume`. `await` inside a `while` loop or a
condition/scrutinee is not yet supported (a `while` would need mutable state carried
across the await, which captured-by-value closures can't express).
