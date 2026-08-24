---
rfc: 0152
title: "Deterministic CSP Ready Paths: Typed Channels, Fixed-Arity Select, and Bounded Suspension"
status: proposed
created: 2026-08-24
superseded-by:
related:
  - "0055 (typed channel messages)"
  - "0129 (concurrency, tasks, and channels)"
  - "0137 (compiler-derived CSP)"
  - "0139 (high-ROI channel optimization)"
  - "0146 (post-hybrid performance program)"
tracking: "Proposes general ready-path and suspension-path specialization for deterministic typed CSP; implementation evidence will live in an RFC-0152 acceptance ledger."
---

# RFC-0152: Deterministic CSP Ready Paths

## Summary

RFC-0146 landed a compiler-owned frame emitter for one closed two-way select
shape and reduced `select_fanin` from roughly 11.4 ms to 1.8 ms. The result is
an important improvement, but it remains about 20x slower than the matched Go
ready-select control, and ordinary program structure can still prevent fusion.

This RFC generalizes the mechanism around semantic facts rather than one task
expression shape:

1. Form maximal eligible async regions even when effectful prologue or epilogue
   statements surround them.
2. Carry exact channel payload layouts through ready receive, fixed-arity
   select, suspension, wakeup, close, and cancellation.
3. Inline the already-ready path for statically known receivers while retaining
   one bounded compiler-owned wait frame when no endpoint is ready.
4. Remove steady-state task, closure, list, and tagged-result allocation when
   the selected value is consumed locally.

The normal Witchy scheduler remains the semantic implementation and the
fallback for dynamic, escaping, opaque, or unsupported task graphs.

## Motivation

The 2026-08-24 fast snapshot measured `select_fanin` at 1.8 ms versus 0.1 ms
for Go, a 20.02x ratio. The measured region contains 4,096 receives from two
already-filled channels. The absolute gap is small, but the per-selection cost
is representative of actor mailboxes, request multiplexers, fan-in pipelines,
and worker coordination.

The benchmark repair that followed RFC-0146 also exposed a general compiler
boundary: placing an effectful clock statement before an awaited task graph
could make the scalar executor reject the whole enclosing block. Removing the
clock restored the fast path, but real programs contain logging, metrics,
setup, cleanup, and capability calls around async regions. An optimizer that
requires an entire function to be a closed task expression will not generalize.

Witchy's advantages are exact message types, no shared mutable state, known
deterministic tie order, and structured task ownership. Those facts should
permit less runtime work than a generic dynamically typed scheduler, provided
they survive lowering.

## Goals

1. Make the cost of an already-ready receive approach a direct ring-buffer
   read plus the user's continuation.
2. Represent supported selected results in typed scalar or exact-layout lanes,
   without an allocated `First` or `Second` carrier.
3. Allocate no steady-state scheduler objects for a repeatedly ready fixed-
   arity select.
4. Allocate at most one reusable wait registration per active select site when
   suspension is required.
5. Preserve Witchy's deterministic first-ready rule, close semantics,
   cancellation, ownership transfer, and bounded-channel behavior exactly.
6. Apply the mechanism to ordinary actor and pipeline programs, not only a
   closed benchmark graph.

## Non-goals

- Shared-memory channels, lock-free host threads, work stealing, or a
  nondeterministic scheduler.
- Busy waiting when every endpoint is empty.
- Changing `chan.select` source semantics or its public result type.
- Making arbitrary dynamic receiver lists allocation-free in the first phase.
- Special handling keyed by `select_fanin`, function names, local names, or
  benchmark constants.
- Replacing the Witchy scheduler with host Go, Rust, or Wasmtime async tasks.

## Semantic invariants

An optimized execution is observably identical to `std/task.witchy` and
`std/chan.witchy`:

1. Receivers are tested in source order. If several are ready at the same
   scheduler step, the first ready arm wins.
2. A send transfers one value according to Witchy's value and ownership rules.
   The sender cannot observe later receiver mutation.
3. Closed endpoints produce the same `Closed` result and wake the same blocked
   operations as the source scheduler.
4. Cancellation cannot lose a value, consume twice, leave a stale waiter, or
   resume a continuation after its structured owner has completed.
5. Suspension yields cooperatively. The ready path may bypass scheduler data
   structures but may not change the points at which an actually blocking
   operation yields.
6. Traps and host effects before the async region occur exactly once and in
   source order. Effects after it occur only after the same completion state.

## Compiler design

### Async region formation

The lowerer identifies maximal single-entry async regions inside a function.
An effectful statement before a region is emitted normally, then its resulting
locals become explicit region inputs. An effectful statement after a region is
an explicit continuation boundary.

Region formation never deletes, duplicates, or moves a statement across a
suspension. Unsupported statements split a region rather than rejecting the
entire function. This directly addresses the leading-block limitation without
special-casing `Clock`.

### `TypedChannelPlan`

Each eligible endpoint carries:

```text
TypedChannelPlan {
    endpoint_place,
    payload_layout,
    ownership_envelope,
    ring_descriptor,
    close_state,
    escape_state,
}
```

The ring descriptor is an internal ABI owned by WIR and the scheduler. This RFC
does not freeze a public header size. Exact layouts include scalar Int, Bool,
Float, buffer roots such as String and Bytes, and packed records/tuples whose
ownership envelope is already representable. Unknown or unsupported layouts
fall back before code generation.

### `ReadySelectPlan`

Fixed-arity `select` over two through four statically known endpoints lowers to
a plan with source-order arms, exact result lanes, and one fallback frame ID.
The first implementation may promote arity two alone, but the descriptor and
tests cover arity independently so a second per-arity architecture is not
introduced.

The ready path performs:

1. load stable ring state for arm 0;
2. if ready, consume exactly one payload and branch to arm 0;
3. repeat in source order;
4. if no arm is ready, enter the reusable wait path;
5. return an unboxed arm tag plus exact payload lanes to the local match.

The local match is lowered directly when the selected value does not escape.
If it is stored, returned, reflected, passed to an opaque callable, or otherwise
requires its nominal union representation, normal constructor materialization
occurs once at that boundary.

### Suspension and wakeup

The no-ready path installs one compiler-owned registration that names all
participating endpoints, source-order priority, destination frame, and payload
layout. A wakeup claims the registration once and writes into the destination
lanes. Cancellation and close use the same claim protocol.

The registration is reused across loop iterations when its owner and endpoint
set are stable. Dynamic endpoint replacement, escape, nested opaque suspension,
or unsupported cancellation state invalidates reuse and uses the ordinary
scheduler.

### One-way receive and send

The same channel plan lowers one-way send and receive. A ready receive returns
directly. A ready send writes directly when capacity and ownership permit. An
empty receive or full send enters the bounded wait path. This is necessary so
real pipelines benefit even when they never call `select`.

## Applicability and fallback

Eligibility depends on typed and ownership facts:

- exact endpoint and message layout;
- statically bounded fixed-arity receiver set for select;
- non-escaping compiler-owned frame;
- representable cancellation and close state;
- no opaque callable that can retain the endpoint or continuation inside the
  promoted region.

Failure of any condition selects the ordinary scheduler at compile time. There
is no partial fast path that silently changes behavior. Deterministic reason
counters record region splits, ready-path promotion, wait-frame promotion,
result materialization, and each rejection class.

## Generalization matrix

Promotion uses the following families at capacities 0, 1, 8, and 2,048 and at
message counts from 1,000 through 1,000,000 where practical:

| Family | Payloads and states |
|---|---|
| one producer/consumer | Int, String, packed record; ready and blocking |
| two-way select | left ready, right ready, both ready, neither ready, close |
| four-way fan-in | skewed readiness, balanced readiness, mixed close |
| actor mailbox | request/response records with effectful setup and logging |
| pipeline | three stages with backpressure and bounded capacities |
| cancellation | cancel before wait, during wait, after wake, repeated close |
| escape control | selected union returned or passed to an opaque callable |

The actor and pipeline fixtures are ordinary Witchy programs outside the
benchmark directory. Their outputs are checked independently on both backends.
Phase 0 must select at least two pre-existing repository async programs or
examples before implementation begins; newly written microfixtures cannot be
the only evidence for generalization.

## Measurement contract

The concurrency sweep records throughput, per-message median and p95/p99,
allocation calls/bytes, peak heap delta, scheduler steps, wait registrations,
wake claims, select result materializations, and CPU utilization. Ready and
blocking cases are reported separately; averaging them together is invalid.

Each optimized row has scalar-scheduler, current compiler fusion, and candidate
controls built from the same source revision. Three deterministic counter runs
must agree exactly. Timing promotion uses two warmups and twelve paired
interleaved samples.

## Acceptance criteria

1. All semantic matrix cases match independent expected output on interpreter,
   ordinary Wasm scheduler, and optimized Wasm.
2. The ready path performs zero steady-state heap allocations and zero wait
   registrations after warmup.
3. The blocking path uses at most one live registration per select site and
   does not grow retained heap with message count.
4. Generated WAT proves direct typed payload lanes and absence of nominal
   selected-result allocation in a non-escaping local match.
5. Int, String, and packed-record payloads each have a promoted path or a named
   rejection with evidence showing why their ownership envelope cannot yet be
   represented.
6. `select_fanin` reaches at most 1.00x of the matched deterministic Go control
   with identical tie behavior, or the RFC remains incomplete.
7. `chan_throughput`, `chan_scheduler`, the actor fixture, and the pipeline
   fixture improve by at least 20 percent on two applicable rows, including one
   pre-existing repository program, and do not regress by more than 5 percent
   on any protected row.
8. Blocking latency p99 and cancellation latency do not regress by more than
   5 percent against the ordinary scheduler.
9. Compile time and emitted Wasm size for the concurrency corpus each grow by
   less than 8 percent.
10. The merge-queue gate passes and the ledger records merged commits, durable
    raw samples, WAT, counter payloads, and correctness hashes.

## Delivery slices

1. Phase 0: freeze the full ready/blocking/capacity/payload matrix and add
   missing allocation and scheduler counters.
2. Phase 1: land async region formation with effect-order tests and no enabled
   optimization.
3. Phase 2: freeze `TypedChannelPlan`, exact result lanes, and compile-time
   rejection reasons.
4. Phase 3: land one-way ready receive/send and measure independently.
5. Phase 4: land two-way ready select and local result scalar replacement.
6. Phase 5: land reusable wait registration, close, and cancellation.
7. Phase 6: expand eligible payload layouts and arities only after each prior
   slice meets its whole-workload gate.

Each phase is independently revertible. A helper-only rewrite that retains the
same allocations is mechanism exploration, not promotion evidence.

## Relationship to RFC-0146

RFC-0146 Track 1 remains the historical record of the first compiler-owned
select frame and its measured 84 percent improvement. This RFC does not rewrite
that implemented work. It owns the remaining general ready-path, region,
payload-layout, one-way operation, and bounded-suspension decisions.

## Alternatives

### Optimize only the two-way benchmark shape

This would leave ordinary prologues, one-way channels, record payloads, and
blocking programs on the slow path. Region and typed-channel descriptors make
the same work reusable.

### Spin until a channel becomes ready

Spinning can look fast when producers prefill a benchmark. It violates
cooperative scheduling, wastes CPU, and fails under real backpressure. The
bounded wait frame is required.

### Delegate concurrency to host threads

Host threads would change determinism, isolation, resource accounting, and
deployment behavior. They are a different concurrency model, not an
optimization of Witchy's CSP semantics.

## Drawbacks

- The compiler and scheduler share a more explicit internal ABI.
- Exact payload lanes multiply test cases across layouts and ownership forms.
- Region splitting around effects is subtle and must preserve evaluation order.
- Small programs may gain code size without enough messages to amortize it;
  the cost model must decline those sites.
- Matching Go's ready-path latency is an aggressive host-specific target and
  may expose a Wasmtime backend ceiling.

## Prior art

- Go channels provide the performance control but not Witchy's deterministic
  scheduling contract.
- Coroutine frame scalar replacement and producer-consumer fusion inform the
  compiler-owned frame design.
- CSP implementations commonly separate an optimistic ready probe from a
  registered blocking path.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior lives in spec/ and code, not here.
-->
