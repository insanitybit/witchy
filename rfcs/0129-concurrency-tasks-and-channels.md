---
rfc: 0129
title: "Deterministic async tasks, typed channels, and explicit parallel workers"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical concurrency RFC. Async/await, typed channels, deterministic scheduling, structured combinators, compiled memory bounds, and backend parity are shipped for bounded workloads. Acceptance rows 1-7 are proven by the named executable evidence below. Logically unbounded service storage remains outside the supported-preview promise."
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

## Message carrier ABI

The scheduler remains monomorphic over the opaque `__Msg`, while checked
`Sender(T)` and `Receiver(T)` endpoints retain the message type. On compiled
WebAssembly, `__Msg` is one GC envelope with i32, i64, f64, externref, and
erased anyref fields. The anyref field carries either a GC struct or GC array,
so reference-bearing aggregates and lists share the same scheduler ABI without
an integer bit-cast. `send` writes the field matching its checked `T`, and
`recv` statically selects that field and casts an erased structural reference
back to its known GC type.

This is semantic erasure, not a universal scalar-slot identity. It permits
heterogeneous scalar, host-reference, and direct-reference messages under one
scheduler ABI without bit-casting references through integers. The envelope is
opaque to scheduler code, and endpoint pairing preserves the erase/recover type
identity.

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

The compiled backend has two measured resumable paths. A qualifying scalar
producer/consumer loop is synthesized as fixed task, channel, and message
state; its one-million-message gate uses no Wasmtime GC task heap and rejects
per-resume linear allocation. The general GC-backed scheduler reuses task slots
and direct aggregate carriers; the nominal-aggregate soak holds Wasmtime's
backing heap at one 65,536-byte page as traffic grows from 100 to 10,000
messages.

Those gates establish the supported-preview contract for the promoted compiled
shapes and bounded workflows in this RFC. `unbounded` continues to describe
logical channel capacity rather than infinite process storage. An application
must use a bounded channel or separately measure its concrete payload and
control-flow shape before treating the channel as an indefinitely running
service queue.

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

### Acceptance evidence

| Row | State | Executable evidence |
| --- | --- | --- |
| 1 | PROVEN | `tests/misc/rfc0129_async_lowering.rs::rfc0129_acceptance_row_1_async_lowering_preserves_source_contracts_on_both_backends` runs mutable loop state, repeated suspension, early return, and a source trap on the interpreter and compiled Wasm. It also inspects the typed suspension-carrier catalog for the exact live `Int` slots and proves the trap retains the source callable and line without leaking generated `__async_` identities. |
| 2 | PROVEN | `tests/misc/rfc0129_channel_contract.rs::rfc0129_acceptance_row_2_typed_bounded_selected_cancelled_and_joined_channels_agree` executes `Int` and `String` channels, capacity-one backpressure, first-ready selection, quiescent closure, cancellation of a parked child, and structured scope/join. It requires the exact 18-line schedule to agree on the interpreter and compiled Wasm. |
| 3 | PROVEN | `tests/rfc0129_schedule_parity.rs::rfc0129_acceptance_row_3_deterministic_schedule_backends_agree` executes the canonical 13-line schedule from `tests/fixtures/rfc0129_deterministic_schedule.witchy` on compiled Wasm first and then the interpreter. It covers bounded backpressure, quiescent join release, deterministic select order and closure, cancellation, retained-sender quiescence, structured joins, nominal aggregate/list carriers, and a distinct string carrier. Rows 2 and 3 landed together at `5c040cc0` after their focused 99-test coordinator gate passed. |
| 4 | PROVEN | `tests/rfc0129_scalar_executor_performance.rs::million_message_scalar_executor_meets_resumption_cost_and_allocation_gate` executes the real compiled-Wasm producer/consumer benchmark for one million messages, checks its exact fold, proves the scalar entry bypasses `task.run`, requires zero GC task-heap capacity, rejects 100 or more linear allocations, and enforces a 300 ns/message median ceiling. `aggregate_channel_gc_heap_capacity_is_flat` forces the GC-backed scheduler with nominal `Packet(Int, Int)` messages, checks both folds, and pins the same 65,536-byte backing capacity at 100 and 10,000 messages. Together they show that the promoted scalar and aggregate paths do not grow a continuation chain with the message count. |
| 5 | PROVEN | `tests/misc/rfc0129_negative_matrix.rs::rfc0129_acceptance_row_5_rejects_invalid_concurrency_sources_before_lowering` rejects reference escape across suspension, capability transfer through a worker result, dedicated async-trait syntax, and discarded task results. Every case checks source-level diagnostic fragments and proves rejection occurs at parse or the pre-lowering source semantic boundary. |
| 6 | PROVEN | `tests/misc/rfc0129_cooperative_worker_boundary.rs::rfc0129_acceptance_row_6_cooperative_and_parallel_maps_use_distinct_boundaries` executes both APIs on the interpreter and compiled Wasm. It proves that `chan.par_map` keeps its explicitly passed `Console` in one VM with zero worker imports; pure `vm.par_map` crosses the two-phase `vm_par_map_run`/`vm_par_map_write` worker boundary; a capability-capturing callback stays in the parent VM; and `vm.with_dir` grants exactly one explicit `Dir`. `examples/channels/src/cooperative_map.witchy` and `worker_vm_map.witchy` are separate runnable examples, and their README records the distinct API and cost contracts. The exact measurement is the stable zero-versus-two worker-host-interface boundary, not an elapsed-time claim. The merge-coordinator landing at `22968974` passed all 111 selected tests, including this test and the example parity/validation sweeps. |
| 7 | PROVEN | `scripts/installed-bounded-channels-smoke.sh --witchy <binary>` copies the public binary into a fresh install root, resolves only that copy on `PATH`, and uses `env -i` plus fresh home, temporary, cache, and working directories for `check`, `build`, and `run`. It creates the capacity-one `bounded_channels` project and checks its exact deterministic drain output without repository source, standard-library, package, or cache state. `scripts/release-smoke.sh` invokes the same harness on the extracted archive, and `.github/workflows/release.yml` runs it for every supported native release target. The initial installed-workflow slice passed the 584-test coordinator batch landed at `c8ff194a`; the environment-scrub hardening passed the 111-test coordinator gate landed at `22968974`. |
