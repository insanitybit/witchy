---
rfc: 0152
title: "RFC-0152 acceptance ledger"
status: superseded
created: 2026-08-24
superseded-by: "0161 (typed resumable runtime)"
---

# RFC-0152 acceptance ledger

> **Superseded (2026-08-26).** RFC-0161 replaces RFC-0152's incremental
> ready-path program. The landed increments, counters, fixtures, and raw
> attribution referenced below are retained as RFC-0161 inputs. Every pending
> RFC-0152 acceptance row remains unproven; this ledger is historical rather
> than an active implementation queue.

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
| Deterministic scheduler transition counters | `be26b292a`, corrected control test `b26aaec6e` | Wasm exports `scheduler_slot_transitions` and `scheduler_wait_installs`; `nominal_packet_channel_uses_the_scheduler_control_path` | Phase-0 attribution can distinguish ordinary scheduler slot transitions and wait installations. It does not yet count ready-path allocation, live registration identity, or wake claims. |
| Fixed-arity public scheduler selections | `87dff4448` (gate artifact rebaseline `7004a587e`) | `rfc0129_acceptance_row_2_typed_bounded_selected_cancelled_and_joined_channels_agree` exercises deterministic `select3` and `select4` source-order results | Adds typed three- and four-way ordinary scheduler forms. This is a fixed-arity fallback contract, not evidence of a compiler direct-ready select path. |
| Typed ready-region operation descriptor | `4789cac6f` | `ready_region_operations_keep_state_order_and_typed_channel_facts`; existing ready-region qualification tests | Exposes stable state-ordered open/send/receive/select2 operations to codegen with exact payload and endpoint facts. No codegen consumer or ready-path execution is enabled by this descriptor alone. |
| Ready-plan consumer seam | `5eaa9ef8c` | `scalar_channel_region_preserves_an_effectful_eager_prologue` | The existing direct scalar graph now receives the complete `ReadyChannelRegionPlan`, including an effect-preserving prologue. Its graph recognition remains narrow and is not general ready send/receive. |
| Independently eligible async regions | `a3e170d88`, guarded fallback repair `9b298b1d1` | formation/sibling/guarded/successor carrier units; lowered `task.map` and guarded-control units; interpreter, ordinary Wasm, and optimized Wasm parity fixtures | Forms static, unguarded channel paths independently in dense state order while leaving unsupported siblings visible as named exclusions. Spawn/call successors are complete. Guarded or opaque task construction remains an explicit `GuardedControlFlow` scheduler fallback until an emitter can lower predicates exactly once; the legacy all-or-nothing scalar plan retains its original collector. This is formation metadata only, not a direct-ready executor. |
| Ready-state WIR body capture | `f77c3bcb4`, nested-prefix repair `1da51875a` | protected `witchy-lower` gate: `ready_state_wir_capture_preserves_effectful_prefix_and_excludes_non_ready_paths` | Captures an individually eligible state’s exact lowered WIR prefix and terminal task boundary without changing runtime selection. Nested effectful prefixes remain once-only; guarded and unsupported paths have no capture. This is the future fallback-body seam, not a ready-path execution or typed scheduler-storage ABI. |
| Fixed three-way select frame loop | `a94c90e3`, `2bc7ba259`, `f28117ed9`, `c3a20360d` (gate rebaseline `a86f738e7`) | syntax/lowerer frame-bridge checks and `rfc0152_select3_frame_preserves_source_order_and_closed_wake_on_both_backends` | Adds a typed three-way compiler-owned frame bridge, preserves mutable frame semantics, and avoids erased-payload recovery when every arm is a wildcard. This lowers continuation allocation; it does not bypass `task.run` or prove a direct-ready path. |
| Fixed four-way select frame loop | `3dd02fba0`, `c61646f19`, `ccdc75b84`, `ca849b367`, `841164a41`, `8094f39e1` (gate rebaseline `357ebb6ca`) | `select4_loop_uses_the_fixed_arity_frame_bridge`; `proven_four_way_select_loop_preserves_source_order_in_the_frame_bridge`; full merge-queue gate | Adds the bounded four-receiver compiler-owned frame bridge and typed carrier facts. The lowerer keeps source-order endpoint probes and can retain a locally consumed payload lane without constructing `Selected4`; escaping matches retain the ordinary decode fallback. This remains a scheduler-frame increment, not a direct ready-ring emitter. |
| Matrix-capable deterministic stats | `52572918e` | `stats_grants_clock_for_in_program_benchmark_kernels`; CLI `stats benchmarks/chan_scheduler.witchy` | Grants the safe local Clock capability to stats runs so timed channel fixtures can report deterministic counters. This is measurement infrastructure, not a scheduler optimization. |
| Reusable frame-install counter | `960123bd7` | `reusable_select_frame_reports_one_initial_wait_installation` | Separates the compiler-owned initial reusable-frame installation from ordinary cumulative `Wait*` constructor counts. It is an attribution primitive, not a proof of bounded live registration across every close/cancel path. |
| Scheduler wake/materialization counters | `15985d494` | deterministic `scheduler_wake_claims` and `selected_result_materializations` exports; `selected_result_materialization_counter_distinguishes_the_nominal_adapter` | Counts ordinary scheduler wake claims and nominal `Selected*` construction deterministically. This is Phase-0 attribution only: it does not prove a direct result lane, identify a live registration, or account for scheduler-specific allocation and retained heap. |
| Sole-live ready select drain | `a700c1f12` (gate rebaseline `068180131`) | `sole_live_select_frame_drains_a_buffered_source_order_run`; WAT contains `task.is_only_live_slot`; protected `channels` and `worker_pool` examples retain interpreter/Wasm parity | When a reusable two-way frame is the sole live slot, the scheduler drains source-order buffered messages through the existing `step`/`done_fn` closures without re-entering an outer scheduler round per message. Competing active or parked slots retain ordinary round-robin scheduling. This is a runtime ready-path increment, not the general typed lowerer executor required by the remaining rows. |
| Payload-free receive-frame carrier | `222e2067b` (queued) | `recv_loop_uses_reusable_frame_bridge_for_scalar_fold`; `scalar_receive_frame_retains_typed_ready_region_facts` | Lowers a payload-free `for await` loop onto a reusable one-way frame while retaining its static endpoint, generated step/done identities, and close continuation in the typed ready-region plan. Value-consuming loops, nominal payloads, and mutable roots retain ordinary lowering until direct typed payload lanes are emitted. |
| Sole-live ready receive probe | `3942e0278` (gate rebaseline `533f780c2`) | `rfc0152_payload_free_receive_frame_uses_the_sole_live_ready_entry`; all four `rfc0152_select3_frame_contract` rows; `stdlib_docs_are_current`; full merge-queue gate | Adds the scheduler-local `PullReadyFrameLoop` entry for a payload-free receive frame. With exactly one live slot it consumes one FIFO message directly; otherwise, and on an empty ring, it installs the existing `WaitFrameLoop` fallback. It preserves close/cancellation handling on that ordinary path and does not claim typed payload lanes, continuous draining, or general one-way ready emission. |
| Differential semantic-matrix baseline | `9259f7978` (gate rebaseline `835f269ae`) | `rfc0152_semantic_matrix`: select4 source order, capacity-0 quiescence/capacity-1 wake, String close/cancellation, capacity-2048 record; each runs expected output, interpreter, normal Wasm, and optimized Wasm | Establishes independently runnable semantic rows for subsequent promotions. It is intentionally a partial matrix and contains no allocation, wait-bound, or performance evidence. |

The fixed three-way frame-loop increment is recorded above because its merge
event exists. Empty-frame work remains only an implementation increment until
it has matching matrix and semantic evidence.

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
| scheduler steps | direct ready path only | cooperative suspension retained | Phase 0: partial — deterministic `scheduler_slot_transitions` counts every executed ordinary-scheduler slot retag; complete path attribution remains pending |
| wait registrations / wake claims | zero / zero after warmup | at most one live registration / one claim per wake | Phase 0: partial — deterministic `scheduler_wait_installs` and `scheduler_wake_claims` count ordinary installation/claim events, but live-registration identity and the one-live-registration bound remain pending |
| selected-result materializations | zero for local non-escaping match | zero until an escaping boundary | Phase 0: partial — deterministic nominal `Selected*` materializations are counted, but direct local result lanes remain pending |
| CPU and p50/p95/p99 | report separately | report separately | Phase 0: harness pending |

## Current measured attribution

The first complete local matrix run after the stats Clock fix is
`benchmarks/.build/rfc0152-landed-select3-clock-20260825-075850` at merged
revision `52572918e`. It is an ignored local artifact, therefore it is useful
attribution but not durable acceptance evidence. It recorded the required
twelve counterbalanced pairs and three matching counter payloads per mode for
`chan_throughput`, `chan_scheduler`, and `select_fanin`.

The decisive result is negative: `select_fanin` measured a 1,515,750 ns median
with `WITCHY_OPT=none` and a 1,655,750 ns median with `WITCHY_OPT=all`.
The candidate also recorded 12,295 `rc_alloc` calls and 4,097 wait installs,
versus 8,199 allocations and the same 4,097 wait installs for the control.
The current frame-loop lowering avoids local `Selected` materialization but
still re-enters the ordinary scheduler; it is not a ready-path promotion.

The later local attribution run
`benchmarks/.build/rfc0152-sole-live-select-drain-20260825-083022` used twelve
counterbalanced pairs and matching output hashes. Its candidate reduced the
`select_fanin` median from roughly 1,655,000 ns to 1,327,000 ns, scheduler wait
installs from 4,097 to 2, RC allocations from 12,295 to 8,200, and retained
heap bytes from 582,047 to 270,853. The artifact was produced before the final
queue rebase and remains ignored, so it is local attribution only; it does not
close any acceptance row or replace the required durable post-landing matrix.

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
