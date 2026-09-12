# Development velocity acceptance ledger

This ledger tracks measured improvements to contributor feedback and landing
latency. It extends the implemented workflow in RFC-0115 without changing that
frozen RFC. The dated investigation in
[development-latency-2026-09-12-codex-01a09617.md](development-latency-2026-09-12-codex-01a09617.md)
is the initial evidence snapshot.

The compiler rewrite has priority. Velocity work uses isolated worktrees, owns
tooling and test-selection files explicitly, and moves to a disjoint slice when
an active agent owns the same file or hunk. Compiler implementation files,
active compiler branches, test assertions, fail-closed behavior, gate policy,
budgets, and exclusions are outside this program's scope.

## Evidence contract

Each performance claim records the source revision, command or journal window,
test and feature selection, machine state when known, before and after values,
and uncertainty. Local feedback and merge-queue landing are separate workloads.
A green focused check is not landing evidence, and a queued branch is not a
landed improvement.

Count the union of overlapping intervals. A shared batch is one gate attempt.
Preparation, lock wait, preflight, gate, and landing durations come from the
existing journal fields. Cargo build and nextest execution durations come from
the corresponding logs. Machine sleep and failure-to-resubmission intervals
are separate labels, not costs to add again. Queue waiting does not imply the
agent was idle; transcript activity determines whether other work overlapped.

Existing `scripts/gate-report.sh`, `scripts/perf-health.sh`, Cargo output, queue
journal, gate logs, and agent transcripts are the evidence sources. Extend
these tools rather than create another telemetry store. Raw local measurements
remain untracked unless an existing repository convention requires otherwise.

## Initial baseline

Snapshot command: `./scripts/gate-report.sh --since 24h --json`, generated at
2026-09-12 16:16:47 UTC from master `5b4d1dbe5` and the main queue state.

| Measure | Baseline | Coverage limit |
| --- | ---: | --- |
| Submissions / merged branches / unique green gates | 20 / 11 / 9 | Rolling 24-hour journal window |
| Failed unique attempts | 9 | 6 red, 2 timeout, 1 batch red |
| First-pass clean rate | 63.6% | Landed branches in the window |
| Attempt p50 / p90 | 440s / 2011s | 18 unique terminal attempt logs |
| Gate p50 / p90 | 374s / 1964s | Includes two sleep-driven timeout outliers |
| Prepare p50 / p90 | 43s / 90s | Maximum 385s included 297s asleep |
| Lock-wait p50 / p90 | 0s / 0s | No sampled attempt waited for the gate lock |
| Queue idle p50 / p90 | 15s / 1460s | Waiting may overlap useful agent work |
| Compile p50 / p90 | 85s / 207s | 17 logs with a Cargo test-profile finish |
| Test execution p50 / p90 | 262.260s / 326.495s | 17 logs with nextest summaries |
| Failed gate time | 93.55 minutes | 67.77 minutes were two sleep-driven timeouts |
| Coordinator observed uptime | 28.1% | Heartbeat coverage, not host uptime |
| Explicit test code / support code | 77,104 / 8,557 lines | `scripts/test-footprint.sh`; not a deletion target by itself |

The latest local build baseline visible to `scripts/perf-health.sh` is older:
revision `fe415a90`, workspace build 13.393s and test compilation 29.034s. It is
retained as historical context and must not be compared to a new result unless
the command, source, feature set, target warmth, and environment match.

## Acceptance criteria

| Criterion | Required proof | State |
| --- | --- | --- |
| 1. Lightweight latency report | Existing report deduplicates batch attempts and exposes prepare, scheduling/lock, preflight, gate, landing, outcomes, and failure rework without inventing unavailable edit time | IN PROGRESS |
| 2. Minimal edit-loop selection | One executable contributor command selects named low-cost checks by changed surface and is documented separately from the unchanged landing set | IN PROGRESS |
| 3. Early recurring-failure probes | Root driver budget, WIR prelude/ABI census, and ownership/admission composition failures are selected before submission on their relevant paths | IN PROGRESS |
| 4. Unchanged-input repetition | Repeated validation is keyed to source tree, feature selection, and environment; reuse never crosses an invalidating input change | PENDING |
| 5. Cargo build consolidation | Exact old/new selected-test sets and production feature graphs are equivalent before matched timing; production-only activation checks remain separate | PENDING |
| 6. Dominated coverage | Each rerouted or reduced-frequency test names the retained stronger oracle and measured blocking cost; security and independent semantics remain represented | AUDIT IN PROGRESS |
| 7. Measured feedback improvement | At least three matched warm edit-loop samples show a material median reduction with identical protected checks | PENDING |
| 8. Measured landing improvement | Current journal window shows improved first-pass-clean rate or reduced failed gate minutes without a weaker landing set | PENDING |
| 9. Operational safety | No compiler/refactor overlap, interrupted foreign process, shared cache use, direct master merge, or lost concurrent work | CONTINUOUS |

## Protected assertions

Frequency and selection may change only when the following authority remains:

- ownership, initialized-prefix, bounds, allocation-identity, and cleanup
  refusals;
- authenticated admission, import authority, production feature activation,
  capability confinement, and fail-closed behavior;
- independent expected semantic output, with interpreter and admitted Wasm
  agreement where the stabilization contract requires both;
- driver/link footprint constraints and generated-catalog freshness; and
- the coordinator's current landing policy and CI or release obligations.

Backend agreement alone cannot replace an independent expected result.
Structural Wasm validation cannot replace source provenance or admission.

## Active dependency graph

1. Add cheap path-routed probes and their router tests. This is independent of
   compiler implementation and improves first-pass cleanliness immediately.
2. Extend the existing queue report with missing phase and rework views while
   deduplicating shared batches. This makes later claims reproducible.
3. Audit current blocking tests for dominated coverage. An audit finding is not
   authority to delete a test.
4. From the first three tracks, define one executable minimal edit-loop command
   and capture three matched warm samples.
5. Attempt build consolidation only after selected-test and production-feature
   equivalence passes. Reject it if the matched samples do not improve.
6. Recompute the report after landed changes. Completion requires current
   first-pass and latency evidence, not projected savings.

## Change ledger

| Date | Change | Local evidence | Landing evidence | Measured result |
| --- | --- | --- | --- | --- |
| 2026-09-12 | Initial latency reconstruction | Five recent slices separated build, execution, queue, sleep, rework, and concurrency | `5b4d1dbe5`, docs-only queue attempt `mqa-2419cf9d7ad52b73dfaa17207ff0796f5c49e6cf` | Baseline only; no executable speedup claimed |

Update this table only after terminal queue evidence. Record proposals and
failed experiments in the criterion state or accompanying change, without
presenting them as measured improvements.
