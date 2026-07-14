---
rfc: 0079
title: "Queue-sharded agent fan-out: implementer/reviewer/fixer loops"
status: proposed
created: 2026-07-08
tracking: Bun-in-Rust methodology review (bun.com/blog/bun-in-rust, 2026-07-08)
related:
  - "0071 (idiomatic-witchy dogfood cut — proposed pilot workload)"
  - "0005 (externref core — deep change previously judged not agent-feasible)"
  - "scripts/MERGE-QUEUE.md (final serialization point; unchanged by this RFC)"
---

# RFC-0079: Queue-sharded agent fan-out

## Summary

Witchy's concurrent-agent practice runs parallel *sessions*: long-lived
agents that own a branch, discover their own work, carry the whole task in
context, and review themselves. This caps out at ~4 agents (etiquette-based
collision avoidance), makes deep changes watchdog-fatal (the session IS the
state), and biases toward plausible-but-wrong merges (the author is the
reviewer). The Bun Rust rewrite demonstrated the alternative at 64 agents for
11 days: parallel *loop iterations* over a materialized work queue, with
durable state in files, roles split across context windows, and collisions
made structurally impossible by ownership sharding. This RFC adapts that
shape to witchy: a queue-materialization script, a checked-in unit-loop
workflow (1 implementer + risk-tiered diff-only adversarial review), sharded
worktrees with an owned-vs-hotspot file model, and a
trial-run-then-edit-the-loop protocol. Two deliberate departures from the Bun
run: review is tiered rather than fixed at 2 reviewers (witchy has a strong
per-phase oracle — the parity suite — that Bun lacked mid-port), and sharding
permits shared hotspot files via staging + consolidation (witchy units
converge on `src/example_tests.rs` and `book/` in a way Bun's 1:1 file ports
did not). The merge-queue coordinator stays exactly as-is — it is the
CI-analog final gate.

**Scope note.** Witchy is not a port; the queue-shaped work volume that made
Bun's full apparatus pay for itself does not exist here today. This RFC is
the reference design for the cases that ARE queue-shaped (a backend
substrate swap, the RFC-0005 site inventory, a large mechanical sweep) — it
should be implemented when such a case is scheduled, not before. Four pieces
stand alone and are adoptable immediately without any of the queue
machinery: (a) the diff-only adversarial review discipline and its two
reject rules (§2), applied selectively — NOT on every diff; reserve it for
branches touching parity-sensitive contracts (typeck/codegen/interp,
ownership analysis, ABI) where wrong-but-green is plausible, and skip it
where the differential suite already adjudicates the change; (b) the
fix-the-generator rule (§5) — recurring agent mistakes are prompt bugs, fix
CLAUDE.md/the spawning prompt, never hand-patch output; (c) verifier-once —
materialize failing tests/errors to a file for agents instead of per-agent
compiles (§1's core, minus the shard layout); (d) guide-first + trial-run
before any homogeneous sweep (§5).

## Motivation

Three observed failure modes, one root cause:

1. **Watchdog deaths on deep work.** RFC-0053 flip and RFC-0005 externref
   killed agents mid-integration because the agent's context was the only
   place the plan, the progress, and the remaining work lived. When the
   session died, everything died. (Recorded in memory as "deep changes not
   agent-feasible" — the correct reading is "deep changes not
   *session*-feasible.")
2. **Parallelism ceiling.** CLAUDE.md's protocol — announce your files, stop
   if someone touches your hunk — is social, not structural. It works at 2–4
   agents and cannot work at 16.
3. **Self-review bias + redundant verification.** Each agent implements,
   reviews itself, and runs its own `check.sh --fast` shard. The role that
   wants to ship and the role that hunts bugs share one context; and N agents
   burn N compiles for work that one phase-boundary verification would cover.

The root cause: the *session* is the unit of work. Bun's run made the *queue
item* the unit of work and the harness the owner of all durable state
(`PORTING.md`, `LIFETIMES.tsv`, `errors.txt`). An agent death cost one unit.
Every one of witchy's recurring task shapes — bug-cluster sweeps, parity
fixes, the RFC-0071 dogfood cut, an RFC-0005 site inventory — is
queue-shaped.

## Design

Five components. Only the first two are new code; the rest are protocol.

### 1. Queue materialization: `scripts/agent-queue.sh`

One command runs the relevant verifier ONCE and writes the work list to
files; agents never discover work and never run the verifier themselves.

```sh
scripts/agent-queue.sh build   [--target-dir DIR]   # cargo check → errors by crate
scripts/agent-queue.sh tests   [--target-dir DIR]   # nextest failures → by test file
scripts/agent-queue.sh files   <glob> <shard-key>   # static file list (ports, sweeps)
scripts/agent-queue.sh status
```

Output layout (gitignored, sibling to the merge queue's state):

```
scratch/agent-queue/<run-id>/
  GUIDE.md            # the task's PORTING.md-analog (see §5)
  shards/<key>/       # one dir per ownership shard (crate or folder)
    unit-NNN.md       # one unit: context, file(s), error text / task text
  done/               # units move here on commit (rename = the claim protocol)
  rejected.md         # reviewer-rejected approaches, so loops don't re-propose them
```

A unit is small by construction: one file to port, one crate's error batch,
one failing test. The shard key is the ownership boundary (see §3).

### 2. The unit loop: a checked-in Workflow

A named workflow (`.claude/workflows/unit-loop`) that any session can invoke.
Per unit, fresh context windows per role:

1. **Implementer** — gets the unit file + `GUIDE.md` + the relevant source.
   Produces a diff. Commits nothing (tier-0 units self-commit, see below).
2. **Adversarial reviewers (0–2, by unit tier)** — each gets ONLY the diff
   and the unit's acceptance criterion. Not the implementer's reasoning, not
   its chat, not the guide's rationale. Standing instruction: *assume the
   diff is wrong; your only job is to find why.* Two standing reject rules
   imported verbatim from the Bun run:
   - a paragraph-long comment justifying a workaround = reject; fix the code;
   - stubbing/gating a function to silence an error = reject.
3. **Fixer** — spawned only when a review produced findings; applies accepted
   feedback, discards rejected findings into `rejected.md`, commits the
   specific files, moves the unit to `done/`.

**Review tiers.** Bun ran 2 reviewers on every unit because, for most of the
port, nothing compiled or ran — adversarial review was the ONLY verification
in existence. Witchy is not in that situation: the differential/parity suite
and executed `book/` examples are a strong oracle, and where the oracle is
strong a second reviewer is largely redundant with it. Fixed 1+2+1 would put
a ~3–4× token multiplier on every unit for no matching risk. Instead the
queue script assigns each unit a tier (overridable in `GUIDE.md`):

- **Tier 0 — verifier-covered, mechanical** (compile-error fixes, test-file
  edits, docs/fmt, anything the phase-boundary gate would catch outright):
  0 reviewers. The verifier is the review. A sampled fraction (~1 in 10)
  is spot-checked by one reviewer to detect systematic drift.
- **Tier 1 — semantic but locally judgeable** (single-module behavior
  changes, std functions with differential tests): 1 reviewer.
- **Tier 2 — verifier-weak or high blast radius** (the `GUIDE.md` itself,
  lifetime/ownership/ABI decisions, parity-sensitive semantics where
  wrong-but-green is plausible, anything touching typeck/codegen contracts):
  2 reviewers, Bun-style.

Expected fleet average lands around 1.3–1.5 contexts per unit instead of
3–4, concentrating spend where a green gate can lie.

Loop-prompt hard rules (structural, in the workflow script, not etiquette):
no git commands except `git add <named files> && git commit` at unit end; no
cargo/nextest/check.sh in mechanical phases (the queue already embeds the
error text); no slow commands. Verification happens only at phase boundaries
(§4).

### 3. Sharding by ownership, with declared hotspots

Each workflow run gets K worktrees (K = 2–4 to start), seeded warm by the
existing `scripts/worktree-create.sh` hook / `worktree-warm.sh` CoW clone —
the disk-cost problem that forced Bun down to 4 worktrees is already solved
here. Shards map to worktrees by key: a crate (`witchy-lower`), a project
(`projects/coven-web`), a folder (`std/`).

Bun could demand fully disjoint shards because port units were 1:1 file
maps. Witchy units are not: nearly every behavioral unit wants to append a
differential test to `src/example_tests.rs`, and `book/`, `Cargo.toml`, and
the generated `spec/stdlib.md` are the same kind of convergence point. Full
disjointness would either serialize the fleet or ban units from adding
tests — both wrong. So files come in two classes:

- **Owned files** — the unit's actual work sites. Disjoint across shards,
  asserted by the queue script when it builds the run; within a worktree,
  units run concurrently only when their owned sets are disjoint. A unit
  whose owned files can't be placed in any shard is queued as sequential
  work at the phase boundary rather than forced into the fan-out.
- **Hotspot files** — a small set declared per run in `GUIDE.md`
  (`example_tests.rs`, `book/`, manifests, generated docs). Units never edit
  these directly. Each unit stages its contribution (its test function, its
  book snippet, its manifest line) as a fragment in
  `shards/<key>/staging/unit-NNN/`, and one **consolidator** agent per
  hotspot merges all fragments at the phase boundary — before the verifier
  run, so the gate sees the merged result. Consolidation is itself a unit
  (tier 1), so it gets a fresh context and review like everything else.

This trades Bun's "collisions impossible" for "collisions confined to one
agent whose whole job is merging them" — the strongest guarantee actually
available given witchy's shared-file topology.

### 4. Phase boundaries and the existing merge queue

A *phase* = queue drained. At the boundary, ONE verifier run
(`CARGO_TARGET_DIR=target-<run> ./scripts/check.sh --fast`, or the relevant
shard) re-materializes the queue from whatever it finds; if the queue comes
back empty, the branch is submitted through `scripts/merge-queue.sh submit`
unchanged. The coordinator remains the only path to master and the only
full-gate runner — this RFC adds nothing above it and removes nothing from it.

### 5. The guide, and trial-run-then-edit-the-loop

Before fan-out, two things exist in the run directory:

- **`GUIDE.md`** — the task's mapping/convention document (for RFC-0071: the
  fossil→idiomatic idiom table; for an RFC-0005 inventory: the value-rep
  classification rules). It is itself adversarially reviewed before any unit
  runs, because a guide error multiplies across every unit.
- **A 3-unit trial run.** The operator reads all three loops' output
  end-to-end. Every defect found is fixed by EDITING THE WORKFLOW PROMPT or
  the guide — never by hand-patching the output. A recurring agent mistake is
  a prompt bug. (Bun's git-stash chaos and stub-and-explain failure modes
  were both killed this way; witchy already half-practices this via
  CLAUDE.md's accumulated rules — this makes it the protocol.)

## Pilot

RFC-0071 (dogfood cut) was the natural pilot when this was drafted: units are
files under `projects/`, the guide is the idiom mapping that RFC sketches,
reviewers can judge a diff against the guide without deep context, and the
verifier is `check.sh --examples` / `--e2e`. Its sweep has since largely
landed slice-by-slice (see RFC-0071's implementation-progress log), so only
residual slices remain pilot-sized. The recommended first run is therefore
RFC-0005's site inventory — the highest-value application, converting a
"not-session-feasible" change into a queue. Parity-divergence fixing remains
the better long-term fit once repro/minimization infra exists.

## Changes to CLAUDE.md

The "Concurrent agents" section gains one paragraph: free-form concurrent
sessions remain the mode for ≤4 agents doing heterogeneous work; anything
homogeneous and >4-way MUST go through a queue run (`agent-queue.sh` + the
unit-loop workflow). The existing etiquette rules stay, scoped to the
free-form mode.

## Non-goals and risks

- **Not a replacement for the merge queue** — it feeds it.
- **Not for heterogeneous or exploratory work.** Design, audits, and novel
  debugging stay session-shaped. The queue is for work where a guide can be
  written first.
- **Cost.** The Bun run was ~$165k at API pricing for 3 engineer-years of
  output. Review tiering (§2) is the main control: tier-0 units lean on the
  verifier instead of reviewers, so spend concentrates on the units where a
  green gate can lie. The trial run exists partly to catch guide errors
  before the multiplier is paid at all. If tier-0 spot checks start finding
  systematic drift, the tier assignment is wrong — fix the queue script's
  classification, not individual units.
- **Reviewer blindness.** Diff-only review can miss cross-unit
  inconsistencies. The phase-boundary verifier and a per-phase "consistency
  sweep" unit (one agent reads all of a shard's diffs together) cover this.
- **Consolidator as bottleneck.** Hotspot consolidation (§3) serializes at
  the phase boundary. Acceptable while hotspots are few and fragments are
  append-shaped (test functions, book snippets); if a run's hotspot set
  grows large, that's a signal the units were mis-scoped, not a reason to
  parallelize consolidation.
