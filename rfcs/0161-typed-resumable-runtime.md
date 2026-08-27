---
rfc: 0161
title: "Typed Resumable Runtime: Structured Tasks, Owned Channels, and Deterministic Select"
status: proposed
created: 2026-08-26
supersedes:
  - "0152 (deterministic CSP ready paths), upon acceptance"
predecessors:
  - "[0129](0129-concurrency-tasks-and-channels.md) (current public concurrency contract)"
  - "[0059](0059-state-machine-async.md) (defunctionalized async segments and the typed-frame destination)"
  - "[0055](0055-channel-message-types.md) (typed endpoints and the erased materialization boundary)"
related:
  - "[0032](0032-multi-core-execution.md) (deterministic single-VM tasks and isolated parallel workers)"
  - "[0036](0036-bounding-the-async-executor.md) (capture ownership and recursive-drop constraints)"
  - "[0114](0114-must-consume-obligations.md) (static lifecycle obligations)"
  - "[0127](0127-ownership-and-opt-mode.md) (value, move, and suspension ownership)"
  - "[0130](0130-generators-and-iterators.md) (owned resumable frames and no-replay generator work)"
  - "[0135](0135-stable-semantics-converge.md) (interpreter and Wasm convergence)"
  - "[0154](0154-call-graph-specialization.md) (general closure and call specialization)"
tracking: >
  Proposed replacement for the RFC-0152 implementation program. The live
  acceptance state is recorded in 0161-acceptance-ledger.md. Existing RFC-0152
  compiler facts, tests, counters, and measurement tools are inputs, not proof
  that this RFC is implemented.
---

# RFC-0161: Typed Resumable Runtime

## Summary

Witchy will have one implementation model for cooperative concurrency. Calling
an `async fn` creates a cold, affine `Task(T)`. Starting that task creates one
typed runtime execution whose compiler-generated state machine owns its live
locals. Channels store their exact message type in an exact-layout queue.
Blocking operations use one reusable wait record in the execution. The
deterministic scheduler moves execution IDs and readiness, not messages,
closures, lists, or nominal selected-result wrappers.

This is the normal async runtime, not an optional optimization selected by
recognizing a benchmark-shaped task graph. Ready operations and blocking
operations use the same channel, frame, and scheduler representation. Dynamic
or opaque boundaries may materialize an erased value, but they do not switch to
a second CPS scheduler.

The public model is structured:

- `Task(T)` is lazy and must be started exactly once;
- `scope` owns all concurrent children, and `Child(T)` cannot outlive it;
- cancellation is an explicit cooperative request, never abrupt destruction;
- a `must Channel(T)` owner closes a channel explicitly;
- `rendezvous`, `bounded`, and `unbounded` are distinct constructors; and
- `select` is an expression over sealed selectable operations with a single
  probe, register, commit, and cancel protocol.

This RFC preserves single-VM determinism and the separate worker-VM model for
CPU parallelism. It intentionally replaces replayable task values,
quiescence-as-channel-close, untyped child handles, and immediate shallow task
killing from RFC-0129.

## Why a new RFC is necessary

The current system has reached the limit of incremental specialization.
`std/task.witchy` is a closure-driven CPS executor with an erased `__Msg`
carrier and a growing family of operation and frame variants. The compiler has
landed useful endpoint, payload, ownership, region, and WIR facts, but a normal
two-way fan-in still crosses task closures, list frames, scheduler slots, and
selected-result adapters.

RFC-0152 correctly requires typed queues, direct result lanes, bounded wait
state, semantic parity, and evidence across real programs. Its final ledger is
still entirely pending. The branches created while exploring it have found the
missing seam: the compiler needs a typed async-program representation after
closure conversion. Adding more `Pull2I64`, frame-loop, or scalar-graph cases
before that seam exists produces more representations without producing one
general runtime.

The design problem is therefore larger than `select_fanin`, but also cleaner:
compile ordinary async code to typed resumable executions, then make channels
and selection scheduling operations over those executions.

## Repository state reviewed for this decision

The concurrency audit began against master `9663d238b` on 2026-08-26 and was
refreshed after RFC-0160 landed at master `90725c953`. The live merge-queue and
worktree state was inspected at both points.

- Before adding this proposal, the repository contained 155 numbered RFC
  documents through RFC-0160: 106 implemented, 14 accepted, 11 deferred, 14
  proposed, one planned, and nine in terminal or ledger states.
- RFC-0160 had already claimed its number and then landed while the audit was
  in progress, so this proposal uses 0161.
- RFC-0129 was accepted and all seven of its existing acceptance rows were
  proven. Those rows describe current behavior and remain valid historical
  evidence. This RFC names every behavior it changes.
- RFC-0152 was proposed. Its acceptance rows 1 through 10 were all pending.
- No RFC-0152 branch was in the merge queue. The active queue was unrelated.
- Master had landed typed endpoint and payload facts, ready-region formation,
  `ReadyStateWir`, typed Int ring bridges, fixed select frame bridges, counters,
  and semantic fixtures. It did not have a general direct async executor or a
  direct select emitter.
- `integration/rfc0152-typed-select-frame` and several direct-executor branches
  contained overlapping prototypes. Two older worktrees had uncommitted diffs.
  Those diffs must be preserved. No prototype branch is the architectural
  authority for this RFC.
- The RFC status audit also found an old dirty
  `perf/rfc0129-spawn-resumption` worktree containing one untracked opt-in
  spawn/join scaling test. It is useful test input but is based far behind
  master and has no implementation changes. It must be preserved and rebased
  before use.
- The `docs/rfc0130-no-replay-contract` pickup and
  `impl/rfc0130-no-replay-fallback` prototype concern generator frames, not
  public task replay. Their one-time owned-resumption rule supports the same
  internal frame direction, but they do not decide `Task(T)` semantics.

## Predecessor disposition

| RFC | Disposition if this RFC is accepted |
|---|---|
| 0032 | Retain deterministic cooperative execution in one VM and isolated worker VMs for parallelism. |
| 0036 | Retain its warning that capture move/drop cannot be guessed. This RFC does not claim general implicit destruction. |
| 0055 | Retain typed endpoints and a single explicit erased materialization boundary. The scheduler itself no longer needs `__Msg`. |
| 0059 | Retain named async segments and typed live-across-await state. Replace the special scalar-SoA destination with the uniform execution ABI below. |
| 0114 | Retain static `must` obligations and their transfer through suspension. No finalizers or GC callbacks are introduced. |
| 0127 | Retain value and move semantics. Task, child, channel, and send-payload ownership are expressed through them. |
| 0129 | Retain `async`/`await`, typed channels, deterministic single-VM concurrency, and separate worker VMs. Replace replayable `Task`, untyped `Handle`, quiescent close, and shallow immediate cancellation. |
| 0130 | Coordinate on the shared owned-resumable-frame vocabulary and no-replay invariant. Keep generator pull state distinct from task execution state. |
| 0135 | Retain the interpreter-debt and convergence rules. Wasm-first physical work cannot become supported behavior without the semantic matrix. |
| 0139 Track 2 | Replace the special two-way selector approach. Retain its allocation diagnosis as history; RFC-0139 as a whole remains a separate proposed multi-track RFC. |
| 0146 Track 1 | Replace its shape-specific approach and 2.00x-Go closure target. Retain its matched evidence protocol; RFC-0146 as a whole remains proposed. |
| 0152 | Supersede it as a whole, importing every acceptance row and its evidence discipline. |
| 0154 | Coordinate with it. This RFC consumes final closure layouts but does not own general call-graph specialization. |

RFC-0152 is not marked `superseded` while this RFC remains proposed. RFC-0139
and RFC-0146 are not whole-RFC lifecycle predecessors and are not marked
superseded by this decision.

## Design principles

1. There is one semantic runtime path for supported async code.
2. Types and ownership known after checking survive into frames, queues, waits,
   and result lanes.
3. Starting work is explicit. Every concurrent child has an owner.
4. Readiness is cheap because it is ordinary state inspection, not because a
   named source shape bypasses the runtime.
5. Blocking allocates bounded state before the wait and reuses it after wakeup.
6. Determinism is specified in task turns, arm priority, closure, wakeup, and a
   canonical admitted-external-event trace. It does not claim to control the
   arrival time of the external world.
7. Dynamic behavior has an explicit materialization boundary and a named cost.
8. No task is destroyed while it may own a `must` value.
9. Actors are a library pattern over tasks and channels.
10. Benchmarks validate the architecture; they do not choose it.

## Public task model

### Cold, affine `Task(T)`

Calling an async function performs no body effects and returns:

```text
must Task(T)
```

A task is consumed by exactly one of:

- `.await`, which runs it as sequential async composition in the current
  execution;
- `spawn`, which starts it as a child execution;
- `run`, which starts a root execution; or
- moving it into another value that preserves the same obligation.

Starting the same task value twice is a compile error. Reusable work is an
ordinary function that creates a fresh task:

```text
fn refresh() -> Task(Data):
    fetch_data()

let first = refresh().await
let second = refresh().await
```

This is an intentional change from the current replayable task recipe. A task
can capture owned values and capabilities, and replaying such a value is not a
sound default. Affinity also lets the compiler move captures into one execution
frame without a hidden copy or memoization rule.

`Task(T)` propagates `must` through records, tuples, lists, closures, and generic
containers. It cannot be implicitly copied or extracted through an API that
loses element identity. Standard task combinators take task inputs by `own` and
return exactly one task obligation. Task collections use consuming iteration
and return the unconsumed suffix on an error edge. `?`, loops, partial aggregate
consumption, and closure capture must discharge or transfer every task on every
path. These library signatures and negative cases are frozen before runtime
implementation begins.

### Internal `Execution`, not a second public task type

`Execution(T)` is compiler/runtime state and cannot appear in source. It has:

```text
ExecutionShape {
    frame_layout,
    entry_state,
    resume_table,
    result_layout,
    ownership_map,
    source_map,
}

Execution {
    shape_id,
    state_id,
    typed_frame,
    status,
    parent_scope,
    wait_record,
    cancellation_state,
}
```

One concurrent task has one scheduler execution. Sequential async calls remain
inside that execution. Known calls can use direct state transitions; dynamic
calls use typed call frames. Neither case creates a scheduler task merely
because one async function awaits another.

The frame contains exactly the locals live across a suspension plus explicit
runtime fields. A transition moves or overwrites fields according to checked
ownership facts. It does not reconstruct a closure tower after every message.

Every state has an initialized-lane bitmap and an ownership transition table.
Normal completion and propagated error edges must move, return, or explicitly
discharge every initialized `must` lane. Cancellation never tears down a frame;
it only makes a cancellation operation ready. A source trap aborts the VM and
does not promise language-level cleanup, matching the existing trap contract.
Failed registration restores the pre-registration lane state before source can
resume.

## Structured concurrency

Every root execution has an implicit scope. User-created `scope` blocks create
child scopes:

```text
let total = scope:
    let left = spawn(count(left_rx))
    let right = spawn(count(right_rx))
    left.await + right.await
```

`spawn(own Task(T)) -> Child(T)` starts the child on the next scheduler round.
Every child execution has a cancellation state. The child may obtain a borrowed
`CancelToken` with `task.cancel_token()` and explicitly pass it to operations it
wants to make cancellable. `Child(T)` is typed, scope-bound, and must-consume.
Awaiting it consumes the handle and produces `T`. A non-`Nil` result cannot be
silently discarded.

The source type is printed as `Child(T)`, but checking gives it an unforgeable
lexical brand `Child(scope_id, T)`. The brand is tracked through aggregates,
closures, generic instantiations, moves, and suspension frames. It may cross an
`await` owned by the same or a nested scope, but it may not be returned, stored
in `Dynamic`, captured by an escaping closure, or moved into an outer scope. A
child result containing a `must` value transfers that obligation to the
awaiting branch.

A child cannot escape the lexical scope that created it. On normal scope exit,
all `Nil` children not explicitly awaited are joined in deterministic spawn
order. Non-`Nil` children must be awaited or collected explicitly. On `return`
or a `?` propagation edge, the scope requests cancellation of every unfinished
child and then waits for every child to finish. This is compiler-generated
control flow, not a destructor or GC finalizer.

Cancellation remains cooperative. A child blocked on an operation that did not
observe its token can therefore delay scope exit indefinitely. This is defined
behavior, not permission to destroy the child. The compiler warns when an
early-exit-capable scope has a live child whose reachable blocking operations
have no cancellation arm, but the warning is not a proof of termination.

Detached tasks are not part of this RFC. A future detached-task RFC would need
an explicit process-level owner, capability and shutdown policy, error sink,
and resource budget.

## Cooperative cancellation

Cancellation is a request represented by a typed `CancelToken`. It is not an
exception injected at an arbitrary instruction and it never discards an
execution frame.

```text
scope:
    let child = spawn(serve(rx))
    // ...
    child.request_cancel()
    child.await
```

The child obtains its borrowed token with `task.cancel_token()`. The token is
selectable. Cancellable channel waits are expressed as a select between the
channel operation and `cancelled(token)`, so a losing send restores its payload
through the ordinary `PendingMove` rule. Timers and host operations use their
explicit operation cancellation contracts. A plain operation is not secretly
cancelled. A task observes cancellation, performs its explicit cleanup, and
returns a normal domain value such as `Result(T, Cancelled)`.

Requesting cancellation does not consume `Child(T)`; the caller must still
await it. A child that never observes its token may delay its scope forever.
That is preferable to abandoning its `must` values. Diagnostics identify the
scope, child creation site, and current wait site.

## Owned typed channels

### Construction and lifecycle

The constructors are distinct:

```text
channel.rendezvous() -> Task((must Channel(T), Sender(T), Receiver(T)))
channel.bounded(capacity: Int) -> Task(Result((must Channel(T), Sender(T), Receiver(T)), CapacityError))
channel.unbounded() -> Task((must Channel(T), Sender(T), Receiver(T)))
```

`bounded` requires a positive capacity. Zero never ambiguously means
unbounded or rendezvous.

`Channel(T)` is the lifecycle owner. Sender and receiver values are typed
views and may be distributed according to normal value and ownership rules.
As a `must` value, the owner is affine: implicit copy, double close, and loss in
an aggregate, collection, closure, or error edge are rejected. It may be moved
or returned while preserving exactly one obligation. The owner is consumed by:

```text
channel.close(own channel) -> Task(Nil)
```

Close rejects future sends, wakes blocked senders with their unsent values,
and lets receivers drain already-buffered values before returning `None`.
Sending is therefore ownership-preserving:

```text
send(Sender(T), own T) -> Task(Result(Nil, SendError(T)))
recv(Receiver(T)) -> Task(Option(T))
```

There is no last-sender rule because Witchy has no destructor event, and there
is no synthetic close at scheduler quiescence. If every runnable execution is
parked while a channel owner remains open, the root-only driver
`task.run(own Task(T)) -> Result(T, Deadlock)` returns a deterministic
`Deadlock` with the parked source locations. Calling `run` from an async
execution is rejected. An async `main` is driven by the launcher; a deadlock
prints the same stable diagnostic and exits nonzero. Deadlock reporting may
allocate diagnostic data; the successful runtime path may not.

### Queue representation

Each channel has one logical queue with a fixed payload layout for `T`:

```text
TypedQueue(T) {
    storage: RingStorage(T),
    head,
    length,
    capacity_mode,
    closed,
    send_waiters,
    recv_waiters,
}
```

Waiter lists are intrusive. Their links live in execution wait records rather
than the channel heap, so an arbitrary number of blocked tasks does not force
per-wait allocation in a bounded channel. Each link contains the execution ID,
operation site, arm index, registration sequence, generation, direction, and
unlink state. Static selects have one inline link per arm. A dynamic select has
a frame-owned link array whose allocation and reuse are explicit.

The queue stores `T`, not `__Msg`. Scalar values use scalar lanes. Strings,
buffers, aggregates, and owned values use their checked layouts and ownership
envelopes. The scheduler never inspects payload structure; generated enqueue,
dequeue, move, and close functions do that work.

An erased envelope is created only when a value crosses a truly dynamic
boundary such as reflection, an opaque heterogeneous operation, or a dynamic
receiver collection whose element layout is not statically recoverable. That
boundary is one-way until an endpoint-specific checked recovery. Scheduler
code may not reinterpret erased aggregate storage.

Unbounded channels use typed chunks and may grow without a fixed limit. Their
name is a logical capacity promise, not a memory bound. Bounded and rendezvous
channels have a fixed queue allocation after creation.

Senders, receivers, and selectable operations waiting on the same channel are
ordered by deterministic registration sequence. A rendezvous pairs the oldest
compatible sender and receiver. Close wakes waiters in that same order. No host
timing or hash iteration may decide which Witchy waiter wins.

## `select` as a language expression

`select` operates on sealed selectable operations rather than arbitrary tasks:

- receive readiness;
- send readiness;
- child completion;
- cancellation token readiness;
- timer readiness; and
- capability-host I/O readiness exposed through the async host ABI.

Illustrative syntax:

```text
let next = select:
    recv(left) as maybe_value -> Left(maybe_value)
    send(out, own pending) as outcome -> Sent(outcome)
    complete(own child) as result -> Finished(result)
    cancelled(cancel) -> Stopped
```

The selected branches form one typed expression. Each arm keeps its own exact
payload layout; a heterogeneous select does not need a common message union.
The compiler materializes a nominal union only if the programmer's result type
requires one.

`recv` binds `Option(T)`, including `None` after a closed queue drains.
`complete(own child)` conditionally consumes `Child(T)` and binds `T` only when
that arm wins. A losing branch retains the child handle. A send arm binds
`Result(Nil, SendError(T))`; close is a ready failure that returns the unsent
value.

Send arms and child-completion arms use path-conditional moves. Operand
evaluation reserves the value in an exact typed frame field. If the arm wins,
commit transfers it to the queue or child result operation. If a different arm
wins, the compiler restores it to its source binding before entering that
branch. The all-path checker then treats the binding as consumed only in the
winning branch. An owned temporary with no restoration binding is accepted
only if its type is ordinarily droppable; a `must` temporary must have a named
disposition on every losing branch.

This is select-specific language semantics, not an ordinary `own` call whose
obligation is discharged at call attempt. Side-effectful endpoint and payload
expressions are evaluated exactly once in source order before probing.

The physical representation is:

```text
PendingMove(T) = Vacant | Reserved(T) | Committed | Returned(T)
```

The tag and exact `T` lanes live in the execution frame. The wait record points
to them and never owns or copies the payload. Every operation descriptor has a
machine-checked table for `reserve`, `commit`, `restore`, `return-error`, and
`retain-for-reprobe`. Close, cancellation, another winning arm, failed
registration, and stale wakeup each name one of those transitions. `must`
obligations remain owned by the execution while `Reserved`.

Arms are considered in source order. The first ready arm wins. A `default` arm
runs only if no operation is ready during the initial probe. Committing default
is a scheduler boundary, so a default loop cannot starve an already-runnable
peer. Arbitrary `Task`
values are not selectable because implicitly starting, racing, and cancelling
them would obscure ownership. Start work with `spawn`, then select its typed
child-completion operation.

### One wait protocol

Every select uses the same protocol:

1. Evaluate arm operands once in source order.
2. Probe each operation in source order.
3. Atomically commit the first ready operation, including its ownership table,
   if any.
4. If none is ready and `default` exists, restore all reservations, commit the
   default boundary, and run it on the execution's next turn when a peer is
   runnable.
5. Otherwise register the execution's one wait record with all operations.
6. On notification, recheck in source order at the execution's deterministic
   turn, commit exactly one operation, and invalidate every other registration.

The scheduler is single-threaded, so probe plus registration is atomic with
respect to Witchy tasks. Host completions are admitted only at turn boundaries.
A generation counter prevents stale notifications after reuse. Cancellation,
close, child completion, and I/O all claim the same wait identity. There is no
separate algorithm for select arity two, three, or four.

The wait record has an explicit state machine:

```text
Idle(generation)
  -> Armed(generation, links)
  -> Notified(generation, cause_hint)
  -> Claimed(generation, arm)
  -> Committing(generation, arm)
  -> Idle(generation + 1) | Completed(generation)

Armed -> Cancelled -> Idle(generation + 1)
```

`cause_hint` only makes the execution runnable; the execution re-probes every
arm in source order before `Claimed`. The first transition from `Armed` to
`Notified` queues the execution. Duplicate notifications for that generation do
not queue it again. A close, cancellation request, or completion arriving after
notification but before the task turn changes the re-probe result, not the wait
identity. `Claimed` unlinks every losing arm before `Committing` moves a payload
or result. `Cancelled` is internal registration rollback, not cooperative task
cancellation; it unlinks every arm and restores every reservation. A
notification with any older generation is ignored and cannot access frame
lanes.

Close is itself a scheduler operation and its state change is the linearization
point. A ready probe and commit performed by one running execution cannot have a
Witchy close interleaved between them. After a parked wait, re-probe observes
either the old open state or the new closed state. Closed send arms return their
reserved payload; closed receive arms become ready with `None` only after the
buffer drains.

### Rendezvous matching

A rendezvous send is ready only when it can pair with a compatible registered
receiver, and conversely for receive. Pairing and select choice are resolved by
a scheduler matching phase:

1. Consider live registrations in `(registration_sequence, arm_index)` order.
2. For each select, expose only its lowest-source-order currently matchable arm.
3. Pair the oldest compatible exposed send and receive.
4. Atomically claim both generations and move the payload into the receiver's
   result lane.
5. Invalidate every losing arm before either execution resumes.

An unmatched payload remains `Reserved`. Close fails unmatched sends with their
values and completes unmatched receives with `None`. Duplicate endpoints are
legal, but only the lowest source arm from one select may be exposed in a
matching phase. The same matching rules apply whether either side is a plain
operation or a select arm.

Matching is a subphase of every scheduler transition. The scheduler first
applies the transition's registrations or state change, then runs matching to a
fixed point on the touched channels, then performs wait claims, and only then
inserts newly ready executions. A close transition marks the channel closed
before matching and therefore fails or drains waiters instead of forming a new
rendezvous. An admitted external batch is processed as ordered individual
transitions, with this subphase after each entry.

Static arm counts become inline descriptors. Dynamic homogeneous select is a
named allocation-bearing boundary in the first implementation. Promotion
requires a frame-owned immutable endpoint snapshot, duplicate-endpoint rules,
and a link array allocated once per execution activation and reused without
copying in the loop. If endpoint identity or length changes, the descriptor is
rebuilt with a measured visible cost. Truly opaque collections materialize at
the explicit dynamic boundary.

The existing `chan.select`, `select3`, and `select4` functions can be migration
wrappers over this expression. They are not separate runtime primitives.

## Deterministic scheduler contract

The scheduler owns dense execution slots, ready queues, scope membership, and
wait headers. It does not own payloads or user continuations.

A task turn runs until it:

- completes;
- blocks;
- explicitly yields; or
- commits a scheduler operation while another execution is runnable.

Pure async composition and awaiting an already-complete sequential async call
do not create a turn. Channel send/receive/select, spawn, child await, timer,
host I/O, committed default, and `yield_now` are turn boundaries.

The executable transition rule is:

1. At round start, snapshot the ready executions in queue order.
2. Run the head execution until a boundary.
3. Give each scheduler operation a monotonically increasing transition
   sequence. Insert every execution it wakes into the current ready queue in
   `(transition_sequence, registration_sequence, arm_index, execution_id)`
   order, unless that execution is already queued.
4. After a ready operation commits, inspect the queue after all of its wakeups.
   If any peer is ready, append the current execution to the tail and switch.
   If no peer is ready, continue the current execution immediately.
5. A blocked execution is not requeued. A completed execution wakes child
   waiters by the same insertion rule and leaves the scheduler.
6. A spawned child is placed in the next-round queue, never the current round.
7. When the current queue empties, promote the next-round queue in spawn order
   and begin the next round.

The post-commit rule is load-bearing: a send that wakes a receiver yields to it,
and a receive that wakes a blocked sender yields to it. Sole-execution
continuation is permitted only when the operation did not leave any peer ready.

Close and cancellation requests are scheduler operations. Their transition
sequence orders them against payload wakeups. Within one transition, claims use
registration and arm order. No hash traversal or physical pointer order is
observable.

Before choosing the next execution at a turn boundary, the embedder may admit a
batch of external completions. Each entry in the canonical admission trace is
`(admission_sequence, operation_id, generation, result_or_error)`. The scheduler
processes the batch in admission-sequence order and applies the same ready-queue
insertion rule. External arrival time is naturally an input; the same canonical
admission trace produces the same Witchy schedule on both backends.

The change from continuation-level yielding to operation-level turns is a
deliberate observable semantic change. It requires independent schedule
fixtures and a migration note. No implementation slice may quietly inherit
whichever order its code shape happens to produce.

## Compiler and runtime architecture

### Typed async program after closure conversion

The compiler produces an `AsyncProgram` only after type checking, ownership
checking, async segment formation, and final closure layout:

```text
AsyncProgram {
    schema_version,
    semantic_fingerprint,
    execution_shapes,
    channel_shapes,
    state_entries,
    operation_sites,
    source_map,
    dynamic_boundaries,
}
```

The schema uses dense numeric shape, state, operation, and lane IDs. An
execution shape records its ordered frame lanes, type layouts, initial state,
result lanes, initialized-lane bitmap, and final closure-layout fingerprint. A
state entry records stable callable/table identity, ordered capture-field to
frame-lane mappings, parameter and result mappings, and all successor edges.
Each edge records copies, moves, writes, initialized-bit changes, obligation
transfers or discharges, operation-site entry, and next state. Operation sites
record pending-move lanes, arm order, wait links, result lanes, and fallback
state. Terminal edges distinguish normal result, propagated value error, and
VM-aborting trap.

Generated function names are never identity. A backend binding maps the
canonical semantic IDs to final WIR table entries and capture-field indices.
The interpreter and Wasm may use different physical layouts, but tests compare
the same canonical semantic encoding and fingerprint. Closure-layout drift
invalidates the binding rather than silently reusing an old plan.

The existing suspension carrier and `ReadyStateWir` are extended to build this
representation; code generation does not re-recognize source expressions. The
schema and canonical encoder are frozen before queue, wait, and executor tracks
proceed independently.

This RFC does not depend on general closure devirtualization. A known async
closure uses its final layout. An opaque closure enters a typed dynamic call
frame under the same execution slot, turn, wait, cancellation, and queue
contracts, or is rejected at source with a named reason. Only callable dispatch
and frame layout are dynamic. RFC-0154 may later remove more of those
boundaries.

The dynamic call frame is not an erased scheduler fallback:

```text
DynamicAsyncCallFrame {
    callable_table_id,
    closure_shape_id,
    closure_layout_fingerprint,
    owned_capture_environment,
    ordered_argument_lanes,
    result_lanes,
    initialized_lane_bitmap,
    caller_return_state,
}
```

The checked closure descriptor maps capture fields and arguments into the
callee's canonical entry shape. Captures and results move according to the
callable signature. The callee inherits the current execution's scope and
cancellation state and uses its same wait record; it does not create a child or
a scheduler slot. Normal return moves result lanes and transitions to
`caller_return_state`. A value error follows the declared result type. A trap
aborts the VM. If table identity, closure fingerprint, or an ownership mapping
cannot be proven, the call is rejected before code generation.

### Uniform lowering

All supported async functions lower to this runtime, including one-way
channels, fixed and dynamic select, joins, nested async calls, actor mailboxes,
pipelines, timers, and host I/O. Every async construct is classified as:

1. lowered to the uniform execution ABI;
2. rejected before code generation with a source diagnostic; or
3. temporarily routed through an explicitly versioned migration path with an
   owner and removal row.

The old CPS `Step` tower remains only during migration and is removed before
RFC completion. No supported or named-dynamic-boundary program may retain a
second scheduler, turn rule, wait owner, cancellation rule, or queue contract.

There are no payload-specific public `Step` variants, no per-arity frame-loop
variants, no source-name matching, and no scalar executor that recognizes a
closed benchmark graph. Payload and arity specialization are generated layouts
inside the one ABI.

The expected successful-path allocation model is:

- one execution allocation per concurrently started task, with reusable frame
  and wait storage;
- one fixed queue allocation per bounded or rendezvous channel;
- typed growth chunks for an unbounded channel;
- no allocation per ready send, receive, select, wake, or async resume; and
- no allocation for a locally consumed selected result.

Dynamic async calls may allocate a typed call frame, but never one frame per
resume of the same call.

### Interpreter and Wasm

The interpreter and compiled Wasm execute the same state, operation, and turn
semantics. Their physical storage need not be identical. The interpreter is the
source-oriented oracle; Wasm uses exact scalar and GC layouts.

Wasm-first work may temporarily carry an RFC-0135 debt row. No surface
behavior, direct-runtime claim, or support status is proven until independent
expected output agrees on the interpreter, ordinary compiled mode, and the
optimized compiled mode.

## Async host operations

Capability methods may return cold `Task(T)` values. Sequential await lowers to
a compiler-known host wait descriptor. Starting such an operation registers a
bounded token with the embedder. Completion writes exact result lanes and
enqueues the execution; it does not call a guest continuation from an arbitrary
host thread.

Selection never implicitly starts an arbitrary host task. A selectable host
request is first represented by:

```text
must HostOperation(T)
```

A capability-specific `start_*` method returns `Task(HostOperation(T))` and
submits the request exactly once when awaited. Ordinary capability methods that
return `Task(T)` remain the concise sequential form.

The operation owns its capability/resource lease, operation ID, generation,
result lanes, and cancellation support. `ready(op)` is a borrowed selectable
view. Winning that arm does not duplicate or consume the operation; the winning
branch must call `finish(own op) -> Task(T)`. Losing branches retain the same
obligation and must choose `cancel(own op)`, when cancellation is supported, or
`drain(own op) -> Task(T)`. Cancellation consumes the operation only after the
host acknowledges it. A late completion before acknowledgment is drained by
the adapter; a completion with an obsolete acknowledged generation is ignored
without touching guest lanes.

Timers use the same operation contract. Capabilities and resource budgets
remain explicit. This RFC does not grant ambient networking, clocks, threads,
or executors.

## Actors and parallel work

An actor is a library-owned task plus a typed mailbox, normally a bounded
channel. The type system already supplies isolation through ownership and
message transfer. A built-in actor runtime would duplicate the task/channel
kernel without adding a missing safety property, so it is out of scope.

Cooperative tasks remain single-VM concurrency. CPU parallel work continues to
use RFC-0032 worker VMs with isolated memory, explicit serialization, and
explicit capability grants. This RFC does not add shared-memory threads, work
stealing, or nondeterministic task execution.

## Migration

### Source migration

1. Make `Task(T)` a propagated `must` obligation and diagnose multiple starts.
   Replace reused task values with functions that create tasks.
2. Add typed `Child(T)` and lexical scopes. Migrate result-over-channel patterns
   to direct child results where that improves clarity; channels remain the
   tool for streams and coordination.
3. Add the three explicit channel constructors. A type-resolved migration tool
   rewrites statically known legacy `channel(0)` and negative capacities to
   `unbounded` with a warning, preserving their current behavior. Positive
   values rewrite to `bounded`. Dynamic capacity calls receive a diagnostic and
   require an explicit branch; the tool never guesses.
4. Add channel owners and explicit close. Replace programs that depended on a
   quiescent `None` with owner close. Quiescent open-channel waits become useful
   deadlock diagnostics.
5. Replace immediate `cancel(handle)` with request plus await and explicit token
   observation.
6. Add select syntax, then implement old fixed-arity functions as wrappers.
7. Change deterministic scheduling to operation-level turns only after the new
   schedule matrix and migration output are reviewed.

Before the semantic cut, a compiler-produced caller census covers std, tests,
examples, book, projects, benchmark programs, package fixtures, and every
resolved use of Task construction/consumption, Handle, channel construction,
quiescent close, cancellation, and select. It classifies mechanical rewrites
separately from ownership or schedule decisions.

Witchy is pre-0.1 and RFC-0061 explicitly defers editions. These changes land
as one documented release cut, following the one-cut precedent of RFC-0087.
Temporary migration diagnostics and tooling may exist on the implementation
branch, but duplicate semantics and compatibility shims are removed before the
release. External source packages receive the same resolved diagnostics.
Previously compiled artifacts are tied to compiler/runtime build identity and
are rebuilt; this RFC does not invent an edition marker.

### Implementation migration

Reusable work from RFC-0152 includes typed carrier facts, ownership envelopes,
async-region formation, `ReadyStateWir`, semantic fixtures, counters, WAT
inspection, and the measurement harness. The typed-select integration branch is
the only reasonable prototype convergence point, but it is not merged as-is.

The following are discarded as architectural endpoints:

- widening the scalar executor to recognize more task graphs;
- source or generated-name matching;
- a parallel typed-Int queue next to the erased queue;
- per-payload or per-select-arity `Step` variants;
- treating a frame-loop bridge as proof of direct execution; and
- treating absence of one nominal union constructor as zero-allocation proof.

Uncommitted prototype diffs are preserved on recovery branches before any
worktree cleanup. Facts, tests, and counter code are cherry-picked only after
they are reviewed against the frozen `AsyncProgram` and queue interfaces.

## Delivery dependency graph

```text
surface semantics + migration matrix + independent expected schedules
                              |
          +-------------------+-------------------+
          |                   |                   |
 typed AsyncProgram ABI   typed queue ABI   wait/select protocol
          |                   |                   |
          +-------------------+-------------------+
                              |
              one end-to-end execution kernel
                 /            |            \
          interpreter     compiled Wasm     host readiness
                 \            |            /
                  semantic + ownership matrix
                              |
          allocation, latency, scaling, and WAT evidence
                              |
              source migration + old CPS removal
                              |
                    merge-queue full gate
```

The three ABI tracks can proceed independently after their shared IDs,
ownership transitions, and fallback boundary are frozen. One integration track
owns the runnable end-to-end slice. The merge queue serializes landing, not
implementation.

## Acceptance criteria

RFC-0152 criteria 1 through 10 are imported without weakening:

1. Independent expected output agrees on interpreter, ordinary Wasm, and
   optimized Wasm for ready, blocking, close, cancellation, and backpressure.
   Exact transition traces cover send-wakes-receive, receive-wakes-send, spawn,
   child completion, close, cancellation, default, and scripted host admission.
2. Ready Int, String, and packed-record paths allocate no steady-state heap and
   install no waits after warmup.
3. Blocking uses at most one live reusable registration per operation site and
   retained heap does not grow with message count or with repeated waits.
   Many-producer, many-receiver, static-select, duplicate-endpoint, and dynamic
   descriptor cases prove intrusive-link allocation bounds.
4. WAT proves exact typed lanes and no nominal selected-result allocation for a
   local match.
5. Int, String, and packed records are supported or have typed named rejection
   evidence.
6. `select_fanin` is at most 1.00x the matched deterministic Go control with the
   same tie rule, or this RFC remains incomplete.
7. `chan_throughput`, `chan_scheduler`, an actor, and a pipeline include at
   least two applicable improvements of 20 percent or more, including one
   pre-existing program, with no protected regression above 5 percent.
8. Blocking and cancellation p99 do not regress by more than 5 percent.
9. Concurrency-corpus compile time and emitted Wasm size each grow by less than
   8 percent.
10. The merge-queue gate and durable raw samples, WAT, counters, correctness
    hashes, and build identity are recorded.

This RFC adds:

11. Task affinity, typed child results, branded non-escaping scopes, early
    `return`/`?`, and `must` transfer are accepted or rejected with source-level
    diagnostics across aggregates, collections, closures, generics, loops,
    partial consumption, suspension, and dynamic-value boundaries. Winning and
    losing child-completion select arms preserve exactly one child obligation.
12. Rendezvous, bounded, and unbounded channels; explicit close; drain after
    close; send-after-close value recovery; open-channel deadlock; multi-waiter
    order; rendezvous pairing; duplicate select endpoints; and every
    probe/register/close interleaving have an independent semantic matrix.
    Ordinary and select sends restore or return `must` payloads on every losing,
    cancellation, close, and failed-registration path.
13. Cancellation never abandons a `must` value. Request-before-wait,
    request-during-wait, wake-versus-request, close-versus-request, repeated
    request, child-result selection, scope `return`/`?`, and a non-cooperating
    child are covered with exact traces.
14. The compiled concurrency corpus uses one execution ABI and one queue ABI.
    Every supported and named-dynamic-boundary path shares execution slots,
    turns, waits, cancellation, and queues. A machine-checked census rejects old
    CPS `Step` construction, scalar graph recognizers, payload/arity-specific
    runtime entry points, and any second semantic scheduler after cutover.
15. Nested async calls, loops, branching, mutation, early return, traps,
    generic payloads, dynamic homogeneous select, actor, and pipeline programs
    all use the same kernel or are rejected at source. Canonical `AsyncProgram`
    encoding, closure-layout invalidation, initialized-lane transitions,
    cancellation cleanup, trap termination, and interpreter/Wasm semantic
    fingerprints are tested.
16. At least one timer and one capability-host I/O fixture prove bounded
    registration, scripted event-trace parity, winning finish, losing drain,
    acknowledged cancellation, late completion, stale generation, and no
    capability or `must HostOperation` leak.
17. A type-resolved census and migration tool cover every repository and public
    package-fixture caller of the changed Task, Handle, channel-construction,
    close, cancel, and select APIs. The one-cut release removes temporary
    diagnostics, compatibility paths, and stale compiled artifacts.
18. A clean installed binary runs the structured bounded workflow and reports a
    deliberately constructed deadlock without repository-only state.

The companion acceptance ledger is the authority for progress. A prototype,
local artifact, or faster benchmark row does not complete a criterion.

## Alternatives

### Continue RFC-0152 as incremental ready paths

The landed facts remain useful, but continuing to add bridges leaves the CPS
scheduler, special scalar executor, and direct prototypes competing for
authority. The uniform runtime makes those facts inputs to one lowering.

### Preserve replayable tasks

A replayable cold recipe is easy to implement with a thunk but hard to combine
with captured ownership, capabilities, typed child identity, and allocation-
free activation. A function returning an affine task expresses reusable work
without ambiguity.

### Preserve quiescence closure

It avoids an owner handle, but it lets a retained sender send after a receiver
has observed closure and turns deadlock into data. Explicit ownership makes
lifecycle and failure honest without relying on destructors.

### Abruptly cancel executions

Dropping a frame is incompatible with static `must` obligations and explicit
cleanup. Cooperative tokens can be slower to stop a broken child, but never
pretend its resources were discharged.

### Use host futures, threads, or a work-stealing executor

They would change deterministic ordering, capability accounting, portability,
and backend parity. Host readiness is integrated as an event source instead.

### Add actors as a second concurrency primitive

Tasks plus owned typed channels already provide actor isolation and mailboxes.
A library abstraction keeps one runtime and permits different supervision
policies.

## Drawbacks

- This is a preview-breaking surface migration, not a hidden optimizer change.
- Explicit channel owners and cooperative cancellation require more lifecycle
  code, though structured helpers cover common cases.
- Exact payload layouts and typed frame transitions enlarge the compiler/runtime
  ABI and its test matrix.
- Dynamic async calls and opaque select collections still have visible costs.
- Operation-level task turns change some existing deterministic output orders.
- A non-cooperating cancelled child can delay scope exit indefinitely.
- The 1.00x-Go acceptance target is aggressive and may require improvements in
  generated Wasm and Wasmtime interaction beyond the scheduler itself.

## Prior art

- Rust's `Future` separates a lazy computation from polling and wakeup, and its
  ownership model motivates affine task activation. Witchy does not adopt
  `Pin`, raw wakers, or executor plurality.
- Go demonstrates channels, blocking send/receive, and communication-oriented
  `select`. Witchy retains deterministic source-order priority instead of
  pseudo-random ready-arm choice and adds explicit channel ownership.
- Swift structured concurrency motivates lexical child lifetimes, typed child
  results, and scope exit that waits for children. Witchy omits detached tasks.
- .NET `System.Threading.Channels` demonstrates typed bounded and unbounded
  queues, backpressure modes, and async readiness. Witchy folds those ideas into
  its deterministic single-VM scheduler.
- Pony demonstrates that actors can be a safe library-facing model when
  messages and mutable authority obey strong ownership rules. Witchy retains
  actors as a pattern over its existing qualifiers.
- CSP supplies the rendezvous and guarded-choice foundation. This RFC combines
  it with typed state-machine lowering rather than a generic runtime object
  graph.

See `external-refs/typed-resumable-runtime/notes.md` for the primary sources and
the specific ideas adopted or rejected.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior lives in spec/ and code, not here.
-->
