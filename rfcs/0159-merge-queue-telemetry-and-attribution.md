---
rfc: 0159
title: "Merge-queue telemetry and change attribution"
status: proposed
created: 2026-08-25
superseded-by:
related:
  - "0001 (documentation structure)"
  - "0093 (gate performance restructuring)"
  - "0079 (queue-sharded fan-out)"
tracking: >
  Proposed; revised 2026-08-26 after review. The revision reconciles the
  design with two things that landed on 2026-08-25 while it was being
  written — `scripts/gate-report.sh` (windowed, `--json`, schema 2) and the
  host-contention covariate (`71237deac`) — corrects §E now that red
  `reason`/`stages` are known to carry the failing stage already, promotes
  the pre-submit receipt to the lead lever on re-derived numbers, and fixes
  a TOCTOU in the proposed dirty-checkout remedy. No implementation yet.
---

# RFC-0159: Merge-queue telemetry and change attribution

## Summary

The merge queue records how *the pipeline* behaved but almost nothing about
*the changes flowing through it*. Every submission is attributed to the OS
user, so the journal cannot distinguish one agent, model, or workflow from
another. This RFC makes the queue record actor, change shape, and pre-submit
evidence at submit time; replaces the cumulative `stats` command with a
windowed one that reports the latency decomposition the journal already
captures; and fixes two operational defects the data exposed.

It deliberately does **not** introduce per-person productivity scoring. §7
says why, and which questions this data may and may not be used to answer.

## Motivation

### What we can measure today

Two things landed on 2026-08-25, while this RFC was being drafted, and the
design below is written against them rather than around them:

- **`scripts/gate-report.sh`** already produces a windowed report
  (`--since 24h|7d|all`, `--state-dir`, `--json`, schema 2) covering gate and
  attempt timing. It is the reporting front end; this RFC extends it and does
  not build a second one.
- **The host-contention covariate** (`71237deac`) journals
  `gate_load_start`, `gate_load_end`, and `host_ncpu` on every
  attempt-terminal event from the single `record_attempt` choke point, and
  `gate-report.sh` buckets attempts by load per core. Contention is therefore
  no longer an unmeasured variable — it is a *newly* measured one, present on
  33 of 546 terminal events as of 2026-08-26 simply because it is a day old.

An analysis of `state/merge-queue/journal.jsonl` (9,177 events,
2026-07-07 .. 2026-08-25: 2,624 submitted, 1,906 merged, 309 red, 182
batch_red, 1,310 requeued, 127 timeout, 113 dropped) shows the gate itself is
healthy:

| stage | p50 | p90 | p99 |
|---|---|---|---|
| full gate (n=1,141) | 272s | 854s | 5,240s |
| docs-only fast path (n=197) | 10s | — | — |
| prepare (rebase + warm) | 64s | 184s | — |
| lock wait | 0s | 0s | 1s |

Lock contention is a non-issue — 0.1% of merges wait over 60s. Throughput is
41.4 merges/day over 46 active days.

But decomposing time-to-land into work versus idle (n=1,337) shows where the
latency actually lives:

| | p50 | p90 | p95 |
|---|---|---|---|
| time-to-land | 8.3m | 213m | 660m |
| attempt work | 5.1m | 16.7m | 33.5m |
| **idle in queue** | 0.6m | 193m | 651m |

For the 131 changes that took over 4h, median idle was 653m against 3.5m of
actual work — **99% of their latency was waiting for a coordinator to exist**.
46 inter-merge gaps over 4h total 498h, 42% of the window. Merges are uniform
across all 24 UTC hours, so this is not working hours.

### What we cannot measure at all

The queue answers "was the pipeline fast?" It cannot answer any of:

- Which agent, model, or workflow produced this change?
- Are changes from one workflow redder than another?
- Did the author run the pre-submit shard, and did it predict the gate?
- How large or risky was this change? Did it touch parity-sensitive code?
- Did the pre-submit shard actually complete, or was it cut short?

One item that appeared in an earlier draft of this list has been struck:
"is a red gate a real defect, a flake, or infrastructure?" is *partly*
answerable today. Red, `batch_red`, and `timeout` events already carry a
`reason` string of the form
`[1] tests (workspace) (t+0s): FAIL [ 56.998s] (108/3136) <test name>` and a
`stages` field naming the stage. The failing stage and failure line are
present; what is missing is that they are concatenated into prose rather
than recorded as fields, so grouping requires re-parsing. §E is scoped
accordingly — it is a structuring change, not a data-capture one.

The blocker is attribution, and it is total:

- `scripts/merge-queue.sh` records `--arg by "${USER:-unknown}"` at four
  sites — the **OS user**, identical for every agent and every human. 2,620 of
  2,624 submissions read `cobrien`.
- Git is no better: 400 of the last 400 master commits are authored *and*
  committed by `Colin OBrien <insanitybit@gmail.com>`.
- `CLAUDE.md` already mandates a `Co-Authored-By:` trailer naming the model.
  It appears on **7 of the last 400 commits** (1.75%).

So a convention that would supply attribution already exists and is
essentially never followed. That is the cheapest lever available.

### The red-rate regression we can see but not explain

| week | submitted | red | red% |
|---|---|---|---|
| W28 | 433 | 53 | 12.2% |
| W29 | 673 | 107 | 15.9% |
| W30 | 290 | 30 | 10.3% |
| W31 | 188 | 16 | 8.5% |
| W32 | 162 | 43 | 26.5% |
| W33 | 173 | 48 | 27.7% |
| W34 | 563 | 164 | 29.1% |
| W35 | 142 | 30 | 21.1% |

Roughly a 3x regression from W31 to W34. With today's data this is a dead end:
we cannot tell whether it is a change in what is being submitted, who is
submitting it, whether pre-submit shards are being skipped, or new flakes. Each
of those has a different fix. **This is the concrete cost of the data gap.**

### What the reds cost, and where they fail

Re-derived from the journal over the seven days ending 2026-08-26 — the
numbers above are lifetime figures and the two windows disagree, so this one
is dated and reproducible rather than cumulative:

| | value |
|---|---|
| merged attempts | 346 |
| failed attempts | 200 (182 genuine, 18 environmental) |
| total gate time | 31.0h |
| gate time in attempts that did not land | **14.0h — 45%** |
| …of which genuine | 10.8h (78%) |
| …of which environmental | 3.1h (22%) |
| reds with no `reason` | **0** |

Split by failing stage, the genuine reds are not spread across the gate.
They are concentrated in one place:

| stage | count | gate-hours | share of *all* gate time |
|---|---|---|---|
| **tests** | **171** | **10.25h** | **33%** |
| queue infrastructure | 3 | 0.40h | 1.3% |
| `witchy fmt` | 1 | 0.10h | 0.3% |
| compile check | 3 | 0.02h | 0.1% |
| clippy, wasm build, other | 3 | 0.02h | 0.1% |

**171 of 182 genuine reds (94%) fail in the `tests` stage, consuming a third
of all gate time — and `./scripts/check.sh --fast` is precisely the shard
that runs those tests before submission.** This is the single largest
recoverable cost in the queue, it is recoverable with evidence the author
already produced locally, and it is why §D is the lead item of this RFC
rather than the fourth.

Note also that "45 all-time red events carry no `reason`" is a lifetime
artifact: over the last seven days, zero reds are unattributed. Reason
capture is not the gap; reason *structure* is.

## Design

The items below are ordered for readability, not by value. Ranked by
measured value they are: **D** (pre-submit receipts — 33% of all gate time),
then the two operational defects in the following section (the coordinator
gap is 99% of tail latency), then **C** and **E** (which make red rate
interpretable), then **A**, **B**, **F**, **G**. §Delivery sequencing turns
that ranking into two branches.

### A. Actor identity at submit time

Add `MERGE_QUEUE_ACTOR` (free-form, e.g. `claude-opus-5`, `codex`, `human`) and
`MERGE_QUEUE_SESSION` (opaque session id). `cmd_submit` records:

```
actor        — from $MERGE_QUEUE_ACTOR, else "unset"
session      — from $MERGE_QUEUE_SESSION, else ""
by           — unchanged ($USER); it is the machine account, not the author
```

`actor` is recorded verbatim and never inferred. `"unset"` is a first-class
value: it must be visible in reporting so adoption is measurable rather than
silently defaulting to a wrong name.

Agents set these from their harness. `.claude/settings.json` can export them
for Claude Code sessions so the common path needs no per-agent work.

An environment variable an agent forgets to set is weak evidence, so record
a second, *derived* field beside it: `actor_hint`, inferred from the
submitting worktree path (`.claude/worktrees/…` → `claude`,
`.codex/worktrees/…` → `codex`, the shared checkout → `master-checkout`).
The two fields are kept distinct on purpose — `actor` is verbatim and
authoritative, `actor_hint` is a guess — and reporting shows both so that
disagreement between them is visible rather than silently resolved.

### B. Enforce the trailer that already exists

`CLAUDE.md` mandates `Co-Authored-By:`. Make `submit` **warn** when the branch's
commits lack it and record `trailer_present: true|false`. Warn, not reject —
rejecting would block work on a convention that is currently 1.75% adopted, and
the warning plus the adoption metric is enough to drive it up. Revisit hard
enforcement once adoption is measured above ~90%.

### C. Change shape at submit time

`submit` already computes the branch SHA; add, from the merge-base diff:

```
files_changed, insertions, deletions
touches: ["typeck","codegen","interp","analysis","wasm-abi","std","docs","queue-infra"]
```

`touches` is derived from path globs against the parity-sensitive contracts
named in the witchy-dev briefing. This is what makes "red rate" interpretable:
a 29% red rate on compiler changes and on docs changes are different facts.

### D. Pre-submit evidence — persist the receipt that already exists

`check.sh` **already emits a structured per-stage receipt** on stdout:

```
WITCHY_TIMING {"schema":1,"kind":"foreground","step":1,
  "name":"tests (workspace, minus e2e)","status":"green",
  "started_epoch":...,"finished_epoch":...,"elapsed_s":397,"gate_elapsed_s":397}
```

It has a schema version, a per-stage `status`, and timings. Nothing consumes
it — not `merge-queue.sh`, not CI. It is printed and discarded.

So this is not a new format, it is persistence and consumption of an existing
one. `check.sh` appends its `WITCHY_TIMING` lines to
`state/merge-queue/receipts/<sha>.jsonl`; `submit` reads that file and records
`presubmit_shard`, `presubmit_status`, and `presubmit_sha_match`.

`status` MUST distinguish three outcomes, not two:

- `green` — the stage ran to completion and passed.
- `red` — the stage ran to completion and failed.
- `truncated` — the stage was killed before completing (timeout, SIGTERM,
  operator interrupt).

The third value is load-bearing. While preparing this RFC I ran the pre-submit
shard as `timeout 900 ./scripts/check.sh --fast | tail -20`, which failed in
both available ways at once: the pipe reported `tail`'s exit status rather than
the gate's, and the 900s budget SIGTERM'd nextest 207 tests into 3,107. The run
reported **exit 0 while 2,900 tests never executed**, and I read that as green.
A receipt that only knows pass/fail reproduces exactly this ambiguity — a
truncated run is not evidence of anything and must never satisfy criterion 1.
`submit` warns when the receipt for the submitted SHA is `truncated` or absent.

This also directly tests the leading hypothesis for the red-rate regression:
that `--fast` no longer predicts the full gate, or is being skipped.

Three details the implementation must not skip:

- **The receipt is a claim about the author's tree, not a prediction of the
  gate.** The coordinator rebases onto current master before gating, so a
  green receipt on SHA *X* says "*X* was green in isolation", not "the
  rebased tree will be green". `presubmit_sha_match` records whether the
  receipt even refers to the submitted SHA; reporting must not present a
  matching green receipt as a guarantee. It is expected to shrink the 171
  tests-stage reds, not to eliminate them.
- **Adding `truncated` changes the `WITCHY_TIMING` contract.** `status`
  gains a third value, so the record's `"schema":1` becomes `"schema":2`,
  and any consumer added later must reject a schema it does not know rather
  than treat an unrecognised `status` as green.
- **`state/merge-queue/receipts/` needs a retention rule.** One file per
  shard run per SHA grows without bound in a gitignored directory nobody
  looks at. Prune on write: drop receipts older than 30 days, and drop any
  receipt whose SHA is an ancestor of current master.

### E. Red-cause classification

Scoped down from an earlier draft, which proposed persisting data that is in
fact already persisted. Red, `batch_red`, and `timeout` events already carry
`failure_summary()` output in `reason` and a stage label in `stages`; the
seven-day window above was decomposed by stage from exactly those fields.
The work is therefore *structuring*, not capture:

- Split the existing `reason` into `failing_stage` and `failure_line` at
  record time, so grouping is a field lookup rather than a regex over prose.
  Keep `reason` — it is the human-readable form and existing consumers read
  it.
- Add `red_class`: `test | compile | clippy | fmt | timeout | infra |
  unknown`.

`red_class` must consume the contention covariate rather than guess around
it. A `tests`-stage failure on a box at load 8/core and the same failure at
load 0.5/core are different events, and `gate_load_start` / `host_ncpu` now
make the distinction available at classification time. Classify a stall
timeout under high load as `infra`; classify the same timeout on an idle box
as `unknown` and surface it — that is a flake worth a bug, not weather.

### F. Coordinator uptime as a first-class signal

The single largest latency contributor — 99% of the tail — is currently
invisible because *nothing is written when nothing is happening*. Emit a
`heartbeat` event every 5 minutes while `run` is alive. Coordinator uptime then
becomes directly computable instead of inferred from gaps between merges.

### G. Windowed reporting — extend `gate-report.sh`, do not duplicate it

An earlier draft proposed replacing `cmd_stats` with a new windowed reporter
taking `--since` and `--json`. That reporter already exists:
`scripts/gate-report.sh` takes `--since 24h|7d|all`, `--state-dir`, and
`--json`, emits schema 2, and already buckets attempts by load per core.
Building a second one inside `merge-queue.sh` would split the reporting
surface across two scripts with two schemas and two definitions of a window
— the exact drift this RFC exists to prevent.

So:

- **`gate-report.sh` is the reporting front end.** Extend it with the
  dimensions this RFC adds: `--by actor|touches|red_class`, the
  work-vs-idle decomposition, first-pass-clean rate, coordinator uptime, and
  `actor` adoption rate. Its schema goes 2 → 3.
- **`cmd_stats` loses its lifetime-cumulative counters** and becomes a thin
  forwarder to `gate-report.sh`, so `merge-queue.sh stats` stays a working
  entry point without being a second implementation. Per house style this is
  one cut: the cumulative output is removed, not kept alongside.

The work-vs-idle decomposition is the addition that matters most — it is
what would have made the 99%-idle finding self-evident, and the timing
fields it needs are already in the journal.

## Operational defects to fix alongside

Two defects surfaced by the same analysis. Both are independent of the
telemetry work and are cheap.

### 1. The dirty-checkout hot loop (24.3h total stall)

On 2026-07-24/25, `rfc0105-test-fixtures` logged **1,052 requeues over 24.3h
with zero merges repo-wide**. 1,252 of all 1,310 lifetime requeues carry the
reason `main master checkout has tracked changes before gate`; 10 changes hold
95% of all requeues.

The cause is loop shape, not policy. `process_one` runs `prepare` (rebase in
the warm worktree, p50 64s) and acquires the gate lock *before* checking
`main_worktree_is_ready_to_land`, then requeues on failure with a flat
15s `poll_interval` and no cap. Each cycle burns a full rebase to discard it.

The current placement is deliberate, and an earlier draft of this RFC
proposed a fix that would have broken it. `merge-queue.sh` comments the
check as running under `gate.lock` "immediately before the expensive
operation", and that is load-bearing: the shared checkout can be dirtied by
another agent *during* the 64s rebase, so a check that runs only before
`prepare` is a time-of-check/time-of-use race that would let a gate start
against a dirty tree. Moving the check is the wrong fix.

Fix:
- Add a **cheap early check** before `prepare` so the common case fails fast
  and does not burn a rebase. This is an optimisation, not the guard.
- **Keep the authoritative check under the lock, where it is.** It is the
  only one that is race-free, and it stays.
- Exponential backoff, 15s to a 10m ceiling, while the reason persists —
  this is what actually ends the hot loop, independent of where the check
  runs.
- Emit one `stalled` event after 3 consecutive deferrals naming the blocking
  files, then log once per backoff period rather than per poll.

The policy — never gate against a dirty shared checkout — is correct and stays.
Only the retry shape changes. Note the irony worth recording: the stall was
caused by tracked edits sitting in the shared master checkout, which
`CLAUDE.md` already forbids.

### 2. Keep a coordinator alive

Addresses 99% of the p90+ tail and is worth more than every other item here
combined. `run` already holds a singleton lock, so an auto-start is safe
against duplicates — but duplication is not the hazard. **Lifetime is.**

A coordinator started as a child of a submitting agent's shell dies when
that session ends, which is precisely the moment the queue most needs one
alive: an agent submits its last branch and exits. An auto-start that
inherits the submitter's process group therefore recreates the gap it was
meant to close, intermittently and invisibly.

So if `submit` auto-starts a coordinator, it must **fully detach** it —
`setsid`, redirected stdio, no shared process group — and journal a
`coordinator_started` event recording that it was auto-started and by which
`actor`, so the uptime signal in §F can distinguish an auto-started
coordinator from an operator-run one. A launchd agent (macOS) or a systemd
user unit is the more durable answer and needs no change to `submit`; the
auto-start is the version that works today on every developer's machine
without external configuration. Recommended: auto-start detached, and
revisit a supervisor once §F's uptime data shows whether detached
auto-starts actually stay alive.

## What this is not

**This is not a productivity measurement system, and the data must not be used
as one.** Three specific traps, stated so a future reader does not repeat them:

- **Merge counts are not comparable work units.** A docs merge gates in 10s; a
  compiler change gates in up to 5,240s. Counting merges rewards splitting
  trivial changes.
- **Latency is mostly not attributable to the author.** 99% of tail latency is
  coordinator idle. Ranking authors by time-to-land would mostly rank them by
  whether someone happened to be running `run`.
- **Red is not misconduct.** The gate exists to catch defects before master. A
  red branch is the process working. Driving red rate toward zero by punishing
  it produces under-submission and larger, riskier batches.

The legitimate questions this data answers are about *changes and process*:
which code areas are red-prone, whether pre-submit shards predict the gate,
which failures are flakes, and how much latency is infrastructure. Per-actor
breakdown is in scope because the actors are largely automated workflows and
the useful output is "this workflow needs a better pre-submit shard" — a
statement about tooling. It is not a ranking of people.

## Migration

Per house style, one cut and no compatibility layer. New fields are additive to
an append-only journal, so old events simply lack them; `stats --since` windows
default to 7d, well inside the post-change period. `cmd_stats` is replaced, not
kept alongside. Existing consumers of `elapsed_s` are unaffected.

## Acceptance criteria

1. `submit` records `actor`, `session`, `trailer_present`, `files_changed`,
   `insertions`, `deletions`, `touches`, `presubmit_shard`, `presubmit_status`,
   `presubmit_sha_match`. A `truncated` or absent receipt is warned on and is
   never treated as evidence of a green shard.
2. `red`, `batch_red`, `timeout` carry `failing_stage`, `failure_line`,
   `red_class`.
3. `run` emits `heartbeat` at most 5 minutes apart while alive.
4. `gate-report.sh --since 7d --json` (schema 3) reports throughput,
   time-to-land percentiles, work-vs-idle split, red rate by `touches` and by
   `red_class`, coordinator uptime, and `actor` adoption rate. No second
   windowed reporter exists: `merge-queue.sh stats` forwards to it.
5. A dirty shared checkout produces backoff and a `stalled` event, not an
   unbounded 15s requeue loop. A cheap readiness check precedes `prepare`
   **and** the authoritative check remains under `gate.lock`; a test
   dirtying the checkout between the two must still prevent the gate.
6. A coordinator auto-started by `submit` survives the exit of the
   submitting shell, and journals `coordinator_started` naming the `actor`
   that started it.
7. `scripts/MERGE-QUEUE.md` documents the journal schema, including which
   fields are recorded verbatim versus derived, and the receipt retention
   rule.
8. `state/merge-queue/receipts/` is pruned on write and does not grow
   without bound.

## Delivery sequencing

Two branches, in this order. They are separable because the operational
fixes depend on none of the telemetry fields.

**Branch 1 — recover the wasted gate time.** §D (receipts, including the
`truncated` status and the schema bump), operational defect 1 (backoff plus
the two-phase readiness check), operational defect 2 (detached
auto-start). This is the whole measured win: 33% of gate time in
`tests`-stage reds, and 99% of tail latency in coordinator idle. It touches
`check.sh` and `merge-queue.sh` only, and none of it needs a schema
negotiation with `gate-report.sh`.

**Branch 2 — make the rest interpretable.** §A, §B, §C, §E, §F, and §G,
rebased on branch 1. This is the branch that changes `gate-report.sh` to
schema 3, and it is worth landing behind branch 1 so the reporting change is
made once, against fields that already exist.
