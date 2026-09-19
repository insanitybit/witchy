---
rfc: 0161
title: "RFC-0161 acceptance ledger"
status: deferred
created: 2026-08-26
---

# RFC-0161 acceptance ledger

> 2026-09-16: The competing source surface is deferred by
> [RFC-0167 section 7](0167-witchy-language-contract.md#7-resources-effects-and-suspension).
> No pending row is closed or canceled. Must-start Task, Child, explicit owner
> close, and selectable-operation rows require an adopted RFC-0167 amendment
> before implementation can count toward the selected language. Representation
> work preserving that language may reuse these assets with its own evidence.

This ledger records implementation and measurement evidence for RFC-0161. A
row is complete only when its source, tests, durable artifacts, and merged
commit are present. A branch, prototype, local artifact, or benchmark result by
itself is not acceptance evidence.

## Frozen starting point

Starting master: `90725c9537ac5128b46467b91bbeb4c94cff0a3e`.

The detailed concurrency implementation and worktree audit was performed at
its immediate parent `9663d238b324d37bf7fe6429e513b891531e8f55`.
`90725c953` adds the unrelated RFC-0160 proposal and does not change the audited
runtime files.

RFC-0129 is the accepted public concurrency contract at this revision. Its
seven existing acceptance rows are proven for the current replayable Task,
untyped child Handle, quiescence-close scheduler, deterministic CPS execution,
and installed bounded workflow. RFC-0161 changes several of those semantics,
so their evidence is a regression oracle, not proof of the new contract.

RFC-0152 is proposed and all ten of its final rows are pending. Its latest
durable conclusion is negative: landed frame bridges and sole-live draining
reduce work but remain scheduler-bound. No merged direct select executor exists
at this starting point.

## Reusable merged assets

| Asset | Current evidence | RFC-0161 use |
|---|---|---|
| Typed suspension facts | endpoint direction, payload layout, ownership envelope, capacity, and result-lane facts | Inputs to `AsyncProgram` and typed queue shapes. |
| Async region formation | independently eligible unguarded paths and named rejection reasons | State formation input, not runtime selection. |
| `ReadyStateWir` | exact WIR prefix and terminal task-building boundary | Extend after final closure layout; do not rewalk source. |
| Typed Int ring bridge | one-way exact Int storage plus erased materialization | Mechanism evidence only; replace parallel queue state with one typed queue ABI. |
| Select frame bridges | fixed arity, source order, local payload-lane evidence | Semantic tests and lowering facts; not the final execution ABI. |
| Semantic fixtures | interpreter, ordinary Wasm, optimized Wasm schedule and payload cases | Expand for the new surface and operation-level turn contract. |
| Counters and matrix harness | scheduler transitions, waits, wake claims, allocations, paired samples | Reuse after counter definitions are updated for the uniform runtime. |

## Branch and ownership snapshot

No RFC-0152 branch was queued when this ledger was created. The primary
unmerged convergence branch was `integration/rfc0152-typed-select-frame`.
Direct async-program, direct select, typed runtime dispatch, typed receive,
evidence, actor, and pipeline branches overlapped it.

The dirty `impl/rfc0152-select34-ready-frame` and
`recovery/rfc0152-send-frame-loop` worktrees had no current handoff note. Their
exact diffs must be preserved before cleanup. Absence of a live owner is not
permission to discard them.

The status audit also found `perf/rfc0129-spawn-resumption` at old commit
`5c040cc0` with one untracked, opt-in 100-versus-10,000 spawn/join scaling test.
It contains no implementation change and is not current evidence. Preserve it,
then rebase and validate it as a possible RFC-0161 scaling fixture.

The RFC-0130 no-replay pickup and fallback prototype own generator behavior.
They are related because they replace replay with an owned resumable frame, but
they do not change the current replayable `Task(T)` contract or implement this
RFC.

## Dependency ledger

| Contract | Depends on | State | Evidence |
|---|---|---|---|
| Public task, child, channel, close, cancellation, select, and turn semantics | RFC review | proposed | RFC-0161 |
| Scope brand, affine Task/container rules, and conditional ownership checking | accepted surface semantics | pending | - |
| Canonical `AsyncProgram` schema, encoder, frame lanes, ownership transitions, and backend binding | accepted surface semantics | partial | Source-control preservation and final-layout schema are merged (`071dcfe03`, `003834abd`, `6a4b1a776`, `d9eef9cf5`, `b646d0d86` through `416befef8`). The conservative producer now emits one canonical program for every independently lowerable callable with explicit per-callable closure fingerprints (`2f378d789`); it deliberately does not yet resolve source operands or execute a normal source program. |
| Typed queue shape and materialization ABI | RFC-0055 and RFC-0127 constraints | pending | - |
| `PendingMove(T)` and result-lane ABI | affine checker and queue ownership | pending | - |
| Intrusive wait record and rendezvous probe/register/match/commit/cancel ABI | accepted select, close, cancellation, and host event rules | pending | Queue admission is now exposed through the deterministic scheduler (`fd7260205`), including send, receive, select, close, and exact-generation cancellation; the full intrusive-wait and host-unification matrix remains open. |
| Scheduler transition and canonical admission-trace format | accepted turn semantics | partial | The scheduler centralizes queue admission and cancellation wake-up into FIFO next-round admission (`fd7260205`), and records canonical queue and host admission events as `(sequence, operation, generation, result)`, including error outcomes (`feaa30b49`, `0815ea6be`); cross-backend and complete external-event trace evidence remains open. |
| Interpreter kernel | three frozen ABIs | pending | - |
| Compiled Wasm kernel | three frozen ABIs and final closure layouts | pending | - |
| `HostOperation(T)` finish/cancel/drain bridge | wait ABI and capability contract | pending | - |
| Type-resolved source/API census and one-cut migration | accepted surface semantics | pending | Type-resolved census baseline is durable in `spec/rfc0161-census-2026-08-28.md`; strict completion predicate and `--strict` release gate are merged (`e4487131b`). The baseline reports 550 checked units and 0 compiler rejections, but legacy findings remain, so the one-cut migration is not complete. |
| Old CPS and scalar-executor removal | semantic, performance, and migration rows | pending | - |

## Acceptance status

Rows 1 through 10 are imported from RFC-0152 without weakening.

| Criterion | Required evidence | Status | Merged commit / artifact |
|---|---|---|---|
| 1. semantic parity | independent outputs plus exact send/receive wake, spawn, completion, close, cancellation, default, and scripted-host transition traces on all three modes | pending | - |
| 2. ready allocation-free | Int, String, packed record; representative capacities; WAT and exact counters | pending | - |
| 3. bounded wait state | repeated waits plus many producers/receivers, static/dynamic select, duplicate endpoints, every wait-state transition, stale/duplicate notifications, intrusive-link counters, and flat retained heap | pending | - |
| 4. direct result lanes | WAT for local heterogeneous and homogeneous select; no nominal result allocation | pending | - |
| 5. payload coverage | Int, String, and packed record promoted or a typed named rejection | pending | - |
| 6. `select_fanin` | matched deterministic Go control at or below 1.00x with correctness hashes | pending | - |
| 7. generalization | channel throughput, scheduler control, actor, and pipeline; two improvements of at least 20 percent; no protected regression above 5 percent | pending | - |
| 8. tail latency | blocking and cancellation p99 no worse than 5 percent | pending | - |
| 9. build footprint | concurrency corpus compile time and Wasm size each below 8 percent growth | pending | - |
| 10. landing evidence | queue gate, raw paired samples, WAT, counters, hashes, and build identity | pending | - |
| 11. affine task and scope ownership | multiple start; branded escape through aggregate/closure/generic/return/await/Dynamic; collections/combinators/loops/`?`; winning and losing child-completion arms | pending | - |
| 12. explicit channel lifecycle | all capacity modes, affine owner, close/drain/deadlock, waiter order, rendezvous matrix, duplicate endpoints, and `PendingMove` restoration on every non-commit path | pending | Scheduler-owned queue admission covers send, receive, select, and close results while preserving typed queue commit delivery (`fd7260205`); the source producer now accepts compiler-supplied queue shapes and lowers terminal receives with direct result lanes (`f59a098de`), while the runtime proves pending-move discharge for committed sends (`d151efad0`). Landed select lowering now preserves fixed and dynamic capacities, reuses one typed queue shape for duplicate endpoint arms, and rejects conflicting duplicate shapes (`9648c1a70`). The complete lifecycle and ownership matrix is still required. |
| 13. cooperative cancellation safety | request, wake, close, child selection, scope `return`/`?`, repeat request, non-cooperation, exact traces, and no abandoned must lane | pending | Exact-generation cancellation is now admitted by the scheduler only after queue waits are unlinked, with a focused cleanup-round regression test (`fd7260205`); compiler-produced terminal send, receive, and select paths now include cancellation cleanup coverage (`9648c1a70`); scope/child/host paths and the full trace matrix remain. |
| 14. one runtime architecture | every static/dynamic path shares slots, turns, waits, cancellation, and queues; census rejects old CPS, scalar recognizers, specialized entries, and a second scheduler | pending | A deterministic typed scheduler owns FIFO execution IDs, operation-level rounds, explicit selected-transition execution, queue send/receive/select/close admission, exact-generation cancellation wake-up, canonical queue admission tracing (`feaa30b49`, `0d0ac8a50`), and canonical `AsyncProgram` installation with mandatory backend binding (`1d55110eb`, `fd7260205`, `38194b44d`). Legacy CPS/scalar paths and migration census findings remain, so the one-runtime requirement is not yet proven. |
| 15. async program generality | one kernel or source rejection; canonical schema/fingerprint, closure-layout drift, lane masks, terminal edges, static and dynamic async calls, dynamic select, actor, and pipeline | partial | Lossless pre-CPS source control topology and final typed-state bindings are merged (`071dcfe03`, `003834abd`, `6a4b1a776`, `d9eef9cf5`, `b646d0d86` through `416befef8`); canonical fully typed-program validation is merged; checked source has an explicit lowering handoff (`ea6e805c`); the producer emits a canonical multi-callable program for linear empty returns/yields with deterministic shape IDs and per-callable fingerprints (`c3237f251`, `2f378d789`); boolean selectors now lower to typed terminal edges (`e826f0c86`), including explicit bare returns on both branches (`84d450f1c`); terminal operations and value-return rejection are enforced (`9bcaf879b`); linear executions preserve typed entry frame lanes and ownership conventions (`ffc36c5b3`); typed `Nil` yields lower through the same linear execution shape without retaining nested call artifacts (`36743e4ce`); continuation states bind to their explicit final callable (`62086dd66`); typed operation arguments are retained before CPS and compiler-supplied queue shapes lower terminal receives to real queue sites (`8e80a415d`, `f59a098de`); a compiler-produced canonical program is installed and driven through the interpreter scheduler and queue admission (`a2ab535fd`); landed continuation send/receive/select lowering now preserves source arm order, typed queue shape identity, duplicate endpoint semantics, and terminal cancellation paths (`9648c1a70`). General nonterminal operation/operand production and shared interpreter/Wasm execution remain outstanding. |
| 16. host readiness | timer and capability I/O prove scripted admission, finish, losing drain, acknowledged cancel, late/stale completion, bounded state, and no obligation leak | pending | The scheduler now records typed host submit and matching completion admissions in the shared runtime trace (`0815ea6be`); timer/capability integration, losing-drain, cancellation, stale-completion, and obligation-leak evidence remain open. |
| 17. source migration | type-resolved repository/public-fixture census, external-source diagnostics, one release cut, no compatibility path or stale artifact | pending | The repository/public-fixture census and strict no-findings gate are merged (`28878b8b7`, `e4487131b`); its recorded legacy findings are the remaining migration set. A release cut with migrated sources and no stale artifacts is still required. |
| 18. installed workflow | clean installed binary runs structured bounded workflow and reports constructed deadlock | pending | - |

## Measurement protocol

Every performance comparison uses one source revision and records compiler and
runtime build identity. Correctness output is checked independently before
timing. Ready and blocking cases are reported separately.

Timing promotion requires two warmups and twelve paired interleaved samples.
Report median, p95, p99 where meaningful, raw samples, allocations and bytes,
retained heap, scheduler turns, queue operations, wait installs, wake claims,
result materializations, and CPU utilization. Three deterministic counter runs
must agree exactly.

`bench_ns` is kernel-only. Whole-workload wall time is recorded separately.
Scaling points must distinguish fixed setup, per-task, per-channel, per-message,
and per-wait costs. A benchmark-specific branch, source name, constant, or WAT
shape is a rejection, not evidence.

## Evidence promotion rules

- `pending` means the criterion is not met.
- `partial` requires a merged reusable slice and durable evidence, but does not
  close the criterion.
- `proven` requires all matrix cells, durable artifacts, and merged commits.
- Wasm-first work carries an explicit RFC-0135 interpreter debt until parity.
- A red merge gate is investigated against the branch and current master; a
  historical flaky test is not automatically blamed or ignored.
- The RFC remains proposed until the public semantic decisions are accepted.
  Implementation progress cannot silently decide the surface.
