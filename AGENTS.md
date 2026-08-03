# Working in this repo with agents

Read `CLAUDE.md` first. It is the shared agent note for this repo and includes
the Witchy-specific build, parity, formatting, and concurrency rules.

When another agent or developer is active in the same checkout:

- Run `git status --short --branch` before editing and before reporting done.
- Own files explicitly in your status updates. If another agent edits the same
  file or hunk, stop and ask instead of rewriting over it.
- Do not revert, delete, or reformat changes you did not make.
- Use an isolated Cargo target directory for long checks so agents do not fight
  over `target/` — and SEED it first so it isn't a cold multi-minute build
  (CoW clone, seconds, ~zero disk; workspace crates stay warm too since the
  source path is identical):

```sh
./scripts/worktree-warm.sh --target-dir target-codex
CARGO_TARGET_DIR=target-codex cargo test --workspace
CARGO_TARGET_DIR=target-codex cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use
```

- Clean up only artifacts you created, such as your own `target-codex/`.
- Do not kill Cargo, dev-server, or test processes unless you started them or
  the user explicitly asks you to.

## Dirty shared master is actionable

If the merge queue cannot run because the shared master checkout has tracked
changes, do not repeatedly report or wait on that condition. Assume interrupted
agent work may need recovery.

1. Inspect the status, diff, worktree dashboard, queue journal, and any
   ownership or handoff notes.
2. Determine whether a live agent still owns the changes. A stale worktree,
   stopped process, or absent recent activity is not active ownership.
3. Preserve the exact diff before moving or changing it.
4. Reconcile the work:
   - If coherent and complete, move it to an appropriately named branch or
     worktree, validate it, commit it, and submit it.
   - If coherent but incomplete, move it to an isolated worktree and finish it.
   - If unrelated changes are mixed together, split them into separate recovery
     branches.
   - If changes are generated artifacts or demonstrably obsolete, remove them
     only after preserving enough evidence to recover them.
5. Restore the shared master checkout to a clean state, then allow the merge
   queue to proceed.
6. Never discard ambiguous user work. If ownership or intent cannot be
   established, preserve it on a recovery branch or patch before asking for
   guidance.

Unexpected changes require reconciliation, not automatic abandonment. Do not
overwrite them in place. Inspect ownership, preserve the diff, and move the work
to isolation before continuing. Ask the user only when the preserved changes
have genuinely ambiguous intent that cannot be resolved from repository
evidence.

`Shared master has tracked changes` is not, by itself, a blocker. The agent owns
driving that state to a clean, recoverable conclusion.

## Large-scope delivery

Large RFCs and cross-cutting projects should optimize for total completion
latency, not serial task completion. Decomposition exists to enable concurrency
and verification; it is not permission to reduce the requested scope.

- Before implementation, turn the acceptance criteria into a dependency graph
  and a live acceptance ledger. Identify the critical path and the contracts
  that independent tracks share.
- Start independent compiler, runtime, host, tooling, test, documentation, and
  evidence tracks immediately in isolated branches or worktrees. Do not make
  one track wait for another unless the dependency graph requires it.
- Freeze narrow shared interfaces early. Keep one integration track responsible
  for combining completed work, resolving contract drift, and keeping an
  end-to-end slice runnable throughout development.
- Run focused checks continuously in each track. Update generated lockfiles,
  manifests, censuses, snapshots, and evidence with the change that invalidates
  them instead of leaving reconciliation to the end.
- Report progress against acceptance criteria, unresolved dependencies, and the
  critical path. Commit counts and lines changed are not completion evidence.
- Treat the merge queue as a landing serializer, never an implementation
  serializer. Continue independent work while gates run.
- If work that can be independent is becoming serial, change the execution plan
  immediately and call out the bottleneck in the next status update.

## Merging: use the gate coordinator

Do not run the full `./scripts/check.sh` gate yourself, and do not merge to
master directly — full gates must be serialized (the publish e2e is
load-flaky under overlap) and a merge landing mid-gate invalidates that gate.

- In your worktree run only focused checks: `./scripts/test-for-paths.sh`
  prints the ones matching your diff (`--run` executes them); the building
  blocks are `./scripts/check.sh --fast` / `--e2e` / `--examples` / `--wasm`.
- When your branch is green on its shard: `./scripts/merge-queue.sh submit <branch>`,
  then **KEEP WORKING — do not idle on the gate.** Start the next queue item on
  a fresh branch off master immediately. The gate lock serializes full gates
  and merges to master, nothing else: it never blocks editing, committing, or
  `check.sh --fast` in your own worktree with your own target dir. Use
  `./scripts/merge-queue.sh wait <branch>` (blocks until the terminal journal
  event; exit 0 iff merged) only when your next task stacks on the pending
  branch; otherwise check the journal for reds before ending your session and
  fix/resubmit then. The coordinator BATCHES compatible queued branches (clean
  rebase + disjoint files) into one gate, so deep queues no longer cost one
  full gate per branch — submitting and moving on makes batching MORE
  effective, not less; a red batch re-gates every member individually, so a
  batch can never blame or block your branch unfairly. `submit --front
  <branch>` exists for genuinely urgent fixes — use sparingly. The coordinator
  rebases it onto latest master in a warm worktree, runs the single serialized
  full gate, and fast-forwards master on green (re-gating if master moved).
  Watch the outcome with `./scripts/merge-queue.sh status` or
  `state/merge-queue/journal.jsonl`; gate logs are in `state/merge-queue/logs/`.
  After the one-time cutover, `scratch/merge-queue` remains a compatibility
  symlink so older agents and historical journal log paths continue to work.
  `state/agents/` may hold local diagnostics and handoff notes, but it is not
  an ownership protocol or a lock; live status and explicit file ownership in
  agent updates remain authoritative.
- While a gate is live (`status` shows `gate_lock`), your builds compete with
  it for CPU — the slow-gate outliers (5-10× normal) are exactly gates that
  overlapped agent builds. Run long builds/tests at reduced priority so the
  gate stays ~3min: `taskpolicy -c utility cargo …` / `taskpolicy -c utility
  cargo nextest …`. Hold off on the e2e-heavy shards (`--e2e`, `--examples`)
  until the gate finishes; build/clippy/`--fast` are fine anytime.
- If `submit` or `./scripts/merge-queue.sh doctor` says NO COORDINATOR RUNNING,
  start the detached one: `./scripts/merge-queue.sh daemon` (survives your
  session; log in `state/merge-queue/coordinator.log`).
- `./scripts/worktree-status.sh` is the dashboard of all worktrees/branches
  (dirty, ahead/behind, queued, merged-and-removable). Report-only.
- Worktrees hold multi-GB `target/` dirs, so clean up after merge: the
  coordinator auto-sweeps journal-merged, clean worktrees after every merge
  (`./scripts/merge-queue.sh sweep` runs the same thing manually). It will
  never touch dirty/unmerged/queued trees — remove those yourself with
  `git worktree remove <path>` once you're done with them.
- If you genuinely need a heavyweight suite yourself, share the lock:
  `./scripts/merge-queue.sh with-lock -- <cmd>`.
