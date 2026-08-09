# Working in this repo (agent notes)

**The baseline briefing is imported below and is always in context — read it, and
do not go exploring to reconstruct what it already tells you.** It covers the
witchy language essentials, the crate layout and code-organization rules, the
build/test traps, and the merge-queue protocol.

@.claude/skills/witchy-dev/SKILL.md

Everything below is the **detail behind that briefing** — the exact paths, RFC
numbers, and hard-won specifics that the briefing states as rules. It doesn't
repeat the rules; consult it when you're working inside one of these areas.

## Gotchas the briefing compresses

- **Worktree seeding.** The `WorktreeCreate` hook (`.claude/settings.json`) runs
  `scripts/worktree-create.sh`, which creates the worktree AND seeds it. That
  hook REPLACES the built-in creation — it must print the new worktree's path as
  its **only** stdout; everything else goes to stderr. When your worktree's work
  is merged, remove the worktree (its multi-GB `target/` goes with it).
- **The stale-binary check.** If results look stale, compare `which witchy`
  against your build output path.
- **`spec/stdlib.md` drift** is caught by the `stdlib_docs_are_current` test.
- **`witchy-runtime`'s hidden modules** are `runtime.rs` +
  `runtime/{compiler.rs,host/**}`, behind the crate's non-default `native`
  feature, which only `witchy`'s default features activate.
- **Stale-doc example:** the old `derive(Json)` never existed (use
  `derive(Reflect)` + `json.stringify`). That's how a wrong *prose* claim slips
  past a build that only executes code blocks.
- **RFC status lifecycle** for `rfcs/NNNN-slug.md` is defined by RFC-0001.

## The gate in detail

`./scripts/check.sh` = build + clippy + `nextest --workspace` + the wasm build.
Clippy runs **bug-lint tiers only** — `correctness` + `suspicious` +
`unused_must_use`, **not** `-D warnings`; style/perf/pedantic lints print but
never block (see `CLIPPY_GATE_LINTS` in the script). Run it before every commit
and `--full` before a push. CI (`.github/workflows/ci.yml`) runs
build/clippy/nextest, heap-check fuzz, parity sweep, e2e, docs, and fmt. For
anything user-visible, add a runnable `book/` example alongside the differential
test.

## Why the optimization zoo stays (RFC-0016 / RFC-0051)

The per-method fast paths you must not *add to* are the `*_cap` runtime helpers
(`dict_insert_cap`, `list_push_cap`, …) and the `self_*` recognizers in
`crates/witchy-lower/src/analysis.rs` (`self_insert_args`, `self_set_at`, …).
The failure mode is concrete: `dict.remove` leaked precisely because it had no
`dict_remove_cap`. The ownership conventions already express the fact a fast
path re-encodes (a unique `var` may be mutated/reclaimed in place; a `let` may
not escape) — consume that fact uniformly. The family is nonetheless **retained,
not deleted**: RFC-0051 (I3) measured removing it and found the general path
perf-negative — it OOM-traps several benchmarks. RFC-0016 is the general
reclamation floor.

## Trait-method dispatch internals (RFC-0046)

The receiver's concrete type comes from `self.table.type_of(&args[0])` in
`crates/witchy-types/src/traits/mono.rs`, with the head name extracted by
`nominal_type_name` in `crates/witchy-types/src/traits.rs`. RFC-0046 deleted the
string "shadow type system" (`recover_generic_call`, `bind_type_var`,
`builtin_ret`): call results (`list.at(xs,i)`, `xs[i]`, generic returns) are
typed by the checker and the annotate/mono **fixpoint** (`lower_with`), which
re-annotates after each round so a generic helper's bounded call
(`iter.collect`) resolves once the helper is specialized. A fix belongs in the
typed path — a `call_sig` entry, a signature — so the table carries it.

The **documented residual**: the empty-table quiet pre-mono pass still uses
`nominal_type_name` for local judgment (literals/ctors/params) and
`cap_op_result_type` for chained cap-op results (bare intrinsics the checker
types but the empty table can't surface). That residual is not an invitation to
grow the shape tables.

## Work selection

- For 0.1 release work, [`RELEASE-READINESS.md`](RELEASE-READINESS.md) is the
  tracked evidence ledger; recheck its claims against current `master`,
  `./scripts/merge-queue.sh status`, and the exact candidate gate before
  reporting readiness. The gitignored `scratch/RELEASE-QUEUE.md` is a historical
  2026-07-09 worklist, not a current ownership or priority source — its
  reproducers remain useful, but do not restart completed tiers from it.
- **For agent process, boundaries, and handoff format**, use
  [`docs/agile-agent-playbook.md`](docs/agile-agent-playbook.md) before editing
  and before queue submission.
- **Do not revive stale RFC-0005 stage branches.** RFC-0005 is implemented on
  `master`; new representation defects need a current repro and a fresh branch,
  not continuation from `impl/rfc-0005-stage2` or another historical worktree.

### Large-scope delivery

Optimize large RFCs and cross-cutting projects for total completion latency.
Before implementation, convert acceptance criteria into a dependency graph and
live ledger, freeze the narrow contracts shared by independent tracks, and start
those tracks immediately in isolated worktrees. Keep one integration track
runnable, validate each track continuously, and update generated evidence with
the change that invalidates it. Report the critical path and unmet acceptance
criteria, not commit or line counts. Decomposition enables concurrency and
verification; it does not reduce scope. The merge queue serializes landing, not
implementation. If independent work is becoming serial, change the execution
plan and report the bottleneck.

## Per-agent target dirs

```sh
CARGO_TARGET_DIR=target-claude cargo nextest run --workspace
CARGO_TARGET_DIR=target-codex cargo test --workspace
CARGO_TARGET_DIR=target-codex cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use
```

`./scripts/check.sh` in the shared `target/` tree is still authoritative, but
coordinate before running it there. While another agent is active, prefer
`CARGO_TARGET_DIR=target-<agent> ./scripts/check.sh --fast`.

## Merge queue: operational specifics

**Full operator's guide: `scripts/MERGE-QUEUE.md`** — architecture, command
reference, invariants, sharp edges, testing recipe, and recovery playbook. Read
it before debugging or extending the queue.

- Why the two invariants exist: the long-tail e2e tests stretch each other, and
  the publish e2e is load-flaky.
- `--fast` overlaps clippy in the background, so a lint failure surfaces at the
  collect stage *after* tests.
- A `submit --after <parent>` change reports as waiting/blocked until the stable
  parent lands; the queue can gate a ready dependency stack at its tip once.
- `run` rebases in the dedicated warm worktree `.claude/worktrees/merge-gate`.
- Queue, journal (`journal.jsonl`), gate logs, and lock live under gitignored
  `state/merge-queue/`. `scratch/merge-queue` is a legacy symlink after
  migration, not a second source of truth.
- Optional local agent diagnostics and handoff notes belong under
  `state/agents/`; they are observational only and must **not** be treated as
  file ownership or a lock.
