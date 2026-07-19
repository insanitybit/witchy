# The merge/gate coordination system — operator's guide

This documents the concurrent-agent merge infrastructure well enough that a
fresh agent (or human) can operate, debug, and extend it with no other
context. The implementation is `scripts/merge-queue.sh`;
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
  focused shard green   ────►  state/merge-queue/queue/<epoch>-<branch>.json
                                     │  (first dependency-ready item by filename sort)
                               coordinator daemon (merge-queue.sh run)
                                     │ takes queue head
                               .claude/worktrees/merge-gate   (dedicated worktree)
                                     │ replay unrepresented patches onto master
                                     │ batch compatible queued branches on top
                                     │ run full gate under gate.lock
                                     ▼
                     green → ff-merge master → journal `merged` → sweep
                     red/timeout → journal, drop (or split batch), continue
```

All state is under `state/merge-queue/` **in the MAIN worktree** (each git
worktree has its own gitignored `state/`, so state written elsewhere is
invisible — the script resolves the main worktree itself via a pipefail-safe
`git worktree list` scan):

`scratch/merge-queue` is a compatibility symlink after the one-time
`migrate-state` cutover. Older agents and absolute log paths already stored in
the journal therefore resolve to the same files; it is never a second queue.
The sibling `state/agents/` directory is reserved for optional local diagnostics
and handoff notes. It is observational, not a locking or file-ownership system.

- `queue/*.json` — one pending submission per file. **Queue order = filename
  sort order.** Files are named `<epoch>-<branch-with-slashes-as-~>.json`;
  `submit --front` uses a reverse-timestamped `00front-` prefix, so it sorts
  before ordinary and legacy front entries. Re-submitting an already queued
  change with `--front` moves it to the actual head; the newest urgent
  reprioritization sorts first.
  Reordering the queue by renaming files is legitimate and was done live.
  Schema-2 entries carry a stable `change_id`, a per-submission `attempt_id`,
  and an `after` array of parent change IDs. The coordinator skips
  waiting/blocked entries rather than letting one dependency stall unrelated
  ready work. Legacy entries without these fields remain independent and ready.
- `changes/*.json` — persistent logical-change registry. The branch-named file
  is its current generation; completed generations move to
  `history-<change-id>.json` so existing descendants retain an addressable
  parent even when a branch name is reused. A change ID survives SHA updates
  and red-parent resubmission, but a branch reused after merge/drop gets a new
  ID. Dependencies use these IDs, not mutable branch SHAs.
- `change.lock/` — short metadata mutex for registry/queue mutations. It is
  held for milliseconds, never around a rebase, build, test, or gate, and is
  separate from `gate.lock`; unrelated editing and agent checks do not wait on
  it. Atomic mutation makes concurrent submissions cycle-safe and prevents a
  coordinator state update from overwriting a resubmission.
- `journal.jsonl` — append-only event log, the system's ground truth.
  Events: `submitted`, `merged`, `red`, `timeout`, `conflict` (won't rebase),
  `blocked` (gate GREEN but ff-merge refused — see below), `requeued`
  (master moved mid-gate), `dropped` (branch deleted), `batch_red`
  (strategy `prefix_split` / `culprit_evict` / `individual`), `evicted`
  (culprit member sent to a solo gate — see batch red below), `rebaselined`
  (prepare regenerated a stale generated snapshot onto the candidate),
  `validated` (test-mode green, merge skipped), `swept`. `merged`/`red`/
  `timeout` carry the gate log path, elapsed seconds, and a stage-timing
  summary parsed from check.sh's `==> [N] stage (t+Ns)` markers.
- `status` resolves all queued entries from one queue/registry snapshot, so it
  remains usable when a large batch is waiting.
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
- `coordinator.lock/` — lifetime singleton for the persistent loop. Atomic
  creation closes the PID-file startup race; the owner exits if it loses the
  lock, and a new owner conservatively reaps idle pre-fix sibling loops without
  touching the PID-file keeper or gate-lock holder (BUG-580).
- `coordinator.log` — daemon stdout/stderr. `daemon` creates a new session
  (`setsid -f` on systems that provide it, POSIX::setsid via system Perl on
  macOS) so terminal or tool-host process-group cleanup cannot orphan a gate.
- `prewarmed` — master sha the gate worktree was last idle-prewarmed to.

## Command reference

```
merge-queue.sh submit [--front] [--after <parent>]... <branch> [note]
                                                 enqueue; --after is repeatable,
                                                 preserves a stable change ID,
                                                 and rejects unknown parents,
                                                 self-dependencies, and cycles
                                                 (pre-checks mergeability
                                                 via git merge-tree — refuses what
                                                 would only journal `conflict`;
                                                 MERGE_QUEUE_SKIP_PRECHECK=1 overrides;
                                                 warns on file overlap with queued
                                                 branches and if no coordinator runs)
merge-queue.sh wait <branch> [secs]              block until terminal journal event
                                                 (default 3600s); prints it as JSON;
                                                 exit 0 iff merged. submit && wait
                                                 is the standard agent pattern.
merge-queue.sh migrate-state                     one-time guarded cutover to state/
                                                 (requires empty queue, stopped
                                                 coordinator, and free gate lock)
merge-queue.sh run [--once]                      coordinator loop (--once drains and
                                                 exits; refuses beside a live daemon)
merge-queue.sh daemon                            start a new-session coordinator (survives
                                                 the launching session)
merge-queue.sh status                            JSON: queue entries with change ID,
                                                 readiness, waiting_on/blocked_by;
                                                 in-flight gate (branch, stage,
                                                 elapsed, log age); recent journal
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
`MERGE_QUEUE_GATE_TIMEOUT` (optional emergency whole-gate ceiling; `0`, the
default, disables it), `MERGE_QUEUE_STALL_TIMEOUT` (600s of no log
output **while the gate's process group is idle** — a group still burning CPU is
compiling/testing, not hung, so silence alone never kills it; see the stall
note below), `MERGE_QUEUE_BUSY_SILENCE_MAX` (3× the stall window: the ceiling on
silence even for a *busy* group, so a CPU-burning runaway is reclaimed here
rather than relying on a whole-gate ceiling), `MERGE_QUEUE_BATCH_MAX` (5),
`MERGE_QUEUE_DOCS_BATCH_MAX` (25, activated only when every path in every
candidate ends in `.md`),
`WITCHY_STATE_DIR` (override the canonical local state root),
`MERGE_QUEUE_STATE_DIR` +
`MERGE_QUEUE_GATE_WT` (isolated state for TESTING the coordinator itself),
`MERGE_QUEUE_TEST_ROOT` + `MERGE_QUEUE_ALLOW_TEST_ROOT=1` (explicitly gated
throwaway repository root for migration fixtures),
`MERGE_QUEUE_ALLOW_MERGE=1` (test mode still merges — see Testing below).
`check.sh` raises the bounded stage-heartbeat count from three to eight
two-minute pulses whenever `WITCHY_GATE_SCOPE` is present, enough for measured
bounded macOS discovery without making the watchdog unbounded.
On macOS, `.config/nextest.toml` routes test discovery through
`scripts/nextest-list-wrapper.sh`. The wrapper bounds only the `--list` binary
launches to two slots by default, keyed by `NEXTEST_RUN_ID` (or the shared
nextest parent PID on older versions); test execution runs at nextest's normal
width — the dyld stall was a cold-first-exec problem, and the list phase leaves
every binary loader-warm. Slots are atomic PID symlinks: a crashed owner is reclaimed,
while an EPERM result from `kill -0` is treated as a live sandboxed process.
There is no time-based fail-open that can turn slow healthy discovery back into
an unbounded loader herd. `WITCHY_NEXTEST_LIST_JOBS` permits local retuning;
production defaults to two because four simultaneous distinct cold binaries
measured no faster in aggregate, while one-wide developed a long tail.
Serialized gates also default `CARGO_PROFILE_TEST_STRIP=symbols`. This removes
the large local-symbol table that macOS otherwise pages in for every discovery
and test invocation; it does not alter the test inventory, debug assertions, or
overflow checks. Focused developer runs retain symbols. Set
`CARGO_PROFILE_TEST_STRIP=none` explicitly when a symbolic native backtrace is
required in a coordinator gate.
Each successful slot acquire/release also touches a
coordinator-owned progress sidecar, so healthy waves reset the idle watchdog
without treating synthetic log heartbeats as liveness.

**Diff-scoped queue infrastructure.** `tests/merge_queue.rs` launches detached
coordinators, process groups, lock holders, and nested Git repositories. It is
excluded from the concurrent workspace test stage and runs first as an isolated,
one-thread shard when the batch changes the queue substrate. The coordinator
sets `WITCHY_GATE_QUEUE_INFRA=1` for changes to the queue/check scripts, their
nextest configuration, or their focused tests. Operators can run the same shard
directly with `./scripts/check.sh --queue-infra`, or force it into an exact-master
baseline with `WITCHY_GATE_QUEUE_INFRA=1 ./scripts/check.sh`. Product and semantic
validation is unchanged; only the machine-sensitive queue fixtures move out of
contention with it.

**Gate fail-fast (check.sh).** The merge-gate profile runs the tests as the
only foreground stage with three background legs — `cargo check --workspace
--all-targets` (surfaces plain compile errors in minutes), clippy, and the
wasm playground build — each in its own CoW-seeded target dir
(`target-check`, `target-clippy`). While the tests run, check.sh polls the
legs' logs (`WITCHY_FAILFAST_POLL`, default 5s) and, the moment a leg records
a failure, ABORTS the foreground tests, prints the red leg's full output, and
exits red — a clippy/compile failure costs ~2-4 min instead of surfacing only
after ~20+ min of doomed tests. The aborted tests stage is emitted as
`WITCHY_TIMING … "status":"aborted"`; consumers that only read green records
(gate-report.sh) ignore it. Green gates are unchanged: overlap, not
serialization, and all legs are still collected — and can still fail the
gate — before green. Idle prewarm also warms `target-check`.

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

1. Select the first dependency-`ready` item in filename order. Waiting and
   blocked entries remain visible in `status` while unrelated ready work passes
   them. Branch deleted → journal `dropped`, consume, next.
2. Record `base` = current master sha and detach the coordinator-owned gate
   worktree onto that exact tree. Use `git cherry` to select only submitted
   patches not represented by `base`, then cherry-pick them in order. This
   preparation is outside `gate.lock`: `with-lock` users never touch the
   coordinator-owned worktree, and unrelated candidate preparation must not
   serialize behind a full gate. A checkout or replay failure journals a loud
   terminal event; merge commits are rejected rather than silently losing
   merge-only conflict resolutions. Submit linear history (rebase/flatten it).
3. **Batching:** explicit dependency descendants take priority. A child joins
   when all its parents are already merged or in the current stack. Ready
   co-parents in the same dependency component join too; repeated passes
   produce topological order, then the stack tip is gated once. If no
   descendant joins, walk other ready entries for the existing opportunistic
   batch. Every candidate contributes only patches not already represented by
   the current stack tip (detached — the agent's branch ref is never moved).
   Clean replay → joins. Semantic and mixed batches stop at
   `MERGE_QUEUE_BATCH_MAX`; a batch whose every changed path ends in `.md`
   may grow to `MERGE_QUEUE_DOCS_BATCH_MAX`. Every candidate is reclassified
   before joining, so one code/config path restores the semantic ceiling.
   Textual overlap is fine; only a failed rebase excludes. `.nobatch` applies
   to unrelated red-batch recovery. A `.batch-limit` marker bounds the next
   dependency-prefix retry.
3b. **Snapshot re-baseline:** deterministic generated artifacts (the RFC-0087
   census TSV `rfcs/0087-migration-census.tsv` and `witchy doc`-rendered
   `spec/stdlib.md`) go stale whenever an unrelated branch lands first, and a
   stale snapshot turns a correct candidate into a ~28-min red. After the
   batch is prepared, the coordinator builds the two generator bins in the
   gate worktree, re-runs them, and — if either committed output drifted —
   commits ONLY those two whitelisted files onto the candidate as
   `chore(gate): re-baseline generated artifacts`, journaling `rebaselined`.
   The gated sha is captured AFTER this step, so the amended sha is exactly
   what is classified, gated, and fast-forwarded. A generator build/run
   failure never fails the candidate (regen is skipped; the gate
   adjudicates), and regen is skipped entirely for docs-safe-set-only diffs
   so docs gates stay seconds. For code diffs the prepare-time `cargo build`
   mostly warms artifacts nextest needs anyway, so green-gate totals barely
   move.
4. Classify the prepared batch diff, then acquire `gate.lock`. Re-check every
   immutable queue attempt and verify master is still `base`; if submission or
   master moved during preparation, release and rebuild without gating. When
   the main worktree has `master` checked out with tracked staged or unstaged
   edits, defer the batch before the full gate and retry after it is clean.
   Untracked local tool state does not defer a gate; Git itself remains the
   collision authority for an untracked path a candidate would overwrite.
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
   - **red/timeout, dependency stack:** journal `batch_red` with
     `strategy: prefix_split`; no member is blamed. Re-gate the first half as a
     stack prefix. A green prefix lands and unblocks the suffix; another red
     halves again. This locates and lands the green prefix without accepting an
     unvalidated commit.
   - **red/timeout, unrelated batch:** journal `batch_red`; every member
     keeps its queue file. On a RED with a parsable failing target (a nextest
     FAIL/TIMEOUT line, or a rustc `-->`/`could not compile` context), the
     coordinator scores each member's own diff by name overlap with that
     target (failing test's source file, binary/test-path name stems, the
     failing crate's directory). A unique positive top score journals
     `evicted` and marks ONLY that member `.nobatch` (solo gate); the
     remaining N-1 re-batch together next loop — 2 follow-up gates instead
     of N. No signal or a tie falls back to marking every member `.nobatch`
     (strategy `individual`). Eviction never blames terminally: it only
     chooses who re-gates alone; a terminal red still requires that member's
     own solo gate, and nothing lands unvalidated (invariant 4 holds).
7. Queue empty → idle prewarm: under the lock, move the gate worktree to
   master, `cargo build --workspace`, run `warm-witchy-caches.sh`, record
   the sha in `prewarmed`. The next gate starts hot. A submission arriving
   during this opportunistic work terminates only the prewarm process group;
   the coordinator releases the same lock and advances the queue immediately.

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
4. **A red batch indicts nobody.** An ordered dependency stack may only narrow
   by re-gating prefixes; an unrelated batch re-gates individuals. Blame and a
   terminal red state require a solo gate.
5. **Agents' branch refs belong to agents.** The coordinator gates SHAs,
   detached; it never rewrites a submitted branch (except the pre-batching
   solo path where branch == merged sha exactly).
6. **State lives in the MAIN worktree's state/merge-queue.** Never
   per-worktree. The scratch path is compatibility only.
7. **Sweep only removes what the JOURNAL says this queue merged** and only
   clean trees. Ahead-count alone cannot distinguish a fresh agent worktree
   from a merged one — that heuristic almost deleted a working agent's
   checkout once.
8. **A queue attempt owns one submitted SHA, not a mutable branch ref.** A
   resubmission while that SHA gates keeps the logical change ID but replaces
   its queued SHA. Compare-before-write transitions prevent the old attempt
   from marking or deleting the newer submission; it receives its own gate
   before descendants become ready.

## Known sharp edges / history (why the odd-looking code exists)

- **`run --once` vs coordinator.pid:** --once used to clobber the pid file →
  doctor reported NO COORDINATOR while a healthy daemon ran → operators
  started a second daemon → two coordinators raced the queue. Only the
  persistent loop writes the pid now; both modes refuse to double-start.
- **Coordinator lifetime singleton (BUG-580):** `coordinator.pid` alone was
  blind to displaced siblings and had a read/write race. `coordinator.lock/`
  is now atomically owned for the whole loop; concurrent daemon starts elect
  one winner, and PPID inspection is advisory so a sandbox-denied `ps` cannot
  kill the winner or `doctor`.
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
- **Gate process groups are lock-owned.** While a full gate runs, `gate.lock`
  records its PGID. A graceful coordinator exit terminates that group before
  releasing the lock; after an untrappable coordinator death, the next lock
  acquirer terminates the recorded orphan before preparing another candidate.
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
  `BUSY_SILENCE_MAX` (3× stall = 1800s, well above any compile+enumeration) is
  reclaimed there without relying on an arbitrary whole-suite duration.
  Raising `STALL_TIMEOUT` would only move the cliff; the compile is genuinely
  long, so liveness is the right signal.
- **Tracked edits on checked-out main `master` defer before a gate.** The queue
  keeps the submission queued and retries once the checkout is clean, avoiding
  a full gate that cannot safely fast-forward. A later `blocked` event usually
  means an untracked collision introduced during the gate, or a worktree state
  Git refused at the final fast-forward; the gate result remains valid and only
  the landing needs help.
- **check.sh shards:** `--fast` (commit gate: build+clippy+tests minus e2e),
  `--e2e`, `--examples`, `--wasm`, `--queue-infra` — agents' pre-submit
  validation. The queue-infrastructure shard is serial and hermetic by design.
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
git branch t-parent master   # make test branches, commit in a temp worktree
git branch t-child t-parent
./scripts/merge-queue.sh submit t-parent
./scripts/merge-queue.sh submit --after t-parent t-child
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
| lock held, holder pid dead | next acquirer steals it automatically; or `rm -rf state/merge-queue/gate.lock` if nothing will acquire soon |
| lock held, holder alive but gate silent | expected during the `test`-profile compile / test enumeration; the monitor kills after 600s of silence WITH an idle process group (a busy group is compiling, not hung), or after 1800s of busy silence. Only `kill <holder>` by hand if both clocks are somehow not progressing. |
| journal says `blocked` | gate was GREEN: `git merge --ff-only <sha from journal>` in the main worktree, then `merge-queue.sh resolve <branch>` |
| queue item says dependency `blocked` | fix and resubmit the terminal parent branch; its stable change ID is reused and the child remains linked. Do not resubmit the child merely to bypass the parent. |
| branch red repeatedly, uniform ~32s e2e failures | environmental (server readiness under load), not the branch: check what else is hammering the machine, resubmit |
| need to reorder the queue | use `submit --front`; it also reprioritizes an existing queued change |
| queue file for an already-merged branch | harmless: the rebase collapses to master, gate passes, ff is a no-op; or just `rm` the file |
| two coordinators running | fixed class (pid-file guard), but if seen: `kill` the older, verify `coordinator.pid` names the survivor |
```
