# Development latency: September 11-12, 2026

This contributor note records three opportunities from existing agent transcripts,
Cargo logs, and merge-queue events. It is a documentation-only follow-up, not a
change to compiler behavior, feature activation, test selection, or gate policy.
The original sections preserve the evidence and recommendations at that time;
the outcome update below records later landed work. Follow the current
[agent playbook](agile-agent-playbook.md) and
[queue operator guide](../scripts/MERGE-QUEUE.md) when applying them.

The measurement snapshot ends at `8f907d084861d00bdeec9bed76e2c6f514d68e24`,
landed at 2026-09-12 14:36:20 UTC. Later compiler work is outside this sample.
All durations are historical observations, not promises about the current tree.
The initial investigation ran no builds or suites and changed no processes or
power settings. Its first follow-up added only this file; the outcome update is
based on the later landed changes and terminal evidence named below.

## Outcome update at 2026-09-12 19:18 UTC

The three highest-impact implementation areas are now bounded as follows.

1. **Remove merge-queue sweep stalls without weakening reclamation guards.**
   Ready work waited 507s, 250s, and 258s behind observed post-merge sweeps; a
   periodic sweep delayed another ready submission by 231s. Commit `8cda23310`
   defers post-merge reclamation while ready work exists. Commit `1475a0d2e`
   lets a persistent coordinator re-exec a committed, syntax-valid script only
   between attempts while retaining its PID and singleton lock. Commit
   `6b6d4213a` parses the append-only merged history once and intersects current
   local refs instead of launching one Git probe per historical branch. In a
   2,464-row matched fixture that branch-history phase fell from 81.01s to a
   0.09s median, saving 80.92s (99.9%). Worktree cleanliness, containment,
   patch equivalence, journal authority, and the live per-worktree queue check
   remain. Risk is coordinator lifecycle and reclamation authority; focused
   lifecycle/sweep fixtures and the full queue shard passed 69/69. The
   current eight-day-old daemon loaded code before these changes, so field
   activation awaits its natural restart. Expected busy-queue savings have high
   confidence; current-daemon activation timing is unknown.

2. **Run decisive ownership, admission, ABI, and driver-budget probes before
   submission.** Commit `eb190d687` routes the existing 0.026-0.387s assertions
   on their relevant paths, and `07c3611c5` separates the small edit selection
   from the full oracle corpus and unchanged landing set. Historical failures
   reached the full gate after 112s, 121s, 151s, 233s, and 554s. A similar
   affected episode is estimated to save 6-10 minutes when the focused binary
   is warm. The estimate has medium confidence because build time and concurrent
   load vary. Exact scope is the RFC-0165 developer shard/router and tests; all
   fail-closed ownership, bounds, admission, catalog, semantic, and backend
   assertions remain in either the edit checkpoint or landing set.

3. **Consolidate identical test invocations and remove fixture-only waiting.**
   Commit `4bec6d70c` combines two identical WIR package/feature/lib invocations
   after proving exact four-test selection and feature-graph equivalence. Three
   warm pairs fell from a 2.733s median to 1.449s, saving 1.284s (47.0%) for that
   edit-loop segment. Commit `476169601` removes synthetic sleeps while keeping
   the queue timing relation and failure assertions. Three post-change queue
   shards passed 67/67, 68/68, and 69/69 in 137s, 137s, and 135s versus the
   preceding 153s, 156s, and 159s, a 19s median saving (12.2%). Confidence is
   high for these bounded segments and medium for whole-gate savings. Exact
   scope is the WIR developer helper/README and one queue fixture; production
   polling, gate selection, policy, budgets, and exclusions are unchanged.

The current 24-hour report deduplicates 37 terminal member events into 29
physical attempts. Across 31 landed samples, submit-to-land p50/p90 is
562s/1776s and scheduling residual p50/p90 is 79s/834s. Five landed changes had
failure-driven rework totaling 5,684 deduplicated attempt seconds. Heartbeats
cover only 24.7% of the window, so pre-dequeue idle still cannot distinguish
machine sleep from coordinator absence. First-edit timestamps remain available
only in the reconstructed transcript slices below; no telemetry framework was
added.

**How the time was counted.** First edit means the earliest located source
mutation in the parent or worker transcript, including the service prototype.
Earlier missing edits would make the elapsed durations lower bounds. Queue
scheduling is submission-to-attempt-start and any automatic retry scheduling.
Attempt start is the terminal timestamp minus `attempt_elapsed_s`. A terminal
failure followed by a later resubmission is recovery time, not queue waiting.

Cargo's `Finished` duration measures its build phase, including lock waits;
nextest's `Summary` measures execution wall time. Neither is CPU time. Preparation
includes rebase and setup and cannot all be assigned to compilation. Background
check, clippy, and Wasm work overlaps the test stage. Count a shared batch once
and use the union of concurrent intervals. Sleep is an attribute of an interval,
not an additional duration. DarkWake counts as awake here, so awake time is only
an upper bound on useful development time.

| Slice | First edit / first submit / landing, UTC | Edit to landing | Observed sleep | Queue scheduling |
| --- | --- | ---: | ---: | ---: |
| Isolated service | Sep 11 19:38:11 / 20:51:02 / 21:23:47 | 1h45m36s | 19m39s | 27s |
| Internal admitted launch | Sep 11 21:10:18 / 21:23:01 / 22:02:30 | 52m12s | 0 | 24m45s |
| Remaining admitted launch | Sep 11 21:25:24 / 21:58:10 / 22:29:27 | 1h04m03s | 0 | 24m20s |
| Owned prefixes/writeback | Sep 11 21:16:18 / 22:23:42 / Sep 12 14:36:20 | 17h20m02s | 14h25m29s | 10m15s |
| Canonical WAT | Sep 11 22:03:02 / 22:36:43 / Sep 12 14:25:10 | 16h22m08s | 14h25m29s | 1m40s |

Do not sum these rows. The launch worker began the remaining callers while its
first branch waited, and submitted them before that first branch landed. The
parent worked on prefix accounting in parallel. These waits delayed landing;
they did not establish idle agent time. Service and generated-append work shared
one 403-second gate. Service submission-to-landing reconciles as 27s scheduling,
22m28s coordinator attempts, and 9m50s failure-to-resubmission intervals. Its first
385-second preparation phase included 297 seconds asleep.

**1. Use a deliberately bounded awake landing window.** Two WAT attempts took
2102s and 1964s of gate wall time. Power records account for 1975s and 1874s of
sleep within them: 64m09s asleep out of 67m46s gate occupancy. Only 3m37s of that
occupancy was awake. Both crossed the then-configured 1800-second workspace-stage
watchdog. A later attempt passed in 384s.

The rebased commits `eceb3d2df`, `820041be9`, and `f5ac4e702` all have Git tree
`3943d4167eb131c1d226081d751f7e9955ffff69`. These are confirmed unchanged-source
retries. They do not justify removing a test or increasing its timeout.

For an operator who deliberately wants an AC-powered machine to remain awake
during a finite landing window, macOS provides this optional invocation:

```sh
caffeinate -i -t 3600
```

This prevents idle system sleep for at most one hour. It is an operator action,
not a prerequisite or a command to run automatically when reading this note.
Do not attach an unlimited assertion to the persistent coordinator. It cannot
guarantee protection from lid closure, explicit sleep, or thermal sleep. The
second failed attempt included thermal-emergency sleep.

Estimated benefit is roughly an hour of elapsed timeout/retry recovery in a
similar idle-sleep episode, not an hour of CPU work or the entire overnight
turnaround. Sleep attribution has high confidence; the counterfactual saving
has medium confidence. Risks are extra energy and heat. Exact present scope is
this operator recipe; no queue code, watchdog, or power setting changes are
needed for the documentation follow-up.

**2. Select existing small failure probes earlier.** The service's two failed
gates consumed 233s and 151s. Their combined coordinator attempts took 861s,
including 297s asleep. The failures themselves were much smaller:

| Trigger | Existing probe | Observed failing execution |
| --- | --- | ---: |
| Add a root integration-test driver | `architecture::root_integration_test_binaries_stay_within_budget` | 0.238s |
| Add or change prelude imports | `wir_prelude::tests::every_prelude_import_has_abi_metadata` and `parsed_prelude_imports_are_complete` | About 0.03s each |
| Change owned prefix publication | `rfc0165_admitted_launch::admitted_aggregates_render_structurally_as_the_interpreter_prints_them` | 0.387s |

The root-driver budget had already caught 27 drivers in an earlier slice; the
service then introduced 26 against the unchanged limit of 25. Keep fixtures
under the existing driver and run its existing budget probe before submission.
Import changes also warrant
`wasm_abi_catalog::public_spec_contains_the_generated_complete_catalog`.

The prefix failure appeared after an 85-second gate build. Its
`DropRef(uninitialized owned slot)` refusal exposed a real composition problem
constructing `List(Option(String))`. A green native WIR library suite did not
exercise that source-to-admission path. Preserve the refusal and add the source
probe to the local selection rather than weakening initialization or ownership.

Estimated benefit is 6-10 minutes of awake feedback/landing time for a service
episode like these two failures, conditional on warm focused binaries. The
prefix failure represents another 1-2 minutes of gate occupancy. These estimates
are not additive guarantees. A 0.03-second assertion can still require a build;
the measured post-repair service/driver probe took 15.81s building and 1.311s
executing. Confidence is high in the missed selections, medium in the saving.

Exact future implementation scope would be named local cases in
[`scripts/rfc0165-dev/shards.sh`](../scripts/rfc0165-dev/shards.sh) and its
[README](../scripts/rfc0165-dev/README.md), reusing existing tests. That script is
unchanged by this document. Selection can miss transitive effects, so it must
not replace the coordinator gate or alter its routing or exclusions.

**3. Investigate build reuse before consolidating commands.** On unchanged
source between the final prefix commands, Cargo reported 40.88s for WIR, 212s
for the lowerer, and 259s for source tests: 511.88s of sequential build phases
versus 13.218s of test execution. Each command compiled `witchy-wir` again;
the last two also compiled `witchy-lower`. WAT route and CLI selections took
237s and 389s building versus 1.96s executing, compiling types and WIR twice.

These are distinct useful tests. Repeated dependency compilation is the
candidate waste. Feature/package selection is a plausible contributor, but
Cargo fingerprint reasons and a matched consolidated run are unavailable.
Required linking and production-only checks still cost time. A planning range
of 2-5 minutes saved per similar round has low-to-medium confidence; it is not
a measured speedup, and the full build envelope is not recoverable savings.

Build consolidation remains a future proposal. Its exact scope would be the
local developer script, with one owned warm target and a consistent native
package selection. Before adopting it, review the old and proposed selected
test names, require nonempty selections, and verify every protected assertion
still runs. Compare feature activation as well as package names. Keep a
production-only check and feature-graph inspection separate: test feature
unification can hide unintended raw or legacy API activation. Demonstrate
selection and production-feature equivalence before a matched timing comparison.
Do not change manifests or features to make that comparison green.

Record source/diff identity, base, features, environment, command, and result in
the existing handoff. Relevant input changes invalidate reuse. A worker pass
with extra uncommitted changes does not establish integration-candidate proof.
No new telemetry framework or cross-agent result cache is proposed.

**Edit-loop and landing selections.** This is a proposed local recipe, not a
gate-policy replacement. Use an isolated worktree and an owned seeded target
when builds are eventually needed, at reduced priority during active gating.

| Change family | Small local selection | Assertions to retain |
| --- | --- | --- |
| Prefix, snapshots, writeback | Exact new regression; owned-tail, initialized-prefix, constant-successor and shared-phi negatives; `canonical_pipeline::tests::list_string_`; aggregate rendering; snapshot, projected-writeback and source-order tests | Bounds, initialized slots, moved allocation identity, no invented read authority or uniqueness, cleanup, snapshots and writeback order |
| VM service and host imports | `vm_service` / `service_byte_image` native boundaries; own/default callback relations; service runtime and owner-adapter execution; import/ABI probes | Authenticated source/callback and issuer identity, isolation, ownership transfer, short-buffer no-write, terminal cleanup |
| Admitted CLI and WAT | Exact caller positives/refusals; relevant `rfc0165_route_gate` tests; production-only feature check on manifest/visibility work | Admitted artifact reaches launch, unsupported/rejected programs refuse, argv/exit semantics, no raw/legacy production activation |
| New root integration fixture | Existing root-driver budget probe | Driver budget and continued fixture registration |

Keep independent expected outputs and the relevant source/oracle/admitted-Wasm
checks. Small stable parity examples stay in the semantic edit loop. At a
coherent-candidate checkpoint, run the complete relevant native security suites
and affected source-family regressions, plus production-only checks where
needed. A runtime-only default-feature check does not cover its native host
module; validate that native path explicitly or through the root package.

Reserve a complete example census for coverage claims and family-completion
checkpoints. The existing developer shard script starts with every `rfc0165_`
misc test, including the oracle corpus. `test-for-paths.sh` without `--focused`
selects `check.sh --fast` for Rust changes; even `--focused` can select broad
examples and Wasm. Those recipes are broader than the per-edit cases above.
At the snapshot, both final gates ran all 16 oracle corpus shards, with longest
individual durations of 146.209s and 115.209s. An earlier local corpus run and a
later gate had changed inputs, so they are not proven duplicates. No additional
corpus saving is included in the build-reuse estimate.

For landing, retain the existing coordinator's scoped workspace selection,
oracle corpus, semantic conformance, admission/ownership/bounds tests,
check/clippy, and routed Wasm, book, paired-correctness and infrastructure stages.
CI and release requirements stay unchanged. Avoid an extra local full suite
solely to repeat the coordinator. Never replace semantic assertions with
backend agreement alone or replace admission checks with structural Wasm checks.

**Warm oracle phase ranking, 2026-09-13.** The landed example filter and phase
timer were run from an already-built master `misc` binary, one example per
process, serially, with utility QoS and the stable Wasmtime cache warm. Values
are milliseconds. `Lower+admit` includes production lowering and admission:
the public observer supplies the already validated module and cannot separate
independent validation without compiler instrumentation. No compiler source was
changed for this measurement. `Not admitted` examples are assigned to the shard
but rejected before the three-leg oracle, so they add no execution phase.

| Shard | Example | Check | Lower+admit | Canonical | Source | Runtime | Spawn/JIT | Wasm | Heap | Total |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | `async_tasks` | 916.407 | 10,417.418 | 3.244 | 535.092 | 0.594 | 15.318 | 5.095 | 0.020 | 11,894.639 |
| 3 | `calc` | 119.151 | 74.254 | 6.709 | 85.203 | 0.577 | 76.372 | 0.254 | 0.040 | 362.741 |
| 3 | `files` | 127.379 | 7.174 | 0.833 | 40.692 | 0.445 | 9.323 | 0.570 | 0.004 | 186.588 |
| 3 | `guard` | 104.889 | 7.456 | 0.121 | 39.150 | 0.458 | 27.203 | 0.082 | 0.004 | 179.499 |
| 3 | `option_std` | 110.195 | 7.250 | 0.101 | 46.500 | 0.549 | 24.666 | 0.075 | 0.004 | 189.467 |
| 3 | `rle` | 105.579 | 31.743 | 3.676 | 38.544 | 0.516 | 34.159 | 0.218 | 0.026 | 214.619 |
| 3 | `zip` | 108.871 | 25.855 | 0.469 | 46.771 | 0.466 | 70.091 | 0.113 | 0.008 | 252.785 |
| 10 | `calculator` | 102.012 | 44.644 | 10.133 | 38.403 | 0.529 | 87.459 | 0.160 | 0.020 | 283.498 |
| 10 | `carried_state` | Not admitted | | | | | | | | |
| 10 | `data` | Not admitted | | | | | | | | |
| 10 | `generators` | 253.676 | 44.089 | 5.946 | 86.376 | 0.460 | 47.624 | 0.290 | 0.088 | 438.711 |
| 10 | `projects` | Not admitted | | | | | | | | |
| 10 | `scope` | 1,451.167 | 13,916.943 | 49.832 | 975.142 | 0.547 | 91.829 | 101.454 | 0.325 | 16,613.814 |
| 10 | `strings` | 99.860 | 2.566 | 0.050 | 38.421 | 0.448 | 8.225 | 0.072 | 0.004 | 149.781 |
| 10 | `toposort` | 104.940 | 26.856 | 1.575 | 49.050 | 0.511 | 60.136 | 0.123 | 0.011 | 243.353 |
| 10 | `wrap` | 106.205 | 56.478 | 2.352 | 47.558 | 0.519 | 92.812 | 0.149 | 0.012 | 306.235 |

Shard 3 sums to 13.280s and shard 10 to 18.035s. Because landing runs them
concurrently, their warm critical path is 18.035s, not their 31.316s sum.
Lowering and admission account for 79.6% and 78.1% respectively. The latest
cache-activation landing measured 88.409s and 101.116s for the whole shards;
that gate overlapped compiler work, so the 70-83s gaps are contention or cold
state candidates rather than recoverable intrinsic example work.

An immutable binary containing the unlanded Boolean-reachability candidate cut
warm lowering/admission from 10.417s to 9.756s for `async_tasks` and from
13.917s to 13.133s for `scope`. Total time fell 4.9% and 4.5%. This is useful
directional evidence, but attribution is only moderate because that binary also
contains other unlanded integration work. Confirm against the eventual landed
commit and a later uncontended gate before claiming a production improvement.

**Post-activation Boolean reachability and routing, 2026-09-13.** Commit
`920616f36` landed the Boolean CFG reachability change with 852 selected
workspace tests green. Its 101-second gate overlapped a separate compiler
profile: host load rose from 4.03 to 9.32 on 10 cores. Two measurements from the
landed, prewarmed gate binary were also contaminated by unrelated host work.
`async_tasks` took 24.910s with 945,906 involuntary context switches and `scope`
took 33.073s with 800,554. Exclude the gate and these two samples from intrinsic
before/after claims. The earlier 4.5-4.9% candidate improvement remains
directional, with moderate attribution uncertainty, until a later uncontended
landing supplies a clean comparison.

The landing log exposed a focused-selection gap: the four new
`boolean_reaches` regression tests took 0.035-0.043s each, but the canonical
compiler edit loop selected nine other `witchy-wir` checks and omitted them.
Commit `424bd21d3` added `test(boolean_reaches::)` to the existing first command;
it adds no process or compilation stage and leaves the landing gate unchanged.
The clean-tip edit check passed 61 router tests and the 16-test queue edit shard
in 44s. Its landing ran the unchanged 133-test queue suite in 91s. The expected
warm edit-loop cost is below 0.2s from the four observed test durations. If one
of these tests catches a future regression, discovery moves ahead of a roughly
101-second workspace gate; that saving is conditional because this landing was
green.

At 21:07Z the rolling three-hour post-activation cohort contained 16
submissions. First-pass success was 92.9%, exact-tip receipt adoption was 8 of
13 eligible submissions (61.5%), and the one failed attempt contributed 25s of
failed work plus 4s of lock wait. Missing receipts remained concentrated in
compiler surfaces: two `codegen`, two `typeck,interp,wasm-abi`, and one
unclassified submission. The compiler router now stays at three commands and
retains ABI, authority, ownership, bounds, admission, and semantic sentinels.
Improving use of that receipt-bearing command has more direct evidence than
adding another gate or deleting coverage.

**Evidence and reproduction of the analysis.** The source report is the local
artifact `/private/tmp/witchy-development-latency-2026-09-12.md`, produced in
Codex thread `01a09617-befb-7ad2-9ef1-e701722ee3a2`. These records are local and
may expire; they are not dependencies or required downloads for contributors.
The measured values above remain a dated record, not a new current baseline.

- Queue evidence: `state/merge-queue/journal.jsonl`, lines 13490-13543. Attempt
  timing semantics are in [`record_attempt`](../scripts/merge-queue.sh).
- Gate logs in `state/merge-queue/logs/`: filenames ending
  `canonical-vm-service-38402-93.log`, `-94.log`, and `-95.log` cover service;
  `internal-admitted-launch-38402-96.log` and `-97.log` cover its batch retry;
  `remaining-internal-launch-38402-99.log` covers the remaining callers;
  `owned-prefix-accounting-38402-100.log` and `-104.log` cover prefixes;
  `canonical-wat-integration-38402-101.log`, `-102.log`, and `-103.log` cover WAT.
  The full filenames include local dates/times; journal timestamps are UTC.
- Parent transcript: Codex session `01a052f8-a103-79a2-b062-83e35a86ef65`,
  lines 161833-164183. Worker sessions `01a09118-f018-7752-994d-10ca74acbad5`
  (lines 2380, 2540, 2932, 3164, 3397) and
  `01a09118-f12e-7730-b59a-92f25c84744b` (line 3301) locate first edits.
- Local Cargo evidence: `/private/tmp/witchy-prefix-publication-{wir,lower,source}-v2.log`,
  `/private/tmp/witchy-canonical-wat-{route-gate,cli-tests}.log`, and
  `/private/tmp/witchy-vm-service-consolidated-tests.log`.
- Sleep evidence: `pmset -g log`, filtered to Sleep/Wake/DarkWake records in
  `/private/tmp/witchy-latency-power.txt`; intersect recorded sleep intervals with
  journal attempt/stage boundaries after timezone conversion.

Do not classify the launch batch's SIGTERM as a semantic failure; its source
was not established. Prefix work also had an LLVM-symbol link failure followed
by a 159-second successful rebuild, with an unproven root cause. Neither is
included as a smaller correctness probe's claimed saving. Background check,
clippy and Wasm durations overlap foreground work; a list-progress timestamp is
not a measurement of time spent exclusively discovering tests.
