---
name: land
description: Land a finished branch on master via the merge-queue coordinator — pick and run the right check.sh shard, submit, make sure a coordinator is alive, watch the gate, and report the outcome. Use when a branch is ready to merge, when asked to "land"/"merge"/"submit" work, or to diagnose why a submitted branch hasn't merged.
---

# Land a branch through the merge queue

The protocol (CLAUDE.md "Concurrent agents"): agents never run the full
`./scripts/check.sh` gate themselves and never merge to master directly. One
coordinator serializes the full gate and fast-forwards master. Your job is a
green *focused shard*, a submission, and a watched outcome.

## 1. Pre-flight

- `git status --short --branch` — everything you're landing must be committed
  on your branch; nothing you didn't write should be swept in.
- If you're in the shared main worktree (not an isolated one), use a per-agent
  target dir for the shard: `CARGO_TARGET_DIR=target-<agent> …`.

## 2. Run the shard that matches the diff

`./scripts/test-for-paths.sh` prints the focused checks for your diff
(`--run` executes them too). Or pick by `git diff master --name-only`:

| Diff touches | Shard |
|---|---|
| Rust in `crates/` or `src/` | `./scripts/check.sh --fast` (always, for any code change) |
| `projects/grimoire`, `projects/coven`, server/registry/publish code | also `./scripts/check.sh --e2e` |
| `examples/`, `book/`, `std/*.witchy` | also `./scripts/check.sh --examples` |
| codegen / lowering / anything wasm-shaped | also `./scripts/check.sh --wasm` |
| docs/markdown only (no code, no executed ```witchy blocks) | no shard needed |

A green shard qualifies the branch for the queue; it does not replace the full
gate — the coordinator runs that once, serialized.

## 3. Submit

```sh
./scripts/merge-queue.sh submit <branch> "one-line note"
```

## 4. Make sure a coordinator is alive

```sh
./scripts/merge-queue.sh doctor
```

If it says `coordinator : NOT RUNNING`, start the detached daemon:
`./scripts/merge-queue.sh daemon` — it survives your session ending
(log: `state/merge-queue/coordinator.log`). `submit` also warns about this,
and warns when your diff overlaps another queued branch's files (advisory:
expect a semantic rebase if the earlier branch merges first).

## 5. Watch and report

Poll `./scripts/merge-queue.sh status` (JSON) every minute or so. Healthy =
`gate_lock.stage` advancing and `log_age_s` well under the stall limit. Then
react to the journal event for your branch:

- **merged** — done; report the sha and gate duration (`elapsed_s`).
- **red** — the full gate failed. Read the log (path in the event), fix on
  your branch, re-run your shard, resubmit. Do NOT bypass with a direct merge.
- **timeout** — the gate hung or exceeded its limit; the log shows the last
  stage. Usually a flaky/hung test, not your change — inspect, then resubmit.
- **conflict** — your branch doesn't rebase onto current master. Rebase it
  yourself in your worktree, resolve, resubmit.
- **blocked** — gate was GREEN but the fast-forward failed (main worktree not
  on master or dirty). Surface this to the user with the sha from the event;
  the fix is manual: `git merge --ff-only <sha>` in the main worktree.
- **requeued** — master moved mid-gate; no action, the coordinator re-gates
  automatically.

## Ad-hoc heavy runs

If you genuinely need a full suite outside the queue (debugging a red gate),
share the lock so you never overlap another gate:

```sh
./scripts/merge-queue.sh with-lock -- ./scripts/check.sh --fast
```
