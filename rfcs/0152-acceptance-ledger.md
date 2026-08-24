---
rfc: 0152
title: "RFC-0152 acceptance ledger"
status: active
created: 2026-08-24
---

# RFC-0152 acceptance ledger

This ledger records implementation and measurement evidence for
RFC-0152.  A row is complete only when its linked source, tests, artifacts,
and merged commit are all present.  A planned result is not evidence.

## Frozen baseline

Baseline revision: `79a0c3cf4` (2026-08-24).

The existing scalar executor accepts only a wholly closed task graph.  In
particular, a leading `clock.now_monotonic()` in the enclosing async block
causes `SuspensionCarrierCatalog::scalar_executor_plan` to reject the block as
an unsupported awaited expression.  The compiled main then calls `task.run`.
The fallback constructs task/continuation frames on its resume path.  This is
the Phase-0 attribution, not a claim that capability effects are removable:
the effect must execute once, in source order, outside any promoted region.

The existing closed `chan_throughput.witchy` control remains scalar-fusible;
`benchmarks/chan_scheduler.witchy` is the intentional nominal-packet
scheduler control.  They must remain separate in all timing reports.

## Landed implementation increments

| Increment | Merged commit | Evidence | Acceptance impact |
|---|---|---|---|
| Typed endpoint ABI | `92b89e1dc` | `typed_channel_plans_preserve_endpoint_direction_and_buffer_root_identity`; `awaited_select_uses_the_fused_typed_bridge_in_wir` | Retains endpoint direction, Int scalar lanes, String/Bytes buffer roots, packed record lanes, and ownership envelopes through the carrier-to-lowerer seam. No ready path is enabled by this increment. |

The async-region identity increment is queued separately at the time of this
entry and is not recorded as landed evidence until its merge event exists.

## Pre-existing program controls

These repository programs predate RFC-0152 and are the non-benchmark
generalization controls.  Their existing observable output and backend parity
are the semantic oracle; RFC-0152 may add harnesses around them but may not
rewrite their task topology to obtain a promotion.

| Control | Existing source | Exercised behavior | Status |
|---|---|---|---|
| channels example | `examples/channels/src/channels.witchy` | bounded send/receive and deterministic task scheduling | selected; baseline pending |
| worker-pool example | `examples/worker_pool/src/worker_pool.witchy` | channel workers, joins, and backpressure | selected; baseline pending |

The actor/pipeline fixture required by RFC-0152 will use one of these controls
when it has the required message topology.  If neither has a suitable fixed
arity select, the additional fixture must live outside `benchmarks/` and this
table must name it before it is counted toward acceptance.

## Measurement protocol

Every promotion row uses the same source revision for ordinary scheduler,
current fusion, and candidate controls.  Record two warmups, then twelve
interleaved paired samples.  Record three deterministic counter payloads; all
three must match exactly.  Do not combine ready and blocking samples.

Required counters:

| Counter | Ready expectation | Blocking expectation | Owner/status |
|---|---|---|---|
| allocation calls and bytes | zero steady-state scheduler allocation | bounded; retained heap does not grow with messages | Phase 0: missing scheduler-specific attribution |
| peak heap delta | flat after warmup | flat with stable endpoint set | Phase 0: missing scheduler-specific attribution |
| scheduler steps | direct ready path only | cooperative suspension retained | Phase 0: missing |
| wait registrations / wake claims | zero / zero after warmup | at most one live registration / one claim per wake | Phase 0: missing |
| selected-result materializations | zero for local non-escaping match | zero until an escaping boundary | Phase 0: missing |
| CPU and p50/p95/p99 | report separately | report separately | Phase 0: harness pending |

## Matrix and acceptance status

| RFC criterion | Required evidence | Status | Merged commit / artifact |
|---|---|---|---|
| 1. semantic parity | independent expected outputs: interpreter, scheduler Wasm, optimized Wasm; ready, blocking, close, cancellation | pending | — |
| 2. ready allocation-free | Int, String, packed record; capacities 1, 8, 2048; WAT and counters | pending | — |
| 3. bounded wait state | capacities 0, 1, 8; repeated wake/cancel/close; retained-heap counters | pending | — |
| 4. direct result lanes | WAT for non-escaping local match; no nominal selected-result allocation | pending | — |
| 5. payload coverage | Int, String, packed record promoted or a typed named rejection | pending | — |
| 6. select_fanin | matched deterministic Go control at or below 1.00x | pending | — |
| 7. generalization | `chan_throughput`, `chan_scheduler`, actor, pipeline; two >=20% improvements; no protected regression >5% | pending | — |
| 8. tail latency | blocking p99 and cancellation p99 no worse than 5% | pending | — |
| 9. build footprint | concurrency corpus compile time and Wasm size <8% growth | pending | — |
| 10. landing evidence | queue gate, raw samples, WAT, counters, correctness hashes | pending | — |

## Delivery dependency graph

```text
Phase 0 counters + matrix ─┬─> async-region formation ─┬─> typed one-way ready paths
                            │                            └─> fixed-arity select lanes
                            └─> wait-registration ABI ─────> reusable wait registration
                                                                  │
                              semantic/counter matrix ───────────┴─> full evidence + gate
```

Independent branches may land the three middle tracks separately, but each
must update the matrix rows it changes and provide an independently runnable
measurement artifact.  The final evidence row remains blocked until all three
tracks use the same typed channel-plan ABI.
