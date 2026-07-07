# Working in this repo with agents

Read `CLAUDE.md` first. It is the shared agent note for this repo and includes
the Witchy-specific build, parity, formatting, and concurrency rules.

When another agent or developer is active in the same checkout:

- Run `git status --short --branch` before editing and before reporting done.
- Own files explicitly in your status updates. If another agent edits the same
  file or hunk, stop and ask instead of rewriting over it.
- Do not revert, delete, or reformat changes you did not make.
- Use an isolated Cargo target directory for long checks so agents do not fight
  over `target/`:

```sh
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

- In your worktree run only a focused shard: `./scripts/check.sh --fast`, or
  `--e2e` / `--examples` / `--wasm` for the section your change touches.
- When your branch is green on its shard: `./scripts/merge-queue.sh submit <branch>`.
  The coordinator rebases it onto latest master in a warm worktree, runs the
  single serialized full gate, and fast-forwards master on green (re-gating if
  master moved). Watch the outcome with `./scripts/merge-queue.sh status` or
  `scratch/merge-queue/journal.jsonl`; gate logs are in `scratch/merge-queue/logs/`.
- If you genuinely need a heavyweight suite yourself, share the lock:
  `./scripts/merge-queue.sh with-lock -- <cmd>`.

