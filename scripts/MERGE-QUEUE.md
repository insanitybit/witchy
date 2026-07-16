# The merge/gate coordination system — operator's guide

This documents the concurrent-agent merge infrastructure well enough that a
fresh agent (or human) can operate, debug, and extend it with no other
context. The implementation is `scripts/merge-queue.sh` (~550 lines of bash);
this file explains the WHY and the invariants that are not obvious from
reading it. Companion pieces: `scripts/check.sh` (the gate),
`scripts/worktree-warm.sh` + `worktree-create.sh` (warm build caches),
`scripts/test-for-paths.sh` (focused pre-submit checks),
`scripts/worktree-status.sh` (dashboard), `scripts/warm-witchy-caches.sh`
(embedded-program cache warmer), `.config/nextest.toml` (test groups,
timeouts, setup script), and the agent-facing protocol in CLAUDE.md
("Concurrent agents") and AGENTS.md.

## The problem it solves

Multiple coding agents share this repository. Before the coordinator
(2026-07-06), two failure modes burned most of the wall-clock:

1. **Concurrent full gates.** Two `./scripts/check.sh` runs at once stretch
   each other's long-tail e2e tests (each registry e2e test spawns a
   `witchy coven-serve` subprocess that compiles embedded witchy programs;
   under CPU contention servers overrun their 30s readiness window → false
   reds that pass in isolation).
2. **Merge-invalidates-gate.** Agent A merges to master while agent B's full
   gate is running; B's green is now meaningless and must be redone.

The design: agents run only FOCUSED checks in their own worktrees; ONE
coordinator process serializes full gates and owns all merges to master.

## Architecture

```
agent worktree                    main worktree
  focused shard green   ────►  scratch/merge-queue/queue/<epoch>-<branch>.json
                                     │  (FIFO by filename sort)
                               coordinator daemon (merge-queue.sh run)
                                     │ takes queue head
                               .claude/worktrees/merge-gate   (dedicated worktree)
                                     │ rebase candidate onto master
                                     │ batch compatible queued branches on top
                                     │ run full gate under gate.lock
                                     ▼
                     green → ff-merge master → journal `merged` → sweep
                     red/timeout → journal, drop (or split batch), continue
```

All state is under `scratch/merge-queue/` **in the MAIN worktree** (each git
worktree has its own gitignored `scratch/`, so state written elsewhere is
invisible — the script resolves the main worktree itself via
`git worktree list`):

- `queue/*.json` — one pending submission per file. **Queue order = filename
  sort order.** Files are named `<epoch>-<branch-with-slashes-as-~>.json`;
  `submit --front` prefixes `0front-` which sorts before any epoch digit.
  Reordering the queue by renaming files is legitimate and was done live.
- `journal.jsonl` — append-only event log, the system's ground truth.
  Events: `submitted`, `merged`, `red`, `timeout`, `conflict` (won't rebase),
  `blocked` (gate GREEN but ff-merge refused — see below), `requeued`
  (master moved mid-gate), `dropped` (branch deleted), `batch_red`,
  `validated` (test-mode green, merge skipped), `swept`. `merged`/`red`/
  `timeout` carry the gate log path, elapsed seconds, and a stage-timing
  summary parsed from check.sh's `==> [N] stage (t+Ns)` markers.
- `logs/` — full gate output per attempt (nextest streams one line per test:
  `NEXTEST_STATUS_LEVEL=pass`).
- `gate.lock/` — a DIRECTORY used as the mutex (`mkdir` is atomic). Contains
  `pid`, `what`, `branch`, `log`, `started`. A lock whose pid is dead is
  stolen by the next acquirer. `with-lock -- <cmd>` lets ANY heavyweight
  command (ad-hoc full suite, a commit to master) share the mutex.
- `coordinator.pid` — pid of the persistent daemon. ONLY the persistent
  `run` loop writes it (a `run --once` clobbering it caused a
  two-coordinators incident; both modes now refuse to start beside a live
  daemon).
- `coordinator.log` — daemon stdout/stderr. `daemon` creates a new session
  (`setsid -f` on systems that provide it, POSIX::setsid via system Perl on
  macOS) so terminal or tool-host process-group cleanup cannot orphan a gate.
- `prewarmed` — master sha the gate worktree was last idle-prewarmed to.

## Command reference

```
merge-queue.sh submit [--front] <branch> [note]  enqueue (pre-checks mergeability
                                                 via git merge-tree — refuses what
                                                 would only journal `conflict`;
                                                 MERGE_QUEUE_SKIP_PRECHECK=1 overrides;
                                                 warns on file overlap with queued
                                                 branches and if no coordinator runs)
merge-queue.sh wait <branch> [secs]              block until terminal journal event
                                                 (default 3600s); prints it as JSON;
                                                 exit 0 iff merged. submit && wait
                                                 is the standard agent pattern.
merge-queue.sh run [--once]                      coordinator loop (--once drains and
                                                 exits; refuses beside a live daemon)
merge-queue.sh daemon                            start a new-session coordinator (survives
                                                 the launching session)
merge-queue.sh status                            JSON: queue, in-flight gate (branch,
                                                 stage, elapsed, log age), recent journal
merge-queue.sh doctor                            human health check: coordinator alive?
                                                 lock stale? which stage? log fresh?
merge-queue.sh stats                             journal analytics: outcome counts,
                                                 last-10 gate seconds, repeat-red
                                                 branches, flake-shaped failures
merge-queue.sh with-lock -- <cmd...>             run anything under the gate mutex
merge-queue.sh resolve <branch>                  after a manual ff following `blocked`:
                                                 verifies the sha is on master, journals
                                                 the closing `merged` event
merge-queue.sh sweep                             remove worktrees whose branch this
                                                 queue merged (journal-verified) and
                                                 whose tree is clean; also -d's merged
                                                 branches. Runs automatically after
                                                 every merge.
```

Environment knobs: `MERGE_QUEUE_GATE_CMD` (default `./scripts/check.sh`),
`MERGE_QUEUE_GATE_TIMEOUT` (2700s), `MERGE_QUEUE_STALL_TIMEOUT` (600s of no log
output **while the gate's process group is idle** — a group still burning CPU is
compiling/testing, not hung, so silence alone never kills it; see the stall
note below), `MERGE_QUEUE_BUSY_SILENCE_MAX` (3× stall = 1800s: the ceiling on
silence even for a *busy* group, so a CPU-burning runaway is reclaimed here
rather than at GATE_TIMEOUT), `MERGE_QUEUE_BATCH_MAX` (5),
`MERGE_QUEUE_STATE_DIR` +
`MERGE_QUEUE_GATE_WT` (isolated state for TESTING the coordinator itself),
`MERGE_QUEUE_ALLOW_MERGE=1` (test mode still merges — see Testing below).
`check.sh` raises the bounded stage-heartbeat count from three to eight
two-minute pulses whenever `WITCHY_GATE_SCOPE` is present, enough for measured
four-wide macOS discovery without making the watchdog unbounded.
On macOS, `.config/nextest.toml` routes test discovery through
`scripts/nextest-list-wrapper.sh`. The wrapper bounds only the `--list` binary
launches to four slots, keyed by `NEXTEST_RUN_ID` (or the shared nextest parent
PID on older versions); test execution runs at nextest's normal width — the
dyld stall was a cold-first-exec problem, and the list phase leaves every
binary loader-warm. `WITCHY_NEXTEST_LIST_JOBS` permits local retuning.

**Diff-scoped fuzzing.** The differential fuzzer is the gate's single biggest
test (~57s, a fixed-seed parity regression suite). `process_one` classifies the
batch diff (`base..sha`) and passes `WITCHY_GATE_FUZZ` to check.sh: `skip` when
nothing under the parity surface changed (`crates/`, `std/`, `src/`, `examples/`,
`projects/`, `build.rs`, `Cargo.*` — a docs/rfc/bug/config-only merge cannot
change backend behavior), `reduced` (10 seeds, ~12s, vs 30/~57s) when it did, and
`full` on any doubt (git error / empty diff — fail-safe). The full 30-seed sweep
under the checked heap still runs post-merge on CI (`ci.yml`) and in `check.sh
--full`, so reduced/skip lowers *pre-merge* cost without removing the regression
net — only its position. Standalone `check.sh` (no env set) runs `full`.

**Diff-scoped gate (docs-only).** The same classification also sets
`WITCHY_GATE_SCOPE`: when EVERY changed path in the batch diff is documentation
no test or gate stage reads — `rfcs/` (except `rfcs/performance-modes.md`,
which `example_tests::public_sources_do_not_call_legacy_render_intrinsic`
reads), `wiki/` and `bugs/` (tracked but read by nothing), and the gitignored
`scratch/`/`security-eval/` — check.sh
skips the heavy stages entirely (`scope=docs` in the gating note): such a diff
cannot change any stage's outcome, so the suite would only re-validate the
already-gated master tree, and post-merge CI still runs the complete suite as
the backstop. Anything else (`book/`, `spec/`, `README.md`, `scripts/`,
`.claude/`, `.github/`, Cargo metadata, …) runs the full gate; empty/errored
diffs fail safe to `all`. Standalone `check.sh`, `--fast`, `--full`, and the
shards ignore the scope.

## The gate lifecycle, step by step (process_one)

1. Read queue head. Branch deleted → journal `dropped`, consume, next.
2. **Acquire the lock BEFORE touching the gate worktree** (an earlier version
   rebased first — that corrupts a with-lock run already using the worktree).
3. Record `base` = current master sha. Detach gate worktree onto the branch,
   `rebase master`. Failure → journal `conflict`, drop, release lock.
4. **Batching:** walk the rest of the queue in order; each candidate branch's
   SHA (detached — the agent's branch ref is never moved) is rebased onto the
   current stack tip. Clean rebase → joins the batch (up to
   MERGE_QUEUE_BATCH_MAX). Textual file overlap is FINE — only a failed
   rebase excludes. Members carrying a `.nobatch` marker (from a previous
   red batch) are skipped, and a head with `.nobatch` gates strictly alone.
5. Run the gate: own process group (`set -m`), stdout to the log,
   `NEXTEST_STATUS_LEVEL=pass` for streaming, and Cargo wrapper variables
   cleared so detached coordinators do not inherit a sandbox-incompatible
   global sccache process. Discovery pressure on the macOS gate host is bounded
   by the nextest list wrapper (see above); execution runs at nextest's normal
   concurrency. A monitor loop kills the group on
   overall timeout, or on log-stall **only when the group is also idle** (CPU
   near zero) — a silent-but-CPU-busy gate is compiling/enumerating, not hung —
   and journals `timeout`. `check.sh` emits three two-minute stage heartbeats for
   standalone runs and eight in the serialized gate to bridge nextest's
   legitimate compile-to-first-result silence; the bounded pulses cannot mask a
   deadlock indefinitely. The default idle window is ten minutes.
6. Outcomes:
   - **green, solo:** if master still == base → `git merge --ff-only <sha>`,
     journal `merged`, sweep. If master moved → journal `requeued`, keep the
     queue file (fresh rebase next loop). If the ff itself fails (dirty main
     worktree / not on master) → journal `blocked` with the VALIDATED sha;
     a human/agent completes it manually (`git merge --ff-only <sha>`) and
     runs `resolve <branch>` to close the record. Never re-gates — the sha
     is already validated.
   - **green, batch:** same, but every member gets its own `merged` journal
     entry (with `batch: N`) and its queue file consumed. Branch refs are
     NOT force-moved (the merged sha contains other branches' commits;
     pointing an agent's ref at it would hand it unrelated work) — sweep
     handles cleanup via `git cherry` patch-equivalence.
   - **red/timeout, solo:** journal with log + stage summary, drop the file.
     The submitter fixes and resubmits.
   - **red/timeout, batch:** journal `batch_red`; NO member is blamed; every
     member keeps its queue file and gains `.nobatch` so each re-gates
     individually. Nothing is ever merged unvalidated.
7. Queue empty → idle prewarm: under the lock, move the gate worktree to
   master, `cargo build --workspace`, run `warm-witchy-caches.sh`, record
   the sha in `prewarmed`. The next gate starts hot.

## Invariants (the load-bearing rules — do not break these when extending)

1. **Every commit on master was validated by a full gate against the exact
   master it landed on** (or is an explicitly hand-authorized `with-lock`
   infra commit). The requeue-on-master-moved check is what makes this true.
2. **At most one full gate / heavyweight suite runs at a time** (the
   directory lock; everything heavyweight goes through `with-lock`).
3. **The journal is append-only ground truth.** Agents make decisions from
   it (Codex declined to act on "blocked" state — correctly). If reality
   diverges from the journal (manual ff), fix the JOURNAL (`resolve`), not
   the habit of trusting it.
4. **A red batch indicts nobody.** Individual re-gating is mandatory; blame
   requires a solo gate.
5. **Agents' branch refs belong to agents.** The coordinator gates SHAs,
   detached; it never rewrites a submitted branch (except the pre-batching
   solo path where branch == merged sha exactly).
6. **State lives in the MAIN worktree's scratch/merge-queue.** Never
   per-worktree.
7. **Sweep only removes what the JOURNAL says this queue merged** and only
   clean trees. Ahead-count alone cannot distinguish a fresh agent worktree
   from a merged one — that heuristic almost deleted a working agent's
   checkout once.

## Known sharp edges / history (why the odd-looking code exists)

- **`run --once` vs coordinator.pid:** --once used to clobber the pid file →
  doctor reported NO COORDINATOR while a healthy daemon ran → operators
  started a second daemon → two coordinators raced the queue. Only the
  persistent loop writes the pid now; both modes refuse to double-start.
- **Test-mode merge guard:** `MERGE_QUEUE_STATE_DIR` isolates queue state but
  NOT the merge target. A harness test once fast-forwarded the REAL master
  with a test commit (caught, rewound — and the rewind itself briefly
  dropped a just-merged real commit, also restored). Hence: state-dir set
  and `MERGE_QUEUE_ALLOW_MERGE != 1` → gate runs, merge is SKIPPED,
  journal `validated`.
- **Main worktree may be on an agent branch:** the coordinator must not
  `git merge <validated-sha>` into whatever branch is checked out and then
  journal it as a master merge. If `master` is not checked out, it atomically
  moves only `refs/heads/master`, guarded by the base SHA that was gated.
- **bash 3.2:** macOS ships bash 3.2 — no `${var^^}`, no associative arrays.
  The uppercase-via-`tr` is deliberate.
- **The daemon holds its script's inode.** To edit merge-queue.sh while the
  daemon runs: `mv` the script aside (daemon keeps the old inode), `cp` it
  back, edit the copy, commit, then restart the daemon at an idle moment
  (kill pid → `daemon`). Editing in place risks bash reading a half-new file.
- **Lock is stealable only on dead pid.** A live-but-stuck holder needs a
  human `kill`; `doctor` shows holder pid + what + elapsed + log age.
- **Stall detection is CPU-gated, not log-only (2026-07-10).** The stall monitor
  once killed on 300s of no log output alone. But the gate legitimately goes
  silent for minutes — from t+0, since the tests stage now runs FIRST:
  `nextest run` compiles the `test` profile (separate artifacts from the
  `dev`-profile `cargo build`/`clippy` artifacts, so it is a full compile),
  then spawns `--list` across ~17 test binaries and runs the warm-caches setup
  script, all before the first streamed `PASS`. Under CPU contention (concurrent agent builds) that silent window blew
  the 300s clock and killed HEALTHY gates: over a single day, 56 of 56
  `timeout`s were this false positive, never a real hang, each burning a full
  gate's wall-clock and forcing a resubmit. Fix: the monitor now consults
  `group_is_busy` (sum of `ps -g <pgid> -o %cpu`) and only kills on silence WITH
  an idle group. A real hang (deadlock/blocked syscall) consumes no CPU so it
  still trips; a CPU-burning runaway (busy-spin) that stays silent past
  `BUSY_SILENCE_MAX` (6× stall = 1800s, well above any compile+enumeration, well
  below `GATE_TIMEOUT`) is reclaimed there rather than blocking the serialized
  queue for the full 45-min whole-gate ceiling. Raising `STALL_TIMEOUT` would
  only move the cliff; the compile is genuinely long, so liveness is the right
  signal.
- **`blocked` almost always means the main worktree is dirty** with an
  untracked file the merge would overwrite, or master is checked out
  somewhere unexpected. The gate result stays valid; only the ff needs help.
- **check.sh shards:** `--fast` (commit gate: build+clippy+tests minus e2e),
  `--e2e`, `--examples`, `--wasm` — agents' pre-submit validation.
  `test-for-paths.sh` maps a diff to the right shards. The `witchy` e2e
  tests live in the `registry-serial` nextest group (width 2, retries 1,
  priority 100 so the long pole starts first); the keyword filter in
  `.config/nextest.toml` must cover every test that spawns a registry server
  — `cargo nextest show-config test-groups` audits membership.
- **Gate-speed history (2026-07-07):** gates went ~360s → ~180-270s via:
  registry-serial width 1→2 + complete filter; e2e test consolidation;
  at-scale guards made compiled-only; `[profile.dev.package]` opt-level=1
  for the six hot crates; the warm-witchy-caches nextest setup script
  (wasmtime caches are keyed on binary mtime+size → every merge invalidates
  them → without the warmer, every spawned subprocess recompiled the
  embedded pm/coven); idle prewarm. The remaining test-stage floor is the
  registry-serial chain; the planned next steps are BUG-554 (coven snapshot
  race — REQUIRED before any shared-server fixture) then a shared registry
  fixture (design + blockers in scratch/shared-fixture-findings-2026-07-07.md).

## Testing the coordinator itself

Point it at throwaway state and NEVER at real master:

```sh
export MERGE_QUEUE_STATE_DIR=/tmp/mq-test MERGE_QUEUE_GATE_WT=/tmp/mq-test/gwt
git branch t-x master   # make test branches, commit in a temp worktree
./scripts/merge-queue.sh submit t-x
MERGE_QUEUE_GATE_CMD='bash -c "true"' ./scripts/merge-queue.sh run --once
# journal shows `validated` (merge skipped) unless MERGE_QUEUE_ALLOW_MERGE=1
```

Clean up: remove temp worktrees (`git worktree remove`), delete test
branches, `rm -rf /tmp/mq-test`, `git worktree prune`. Check `git log
--oneline master` afterwards — if a test commit leaked (guard bypassed),
rewind with `git reset --keep <good-sha>` UNDER `with-lock`, and re-check
you didn't drop a real commit that landed meanwhile.

## Recovery playbook

| Symptom | Action |
|---|---|
| doctor: coordinator NOT RUNNING | `./scripts/merge-queue.sh daemon` — state is on disk; nothing is lost between coordinators |
| lock held, holder pid dead | next acquirer steals it automatically; or `rm -rf scratch/merge-queue/gate.lock` if nothing will acquire soon |
| lock held, holder alive but gate silent | expected during the `test`-profile compile / test enumeration; the monitor now kills only after 300s of silence WITH an idle process group (a busy group is compiling, not hung). If it is genuinely wedged AND idle it self-kills; a spinning runaway is caught by `GATE_TIMEOUT`. Only `kill <holder>` by hand if both clocks are somehow not progressing. |
| journal says `blocked` | gate was GREEN: `git merge --ff-only <sha from journal>` in the main worktree, then `merge-queue.sh resolve <branch>` |
| branch red repeatedly, uniform ~32s e2e failures | environmental (server readiness under load), not the branch: check what else is hammering the machine, resubmit |
| need to reorder the queue | rename files in `queue/` (sort order = order) or use `submit --front` |
| queue file for an already-merged branch | harmless: the rebase collapses to master, gate passes, ff is a no-op; or just `rm` the file |
| two coordinators running | fixed class (pid-file guard), but if seen: `kill` the older, verify `coordinator.pid` names the survivor |
```
