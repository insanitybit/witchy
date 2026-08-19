---
rfc: 0129
title: "Deterministic async tasks, typed channels, and explicit parallel workers"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical concurrency RFC. Async/await, typed channels, deterministic scheduling, structured combinators, and backend parity are shipped for bounded workloads. Promotion requires removing or enforcing the compiled long-stream memory ceiling and publishing separate cooperative-versus-parallel worker contracts."
predecessors:
  - "[0032](0032-multi-core-execution.md) (deterministic tasks versus explicit parallel workers)"
  - "[0036](0036-bounding-the-async-executor.md), [0059](0059-state-machine-async.md) (deferred bounded executor and state-machine destination)"
  - "[0055](0055-channel-message-types.md) (typed heterogeneous channels)"
  - "[concurrency design](concurrency-design.md) (original task/channel surface)"
related:
  - "[0128](0128-regions-and-reclamation.md) (frame and per-step reclamation)"
  - "[0130](0130-generators-and-iterators.md) (resumable owned state)"
---

# RFC-0129: Deterministic async tasks, typed channels, and explicit parallel workers

## Decision

Witchy retains two deliberately different concurrency tools:

1. cooperative `async` tasks and typed channels for deterministic concurrency
   inside one Witchy VM; and
2. explicit worker-VM operations for true CPU parallelism across isolated
   memories.

The two tools do not share a name or performance claim. Cooperative workers are
scheduled tasks. Parallel workers cross a serialization and capability boundary.

## Cooperative task model

- `async fn` creates a lazy `Task(T)`.
- `.await` sequences a task and resumes with its value.
- `chan.spawn` starts a task; `chan.join` waits for completion.
- `chan.channel(capacity)` creates `Sender(T)` and `Receiver(T)` values whose
  message type is inferred from their checked uses.
- bounded sends apply backpressure; receive returns `Option(T)` at closure.
- `for await` consumes a receiver.
- `scope`, `gather`, `par_map`, `par_reduce`, and `race` provide structured
  lifetimes and deterministic result order.

The scheduler has one specified deterministic interleaving. Backend parity
therefore includes output order, channel selection, cancellation, closure, and
join behavior rather than merely final values.

## Channel closure

Witchy values have no RAII destructor event that can define "last sender
dropped." Channel closure is a scheduler event. The current quiescence rule is
retained until a replacement RFC proves a better deterministic contract:
when no live task can make progress, parked receives close, blocked operations
are released according to the documented scheduler transition, and execution
continues or terminates.

This behavior must remain explicit in the API and tests. A retained sender may
be usable after an earlier quiescence transition; source must not infer Rust- or
Go-style last-sender semantics.

## Bounded lifetime contract

The current compiled continuation representation accumulates memory in long
message loops. That limitation is incompatible with promoting unbounded service
queues.

Before the full channel surface becomes supported preview, one of these must be
true:

- per-object reclamation and frame ownership keep memory flat for the sustained
  channel soak; or
- the compiler and runtime enforce a documented bound that rejects or safely
  stops workloads beyond the supported class.

Documentation alone is insufficient for an operation named `unbounded` when
the compiled runtime predictably exhausts memory after a modest number of
resumptions.

## Async state machines

The long-term lowering is an owned resumable frame. Each local live across an
`await` occupies a checked frame slot; each transition drops superseded state;
references cannot cross suspension without a proven owner relation. This model
supports loops, branch conditions, and match scrutinees without rebuilding a
closure chain per message.

`async` trait behavior remains expressible through an ordinary trait method
returning `Task(T)`. Dedicated async-trait syntax is unnecessary unless it adds
a contract that `fn ... -> Task(T)` cannot express.

## Parallel worker model

True parallel work runs in isolated worker VMs. Inputs and outputs are owned and
serializable. Capabilities do not cross implicitly. Scheduling may be
nondeterministic internally, while result ordering and failure aggregation are
specified by the calling combinator.

The standard library and book must not describe cooperative `chan.par_map` as
CPU parallelism or describe worker-VM execution as zero-cost task spawning.

## Acceptance

1. Async lowering preserves source locations, types, mutation, early returns,
   loops, and traps.
2. Typed channels cover multiple message types, bounded backpressure, selection,
   closure, cancellation, and structured joins.
3. Interpreter and compiled Wasm agree on the complete deterministic schedule.
4. A sustained compiled producer/consumer soak has bounded memory and no
   continuation leak before unbounded service use is promoted.
5. Invalid reference escape, capability transfer, async trait syntax, and task
   result use fail with source-level diagnostics.
6. Cooperative and true-parallel APIs have separate examples, capability
   boundaries, cost models, and measurements.
7. A clean installed binary runs the promoted bounded workflow without
   repository-only setup.
