# Development latency: September 11-12, 2026

This contributor note records three opportunities from existing agent transcripts,
Cargo logs, and merge-queue events. It is a documentation-only follow-up, not a
change to compiler behavior, feature activation, test selection, or gate policy.
The implementation suggestions below remain proposals. Follow the current
[agent playbook](agile-agent-playbook.md) and
[queue operator guide](../scripts/MERGE-QUEUE.md) when applying them.

The measurement snapshot ends at `8f907d084861d00bdeec9bed76e2c6f514d68e24`,
landed at 2026-09-12 14:36:20 UTC. Later compiler work is outside this sample.
All durations are historical observations, not promises about the current tree.
The investigation ran no builds or suites and changed no processes or power
settings. This follow-up adds only this file.

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
