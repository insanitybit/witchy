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
- When your branch is green on its shard: `./scripts/merge-queue.sh submit <branch>`.
  The coordinator rebases it onto latest master in a warm worktree, runs the
  single serialized full gate, and fast-forwards master on green (re-gating if
  master moved). Watch the outcome with `./scripts/merge-queue.sh status` or
  `scratch/merge-queue/journal.jsonl`; gate logs are in `scratch/merge-queue/logs/`.
- If `submit` or `./scripts/merge-queue.sh doctor` says NO COORDINATOR RUNNING,
  start the detached one: `./scripts/merge-queue.sh daemon` (survives your
  session; log in `scratch/merge-queue/coordinator.log`).
- `./scripts/worktree-status.sh` is the dashboard of all worktrees/branches
  (dirty, ahead/behind, queued, merged-and-removable). Report-only.
- Worktrees hold multi-GB `target/` dirs, so clean up after merge: the
  coordinator auto-sweeps journal-merged, clean worktrees after every merge
  (`./scripts/merge-queue.sh sweep` runs the same thing manually). It will
  never touch dirty/unmerged/queued trees — remove those yourself with
  `git worktree remove <path>` once you're done with them.
- If you genuinely need a heavyweight suite yourself, share the lock:
  `./scripts/merge-queue.sh with-lock -- <cmd>`.

