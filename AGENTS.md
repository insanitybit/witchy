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
CARGO_TARGET_DIR=target-codex cargo clippy --workspace --all-targets -- -D warnings
```

- Clean up only artifacts you created, such as your own `target-codex/`.
- Do not kill Cargo, dev-server, or test processes unless you started them or
  the user explicitly asks you to.

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
