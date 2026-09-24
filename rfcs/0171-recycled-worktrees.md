---
rfc: 0171
title: Recycle agent worktrees so incremental state survives between tasks
status: accepted
created: 2026-09-24
tracking:
---

# RFC-0171: Recycle agent worktrees so incremental state survives between tasks

## Summary

Every agent task starts in a fresh worktree. The first edit there pays a
full, non-incremental compile of the edited crate and everything that
depends on it. After a one-line `typeck.rs` edit, `cargo test --workspace
--no-run` took 256s the first time and 43s the second time in the same
worktree. rustc keys incremental state to the worktree's absolute path, so
the seed cannot carry it, and neither copying nor path remapping transfers
it.

This RFC keeps the path instead. Two changes:

- The merge queue *recycles* a landed branch's clean worktree into a pool
  instead of deleting it.
- The `WorktreeCreate` hook hands out a pooled worktree, on a fresh branch,
  before making a new one.

Agents see no difference: the hook already chooses the path it prints.

## Motivation

Measured on 2026-09-24 on `master` after RFC-0170, through the `mbx` cargo
shim, at load 80-185 on 10 cores:

| Step | First edit in a fresh worktree | Second edit, same worktree |
|---|---|---|
| `cargo test --workspace --no-run` after a typeck edit | 256s | 43s |
| of which `witchy-types` | 109.5s | 3.6s |
| of which `witchy-lower` lib | 130.6s | 3.7s |
| `cargo build -p witchy-types` after a typeck edit | 48s | 2.0s |

The seed is `scripts/worktree-warm.sh`: an APFS clone of the gate's target
without `incremental/`. It makes a no-op build take 0.5s, so every artifact
is already warm. What it cannot provide is incremental state for the crates
an agent then edits. Three attempts to provide it failed:

1. **Clone the gate's `incremental/witchy_types-<hash>`.** The directory name
   is the same across worktrees. The first edit still took 48s.
2. **Build with `--remap-path-prefix=<worktree>=/witchy`** (through
   `cargo rustc`, on witchy-types alone) and move the remapped state to a
   second worktree. The second worktree's no-edit rebuild took 24.7s,
   against 1.8s once it had its own state.
3. **Have the create hook's background prebuild force-compile the workspace
   crates incrementally.** This works, but it adds a full workspace compile
   per worktree, of every crate, on a machine that already runs at load 30
   to 180. It spends more total CPU than it saves.

So the state has to stay where it was made: the same path.

## Design

### The pool

The pool is a set of linked worktrees at
`.claude/worktrees/pool-<NN>`. Each is detached at a landed `master`
commit, has a clean tree, and holds a `target/` with its own incremental
state. `state/worktree-pool/` records one file per free slot, holding the
slot path and the time it was returned. A slot is claimed by an atomic
`mkdir state/worktree-pool/claims/<NN>`.

### Returning a worktree: the sweep

`cmd_sweep` already proves that a worktree is safe to reclaim: its branch is
journaled merged, it is not queued again, and its tree is clean. Today the
sweep deletes it. Instead, the sweep will:

- run `git -C <wt> switch --detach master`;
- delete the branch ref as it does today;
- remove untracked files the queue itself created (`.worktree-prebuild.log`,
  `target/cargo-timings`);
- register the slot as free.

The sweep recycles only worktrees at a `pool-<NN>` path. It never moves a
worktree: `git worktree move` would change the path and discard the very
state the pool exists to keep. Instead, the create hook puts every new
worktree at a pool path from the start, so the pool fills itself.
Worktrees at any other path, such as ones made by hand with `git worktree
add`, are deleted as today. The pool is capped at `WITCHY_WORKTREE_POOL_MAX`
(default 6). Past the cap, the hook creates ordinary named worktrees, and
the sweep deletes those as today.

### Handing a worktree out: the hook

`scripts/worktree-create.sh` gains a first step. If a free slot claims
successfully, it:

- runs `git -C <slot> switch -c <branch> HEAD`, where `<branch>` follows the
  existing `worktree-<name>` rule, including its merged-name avoidance;
- prints the slot path.

Otherwise, if the pool is below its cap, the hook creates and warms a new
worktree at the next free `pool-<NN>` path, which the sweep will later
recycle. If the pool is full, it creates a named worktree, as today.

Branch switching changes the mtimes of the files that differ between the
slot's old `master` and the new `HEAD`. Cargo rebuilds exactly those crates,
and they rebuild incrementally, because the path and its state are unchanged.

### Bounds

- **Disk.** Each slot keeps its `target/`, typically 5-15 GB after
  incremental builds. Six slots is the default. `reclaim-build-caches.sh`
  learns the pool: a slot idle for longer than `--pool-min-age-hours`
  (default 24) is evicted first.
- **Staleness.** A slot detached at an old `master` still hands out a
  correct branch, since the hook branches from the caller's `HEAD`, not the
  slot's commit. The cost is a larger rebuild. Slots older than a day are
  evicted rather than reused.

## Safety

- A slot is only ever a worktree the sweep already proved safe to delete.
  Recycling keeps strictly less risk than deletion has today: a clean tree,
  a merged branch, and no queue entry.
- An agent never shares a slot. The `mkdir` claim is atomic, and the hook
  removes the claim together with the free-slot file.
- The bash cwd trap (`feedback`: a swept worktree's failed `cd` falls back to
  the main checkout) gets milder, not worse: a recycled path still exists.
  The briefing rule remains that an agent does not reuse a worktree after
  submitting its branch.

## Acceptance

- In a recycled worktree, the first `witchy-types` edit rebuilds
  incrementally. Measured the same way as the motivation table, its
  `cargo test --workspace --no-run` stays within 2x of the second-edit
  figure.
- The gate-behaviour tests cover all of the following:
  - a merged, clean worktree is recycled;
  - a dirty or re-queued one is not;
  - the cap deletes the overflow;
  - two concurrent hook runs never claim the same slot.
- `reclaim-build-caches.sh --dry-run` lists pool slots with their age.

## Alternatives

- **Persistent per-agent worktrees chosen by the agent.** This gives the same
  effect, but it changes every agent's workflow and depends on each agent
  following a switch-branch routine correctly. The hook-level pool is
  invisible to agents.
- **Seeding or remapping incremental state.** Measured above; it does not
  transfer.
- **Accept the cost.** Each fresh-worktree task pays roughly 50-250s on its
  first edit, depending on which crates it touches.

## Drawbacks

- Pool slots hold disk that deletion used to free. The cap and age eviction
  bound it, but free space on this machine is often under 50 GB.
- The sweep and the create hook become stateful. A bug there can hand an
  agent a dirty tree, so the hook re-checks `git status` on the slot before
  printing it, and falls back to a fresh worktree if the slot is not clean.

## Implementation

Landed with this status change:

- `scripts/worktree-pool.sh` holds the slot protocol (claim, release, evict,
  reserve). State lives under `state/worktree-pool/{free,claims}`.
- `scripts/worktree-create.sh` claims a free slot first, then creates new
  worktrees at `pool-<NN>` while under `WITCHY_WORKTREE_POOL_MAX` (default 6;
  `0` turns the pool off). A slot that is stale (older than
  `WITCHY_WORKTREE_POOL_MAX_AGE_HOURS`, default 24), has no `target/debug`,
  or is not a clean detached checkout is evicted, not handed out. The name
  `pool-<NN>` is reserved.
- `merge-queue.sh sweep` recycles a merged, clean, unqueued pool worktree in
  place and journals it as `swept` with `recycled: 1`. Anything else is
  removed as before.
- `reclaim-build-caches.sh` lists a free slot as `POOL:<age>h` and keeps its
  cache until `--pool-min-age-hours` (24). After that, the cache is reclaimable,
  and the hook evicts the cacheless slot.
- Tests: `tools/merge-queue-tests` `worktree::create` (recycle, cap, dirty
  eviction, concurrent claims) and `merge_queue::coordinator`
  `sweep_recycles_a_landed_pool_worktree_in_place`.

The status moves to `implemented` once the timing acceptance criterion, the
first edit in a recycled slot within 2x of the second edit, is measured on a
recycled slot.
