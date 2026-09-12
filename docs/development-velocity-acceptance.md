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
| 1. Lightweight latency report | Existing report deduplicates batch attempts and exposes prepare, scheduling/lock, preflight, gate, landing, outcomes, and failure rework without inventing unavailable edit time | LANDED (`00b909301`) |
| 2. Minimal edit-loop selection | One executable contributor command selects named low-cost checks by changed surface and is documented separately from the unchanged landing set | LANDED (`00b909301`) |
| 3. Early recurring-failure probes | Root driver budget, WIR prelude/ABI census, and ownership/admission composition failures are selected before submission on their relevant paths | LANDED (`eb190d687`) |
| 4. Unchanged-input repetition | Repeated validation is keyed to source tree, feature selection, and environment; reuse never crosses an invalidating input change | LANDED, PROCESS LOCAL (`07c3611c5`) |
| 5. Cargo build consolidation | Exact old/new selected-test sets and production feature graphs are equivalent before matched timing; production-only activation checks remain separate | LANDED, BOUNDED (`4bec6d70c`) |
| 6. Dominated coverage | Each rerouted or reduced-frequency test names the retained stronger oracle and measured blocking cost; security and independent semantics remain represented | FIRST SLICE LANDED (`07c3611c5`) |
| 7. Measured feedback improvement | At least three matched warm edit-loop samples show a material median reduction with identical protected checks | LANDED: WIR SEGMENT MEDIAN REDUCED 47.0% (`4bec6d70c`) |
| 8. Measured landing improvement | Current journal window shows improved first-pass-clean rate or reduced blocking gate time without a weaker landing set | LANDED: QUEUE SHARD 135/137/137s VS 153/156/159s (`476169601`) |
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

## Rejected experiments

Sharing one target between sequential `cargo check` and clippy was rejected.
Three matched `witchy-types` pairs used identical package, target, lint, and
feature selections from identical target clones. Reuse reduced reported Cargo
compiler actions from 62 to 35, but median completion increased from 24.818s
parallel to 30.968s sequential: 6.150s slower (24.8%). This package-scoped
result has limited authority over whole-workspace CPU effects, but it provides
no wall-latency basis for changing the current parallel gate.

## Change ledger

| Date | Change | Local evidence | Landing evidence | Measured result |
| --- | --- | --- | --- | --- |
| 2026-09-12 | Initial latency reconstruction | Five recent slices separated build, execution, queue, sleep, rework, and concurrency | `5b4d1dbe5`, docs-only queue attempt `mqa-2419cf9d7ad52b73dfaa17207ff0796f5c49e6cf` | Baseline only; no executable speedup claimed |
| 2026-09-12 | Path-routed recurring-failure probes | Router suite 45/45 in 2.99s; relevant paths select root-driver budget, WIR prelude/ABI catalog, prefix ownership, and admission probes before broad checks | `eb190d687`, attempt `mqa-5ce1c4bbc87424e7189a771191b63c5f7ff1130f` | Historical failures reached the full gate after 112s, 121s, 151s, 233s, and 554s; their decisive assertions executed in 0.026-0.387s. Savings are bounded by build warmth and concurrent load. |
| 2026-09-12 | RFC-0165 edit/corpus/landing split | The edit helper retains named budget, ABI, ownership, lowering-publication, and admission assertions; `corpus` retains every oracle partition; `landing` retains the previous six-command set | `07c3611c5`, attempt `mqa-164766af7968eb90a69282d84d610235d480a491` | Moving the full oracle corpus to family checkpoints avoids an estimated 4-6 minutes per small edit. Confidence is high for retained coverage and medium for wall savings because recorded tests overlap. |
| 2026-09-12 | Process-local preflight reuse | Three deterministic fixtures prove workspace partitioning, scoped-selector non-subtraction, and immediate stop on a red preflight | `07c3611c5`, attempt `mqa-cbace692328412f747bf5420d8bd6665c1705335` | Nine observed gates repeated 10 already-green tests with 169.137s summed per-test duration. Estimated affected-gate wall saving is 12-30s typically, 1-55s observed bounds; medium confidence because nextest overlaps tests. |
| 2026-09-12 | Minimal path-routed edit command | `agent-check edit` on `wir_prelude.rs` passed all three named probes. One-time native setup was 152.98s; unchanged-source warm runs were 2.79s, 2.89s, and 2.85s, median 2.85s. | `00b909301`, attempt `mqa-6fd0a28e0a522aa636ff10246f6d5d556498f88d` | Current warm feedback is measured. The previous broad route is estimated at 4-6 minutes from recorded oracle costs because rerunning it would be a heavyweight suite; this is not a matched before/after claim. Mixed or uncovered paths fail safe to the broader focused set. |
| 2026-09-12 | Development-latency report | Exact fixture covers batch deduplication, conflict exclusion, change-bounded rework, and a resubmission racing an older live attempt | `00b909301`, attempt `mqa-f51696c563780a019ef2a7b8215b428888c999bf` | Current 24-hour view at 23:46 UTC: 43 physical attempts and 41 landed samples; submit-to-land p50/p90 466s/1084s; scheduling residual 35s/458s; 10/41 landed changes had failure rework totaling 4,139 deduplicated attempt seconds. First-edit time remains unavailable, and scheduling can include sleep or coordinator absence. Rolling-window movement is descriptive and is not attributed to one change. |
| 2026-09-12 | Defer post-merge worktree reclamation while ready work exists | Focused fixture models two incompatible ready gates and proves both merge before the first sweep, then proves both worktrees are reclaimed after drain | `8cda23310`, attempt `mqa-74b14f10a20079721b783d54dd88a124bb1c624a` | With 450-458 linked worktrees and a 13,642-line journal, the old coordinator produced ready-queue gaps of 507s, 250s, and 258s after observed landings. After `e10f16af8`, the old daemon continued post-merge branch reclamation for more than four minutes while a latency stack was ready. It was restarted only after the sweep had finished and no child or gate existed. New PID `99116` immediately journaled `startup sweep deferred while queue has pending changes`; after `1d243d339` landed, it re-executed and began the next ready gate 26s after the second member's merge event. The 26s includes reload and candidate preparation, so it is not pure scheduling overhead. |
| 2026-09-12 | Consolidate identical WIR edit probes | Nextest listings prove the old 2+2 selections and new four-test selection have identical sorted-set SHA-256 `21cc6ce7ec1f25c8a5267ff2fbc21f311ce55026d125c2e060676476d96b4619`; package, `native` feature, and lib target are unchanged; normalized feature graphs match | `4bec6d70c`, attempt `mqa-dce6643ed87bcee2bd16af82e613bc366a5cfded`; 4,187/4,187 landing tests passed in 557s | Three warm pairs: old median 2.733s, new median 1.449s, saving 1.284s (47.0%) for the WIR probe segment. All samples passed the same four tests. Scope is only `scripts/rfc0165-dev/shards.sh` and its README. |
| 2026-09-12 | Remove synthetic waits from queue timing fixture | The named fixture retained its one nonzero foreground/background timing relation, fail-fast red, process-reaping, feature, selector, and stage-order assertions. Three warm focused runs passed in 19.789s, 20.928s, and 20.966s. | `476169601`, attempt `mqa-e62c475e5ee58adb7729a6595ec7aa967712ed06`; three subsequent queue shards passed 67/67, 68/68, and 69/69 | The fixture fell from a stable recent 49.283-49.554s range to 17.469-18.267s in landing logs. Queue stages fell from 153/156/159s (median 156s) to 135/137/137s (median 137s), saving 19s (12.2%) while adding two tests. Confidence is high for the bounded shard and medium for total gates, whose other stages vary. Scope is one test fixture file; production polling is unchanged. |
| 2026-09-12 | Reload committed coordinator updates between attempts | The lifecycle fixture lands a script update followed by a second branch, then proves exactly one re-exec, unchanged PID, retained singleton ownership, and no gate or metadata lock at reload. Existing daemon and concurrent-start neighbors also passed. | `1475a0d2e`, attempt `mqa-45c05b84fb701f67b9f36252c971b5ad9f8272b3`; queue shard 68/68 in 137s | Activated at an idle boundary as PID `99116`. After `1d243d339` landed, the journal recorded `coordinator_reexec` from script hash `1f1e3ca7` to `1a582fd8`; PID `99116` stayed constant, the singleton was adopted, the pending sweep was deferred, and the next candidate gated successfully. Dirty or invalid scripts remain dormant. |
| 2026-09-12 | Parse sweep history once and intersect local refs | A 2,464-row isolated history fixture took 81.01s before and 0.10s, 0.09s, and 0.09s after. Focused tests preserve live queued worktrees, delete only journal-merged branches, retain unrelated branches, and reclaim after the queue entry leaves. | `6b6d4213a`, attempt `mqa-4165c4dfcaa91f00bf73bd92c4124d08f56637a6`; queue shard 69/69 in 135s | Median saving is 80.92s (99.9%) for the historical branch-probe phase. This does not include necessary worktree status, patch-equivalence, or removal costs and therefore does not claim the full 231-507s field gaps disappear. The live queue check remains per worktree to avoid a concurrent-submit race. PID `99116` now has this implementation loaded; a future drained sweep is still needed for matched field timing. |
| 2026-09-12 | Record exact-tip edit-loop receipts | A clean post-commit `agent-check edit` records revision, tree, selected shards, actor, session, result, and time in the existing queue state directory. Dirty trees and mismatched revisions do not attest a submission. | `5a67c0699`, exact receipt on attempt `mqa-e609ef61f780363915973636014721ffeb459275`; authoritative contributor guidance `33dc9c89a`; actionable warning `1d243d339` | In the first 26 submission attempts after receipt support landed, 19/26 (73.1%) carried an exact matching receipt. After contributor guidance landed, 14/17 (82.4%) did. The later sample is biased toward repeated latency-stack resubmissions and measures use, not agent idle time or correctness coverage. |
| 2026-09-12 | Isolate queue fixtures and add a bounded edit shard | The standalone queue crate retains 69 assertions without compiling the root product test binary. The edit shard runs 12 representative coordinator assertions covering admission, dependency order, timing, failure, cleanup, receipts, and sweep behavior. Warm local runs completed in 41-42s. | `79709c492` moved the fixtures; `d686fc490`, attempt `mqa-beaa0a088081286f680f0a2f8fcb4a9399f3b464`, landed through a queue-only 69/69 gate in 141s | The preceding one-time manifest-moving gate took 695s, including a 536s product stage plus a 143s queue stage. Once classification became available, the bounded queue-only landing skipped more than 4,000 product tests. This is live selection evidence, not a matched claim that every queue change saves 554s. Gate policy and the complete queue landing shard are unchanged. |
| 2026-09-12 | Isolate path-router regressions from the product binary | All 48 existing router assertions, including an exact structural-regression probe, compile in the standalone queue crate. The old root binary took 143s to compile and 2.133s to execute in the observed cold worktree; the standalone router took 0.47s to compile and 2.185s to execute. | `2551faed4`, attempt `mqa-8ad95673174fbd55a281d4780eefdfabba5cea2f`; 4,193/4,193 product tests and 117/117 queue tests passed | The complete queue stage with 69 coordinator plus 48 router tests took 136.280s, versus 135.937s for the preceding 69-test shard, because the router tests ran within the existing parallel envelope. The one-time move still required a 527s product stage for its product-harness path update. Exact cold-build savings have medium confidence because the source roots differed; retained assertion count and landing coverage have high confidence. |
| 2026-09-12 | Shorten the fail-fast fixture and repair queue-only product selection | The unchanged-source timing fixture fell from 17.470s to 8.275s while retaining foreground/background timing, selector, feature, abort, and process-reaping assertions. The exact queue-edit shard passed 12/12 in 26.164s. | `40151ae69`, attempt `mqa-c162af890c2d6c715a82a65806769449d9fb1249`; the shared gate compiled for 106s, then passed 687 focused product tests in 9.172s and 117 queue/router tests in 122.780s | Direct fixture saving is 9.195s (52.6%). The branch also stopped an exact queue-only selection from asking root Cargo to resolve the standalone `merge_queue` target. Before repair, five candidate attempts failed in 10s gates and consumed 316s of preparation, preflight, and gate work, excluding a separate 307s lock wait. Production failure polling changes from 5s to 1s; risk is more frequent lightweight log scans in exchange for faster red detection. |
| 2026-09-12 | Attribute batch preflights to the culprit and parse padded nextest counters | A padded `( 1/3)` failure now identifies the unique `foo_widget` branch, records the replay only there, and leaves both innocent changes without a preflight. The exact edit shard now includes this probe and passed 13/13 in 40.705s. | `acad081da`, attempt `mqa-eabc5383dc081e13d41e1abee64782f0a0270997`; 117/117 queue/router tests passed in a 151s gate | A live innocent candidate previously paid a 175s build for a preflight assigned from another branch. Estimated saving is up to that cost per comparable misattribution; frequency is unknown. Two full queue gates, 151s and 161s, found the padded-counter parser defect before the focused set included it. The added 14.306s probe now catches that exact failure before submission. Ambiguous batches still skip attribution rather than attaching a replay to an arbitrary head. |
| 2026-09-12 | Bound canonical compiler edit checks | A current cross-stage RFC-0165 diff selected 20 commands before and three feature-distinct sentinel commands after. A live one-file `canonical_aggregates.rs` diff now selects the same bounded route. The set retains ABI and authority catalog checks, prefix ownership, move/clone bounds, admission refusal and boundary guards, lower semantic list/string checks, and aggregate rendering. Exact local validation passed 13 queue-edit tests in 42.499s and all 50 router tests in 2.580s. | `5adae588a`, attempt `mqa-b3255af1642a5a59fefd11078551bfa0a0d716f4`; 119/119 queue/router tests passed in 122.956s, 131s gate wall | Command-count reduction is measured; developer wall saving is not yet measured and remains uncertain because the old 20-command route was not rerun. Mixed noncanonical paths retain the broader route, and the serialized landing selection is unchanged. Scope is only `scripts/test-for-paths.sh` and its standalone router tests. |

Update this table only after terminal queue evidence. Record proposals and
failed experiments in the criterion state or accompanying change, without
presenting them as measured improvements.
