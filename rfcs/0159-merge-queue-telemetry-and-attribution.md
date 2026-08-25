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
tracking: "Proposed. No implementation yet."
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
- Is a red gate a real defect, a flake, or infrastructure?

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

## Design

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

### D. Pre-submit evidence

`check.sh` writes a receipt to `state/merge-queue/receipts/<sha>.json`
(shard name, exit status, duration, git SHA). `submit` reads it and records
`presubmit_shard` and `presubmit_sha_match`. This directly tests the leading
hypothesis for the red-rate regression: that `--fast` no longer predicts the
full gate, or is being skipped.

### E. Red-cause classification

`record` already has `failure_summary()`. Persist it on `red`/`batch_red`/
`timeout` as `failing_stage` and `failure_line`, so reds can be grouped by
cause without re-reading gate logs. Add `red_class` with a deliberately coarse
heuristic: `test | compile | clippy | fmt | timeout | infra | unknown`.

### F. Coordinator uptime as a first-class signal

The single largest latency contributor — 99% of the tail — is currently
invisible because *nothing is written when nothing is happening*. Emit a
`heartbeat` event every 5 minutes while `run` is alive. Coordinator uptime then
becomes directly computable instead of inferred from gaps between merges.

### G. Windowed `stats`

Replace `cmd_stats` (lifetime-cumulative counts) with:

```
merge-queue.sh stats [--since 7d] [--by actor|touches|red_class] [--json]
```

reporting, for the window: merges, throughput/day, time-to-land percentiles,
the **work-vs-idle decomposition**, gate percentiles split by scope, red rate,
first-pass-clean rate, coordinator uptime, and `actor` adoption. The timing
fields already exist in the journal — `stats` simply never surfaced them. This
is the change that would have made the 99%-idle finding self-evident.

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

Fix:
- Check `main_worktree_is_ready_to_land` **before** `prepare`, not after.
- Exponential backoff, 15s to a 10m ceiling, while the reason persists.
- Emit one `stalled` event after 3 consecutive deferrals naming the blocking
  files, then log once per backoff period rather than per poll.

The policy — never gate against a dirty shared checkout — is correct and stays.
Only the retry shape changes. Note the irony worth recording: the stall was
caused by tracked edits sitting in the shared master checkout, which
`CLAUDE.md` already forbids.

### 2. Keep a coordinator alive

Addresses 99% of the p90+ tail and is worth more than every other item here
combined. Either have `submit` start a coordinator when none holds the
singleton lock, or run `daemon` under a supervisor. `run` already holds a
singleton lock, so an auto-start is safe against duplicates. Recommended:
auto-start on submit, since it needs no external supervisor.

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
   `insertions`, `deletions`, `touches`, `presubmit_shard`,
   `presubmit_sha_match`.
2. `red`, `batch_red`, `timeout` carry `failing_stage`, `failure_line`,
   `red_class`.
3. `run` emits `heartbeat` at most 5 minutes apart while alive.
4. `stats --since 7d --json` reports throughput, time-to-land percentiles,
   work-vs-idle split, red rate by `touches`, coordinator uptime, and `actor`
   adoption rate.
5. A dirty shared checkout produces backoff and a `stalled` event, not an
   unbounded 15s requeue loop; the readiness check precedes `prepare`.
6. A coordinator is auto-started by `submit` when none holds the singleton lock.
7. `scripts/MERGE-QUEUE.md` documents the journal schema, including which
   fields are recorded verbatim versus derived.
