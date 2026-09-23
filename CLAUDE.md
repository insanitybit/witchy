# Working in this repo (agent notes)

**The baseline briefing is imported below and is always in context — read it, and
do not go exploring to reconstruct what it already tells you.** It covers the
witchy language essentials, the crate layout and code-organization rules, the
build/test traps, and the merge-queue protocol.

@.claude/skills/witchy-dev/SKILL.md

**CRITICAL REMINDERS**:
- **Always use isolated worktrees** (`.claude/worktrees/<name>`). **Never** edit the shared `master` checkout.
- **Language contract versus implementation**. Start design work at `rfcs/0167-witchy-language-contract.md` and its acceptance ledger. It is a proposed target, not proof that the compiler implements every form. Check current source/tests for executable behavior; retired RFCs are indexed in `rfcs/retired-rfcs.md`.

Everything below is the **detail behind that briefing** — the exact paths, RFC
numbers, and hard-won specifics that the briefing states as rules. It doesn't
repeat the rules; consult it when you're working inside one of these areas.

## Gotchas the briefing compresses

- **Worktree seeding.** The `WorktreeCreate` hook (`.claude/settings.json`) runs
  `scripts/worktree-create.sh`, which creates the worktree AND seeds it. That
  hook REPLACES the built-in creation — it must print the new worktree's path as
  its **only** stdout; everything else goes to stderr. When your worktree's work
  is merged, remove the worktree (its multi-GB `target/` goes with it). What
  gets left behind is abandoned checkouts, not merged ones (569 registered on
  2026-09-19, 362 untouched for two weeks): `scripts/worktree-status.sh
  --prune-stale --dry-run` lists them and `--prune-stale` removes them,
  WIP-committing a dirty tree first so the branch keeps every edit.
- **Reclaiming disk: use `scripts/reclaim-build-caches.sh`, not `du | sort | rm`.**
  It is a dry run by default. Two traps it already knows: a worktree's plain
  `target` is an APFS **CoW clone** of the origin, so `du` reports its full size
  while deleting it frees almost nothing (34GB of `du` once returned 1GB of
  `df`) -- the freshly built `target-<agent>` dirs are the ones that pay; and a
  cache with an open file handle is a **live build** belonging to another agent.
  It also skips the `merge-gate` worktree, whose prewarm caches every queued
  branch depends on. It covers two classes: `target*` dirs in registered
  worktrees (in the repo or under `/private/tmp/witchy-*`), judged by branch
  state, and standalone `/private/tmp/witchy-*` caches (a `CACHEDIR.TAG`, no
  `.git`, no worktree) that nothing can vouch for, reclaimable only after
  `--orphan-min-age-hours` (48) with no open handle. Judge reclaim by `df`,
  never `du`.
- **The stale-binary check.** If results look stale, compare `which witchy`
  against your build output path.
- **One build profile for everything.** `dev` and `test` both set
  `strip = "symbols"` in Cargo.toml, so `cargo build`, `cargo test`, nextest,
  `agent-check` and the gate all reuse the same compiled crates. Do not
  set `CARGO_PROFILE_*` overrides casually: any difference in profile settings
  (strip, opt-level, debug) forks a second full build of every crate AND
  dependency in the same target dir. For symbolized backtraces, knowingly pay
  for that second build with `CARGO_PROFILE_DEV_STRIP=none CARGO_PROFILE_TEST_STRIP=none`.
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
never block (see `CLIPPY_GATE_LINTS` in the script). Do not run the full gate
yourself. After committing the exact tip you intend to submit, run
`./scripts/agent-check.sh edit`; it selects focused checks from the committed
diff and records the commit-keyed receipt consumed by the merge queue. The
coordinator owns the serialized full gate. CI (`.github/workflows/ci.yml`) runs
build/clippy/nextest, canonical-conformance sweep, heap-check fuzz, e2e, docs, and fmt. For
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

- **The compiler is the priority (decided 2026-09-21).** glamour, coven,
  coven-web and grimoire are consumers of witchy and are not being supported
  right now. The book and its glamour-built docs site are deprioritized with
  them until the compiler work is done: do not spend effort on book chapters,
  runnable-book validation or the docs site unless asked. RFC-0168 plans the
  split that takes these out of the compiler. The merge gate neither builds nor runs their tests (the
  `glamour-tests` / `coven-tests` features are off by default; `./scripts/check.sh
  --glamour` / `--grimoire` run them on demand). Do not spend effort keeping
  them green unless asked.

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

## Per-agent target dirs and the build cache

```sh
CARGO_TARGET_DIR=target-claude cargo nextest run --workspace
CARGO_TARGET_DIR=target-codex cargo test --workspace
CARGO_TARGET_DIR=target-codex cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use
```

Compiler outputs are shared across worktrees and target dirs by the
**mr-boxington (`mbx`) build cache**. `scripts/mbx-install.sh` (run by the
SessionStart hook) installs the pinned binary under `state/mbx/`;
`scripts/bin/cargo` is a `cargo` shim that routes build subcommands through it
with the store at `state/mbx/store-<version>`; the checked-in `.mbx.toml`
carries the build policy (workspace-root remapping, so a crate rebuilt in one
worktree still hits in the others). `agent-check.sh` and the
scripts it drives already use the shim; for ad hoc commands put
`scripts/bin` first on `PATH` or call `./scripts/bin/cargo build ...`. The
gate (`WITCHY_GATE_SCOPE`) always uses plain cargo. `WITCHY_BUILD_CACHE=off`
is the kill switch. Workspace crates are cached and shared across worktrees
too; mbx learns which crate you are editing and compiles that one
incrementally (`WITCHY_MBX_MODE=incremental` restores the old
every-member-incremental policy, which shared only third-party crates). Every shim build leaves a small JSON report
under `state/mbx/reports/`; `scripts/mbx-report.py --since 24h --by worktree`
turns them into hit rates and compiler time avoided, which is how to answer
"is the cache helping" with numbers. Never run `mbx setup`: it rewrites machine-wide
Cargo, mise, and rust-analyzer configuration for every agent. Measurements and
the two reproducibility fixes it depends on:
`scratch/build-cache-eval-2026-09-17.md`.

The SafeWIR checker keeps a **verdict memo** as well: admitted per-function
verdicts are memoized in-process, and every cargo/nextest-launched process
(tests and the `witchy` binaries they spawn) also persists the expensive ones
under `state/safewir-check-cache/` via `[env]` in `.cargo/config.toml`, so the
std async executor is checked once per checkout instead of once per test
process. A shell `witchy` run is not opted in. `WITCHY_SAFEWIR_CHECK_CACHE=off`
disables memoization; the store is keyed under a digest of `witchy-wir`'s
sources, so an edited checker never replays old verdicts. Design and trust
boundary: `crates/witchy-wir/src/canonical_safewir/check/cache.rs`.

`scripts/bin/witchy` is the same idea for ad-hoc compiler runs: the store is
opt-in, and a bare shell `witchy` is deliberately left out, but that taxes the
loop you actually wait on. Measured on a five-line async program, twice each:
**29.3s CPU without the store, 8.6s with it** -- anything touching async
re-checks the std executor from scratch otherwise. Using the shim IS the
opt-in, exactly as `scripts/bin/cargo` is for the build cache; the first run
pays to populate the store and every later one collects. A `witchy` invoked
from outside the repo keeps the untouched default.

`./scripts/check.sh` in the shared `target/` tree is still authoritative, but
coordinate before running it there. While another agent is active, prefer
`CARGO_TARGET_DIR=target-<agent> ./scripts/check.sh --fast`.

## Measuring the compiler

`scripts/compile-bench.sh` is the one way to time `witchy` itself, the way
`scripts/gate-bench.sh` is for gate wall time. It runs a deliberate SPREAD of
workloads because a single benchmark misleads: a 2026-09-20/21 perf session
tuned six changes against the async boundary test (an 11k-block generated
executor, 10-15% typeck) before one profile of a real 1.8k-line program showed
85% typeck and ~70% allocator traffic. The next change, found only there, was
worth 17% -- more than everything the async benchmark had produced.

    ./scripts/compile-bench.sh                  # baseline, all workloads
    ./scripts/compile-bench.sh --against master # alternating A/B vs a ref

It reports CPU seconds (robust to this box's 10x load swings), takes the best
of N, alternates against the ref IN ONE WINDOW, prints the load with the
result, and REFUSES an A/B on a dirty tree -- the swap would silently destroy
the uncommitted change and measure a ref against itself. Read its header
before hand-rolling a measurement; every trap in it was hit in practice.

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
