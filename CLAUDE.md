# Working in this repo (agent notes)

`witchy` is a capability-secure language with twin backends: the interpreter
(`crates/witchy-interp/src/interpreter.rs`) is the reference, the compiled-WASM
path (`crates/witchy-lower/src/codegen/`) must match it. **Authoritative docs:**
`CONTRIBUTING.md` (build/test/parity) and `spec/architecture.md` (pipeline +
workspace layout). Read those first; this file only adds the things that have
actually bitten agents.

**The compiler is a Cargo workspace** (RFC-0018): seven stage crates under
`crates/` (`witchy-syntax`/`-types`/`-wir`/`-lower`/`-runtime`/`-interp`/`-caps`)
plus the `witchy` binary. Build/lint/test the whole thing with `--workspace`
(`cargo nextest run --workspace`, `cargo clippy --workspace --all-targets`). The
root lib re-exports every crate's modules, so `crate::{ast,typeck,codegen,…}::…`
paths still resolve from the binary — but new code belongs in the owning crate.

**Where write-ups go:** **bug reports → `bugs/`** (a gitignored local backlog — one
`BUG-NNN-slug.md` per defect: symptom, repro, root cause, fix). **Design decisions and
proposals → `rfcs/`** (tracked, numbered `NNNN-slug.md`, status lifecycle per RFC-0001).
Don't file a bug as an RFC or an ad-hoc design doc as a bug; security findings still go to
`security-eval/` (SEC-NNN, also gitignored).

## Gotchas

- **The `witchy` on your PATH is the RELEASE binary; `cargo build` produces
  DEBUG.** `~/.cargo/bin/witchy` symlinks to `target/release/witchy`, but
  `cargo build` writes `target/debug/witchy`. So editing `src/`, running
  `cargo build`, then testing with `witchy foo.witchy` runs your *old* code and
  looks like your change did nothing. Either run `./target/debug/witchy …`, or
  `cargo build --release` to refresh the PATH binary. (Check with
  `which witchy` vs. your build output path if results look stale.)
- **Fresh worktree? Seed its build cache before building.** Agent worktrees get
  this automatically: the `WorktreeCreate` hook (`.claude/settings.json`) runs
  `scripts/worktree-create.sh`, which creates the worktree AND seeds it. (That
  hook REPLACES the built-in creation — it must print the new worktree's path as
  its only stdout; everything else goes to stderr.) For a manual
  `git worktree add`, run `./scripts/worktree-warm.sh <worktree-path>` yourself:
  it APFS-CoW-clones the main tree's `target/` (seconds, ~zero disk), so all
  dependency crates (wasmtime included) come up warm and only the 8 workspace
  crates rebuild. Don't point `CARGO_TARGET_DIR` at a shared dir instead —
  cargo's build lock would serialize concurrent agents. When your worktree's
  work is merged, remove the worktree (its multi-GB `target/` goes with it).
- **`spec/stdlib.md` is GENERATED — never hand-edit it.** It is rendered from
  the doc-comments in `std/*.witchy`; a test (`stdlib_docs_are_current`)
  fails if it drifts. Edit the `std/*.witchy` comment, then regenerate:
  `witchy doc std/*.witchy > spec/stdlib.md`.
- **Never `cargo fmt`.** The Rust here is HAND-FORMATTED on purpose — `cargo fmt`
  reformats ~70 files. The only formatting gate is `witchy fmt` over `std/*.witchy`
  + `examples/*/src/*.witchy` + `projects/**/src/*.witchy`.
- **`book/` and `README`/`spec` ` ```witchy ` blocks are executed tests.** A
  fenced `witchy` example must be a complete, correct program (it is parsed,
  type-checked, and run on both backends). Use an untagged or ` ```sh ` fence
  for partial snippets. Don't document an API that doesn't exist — the build
  won't catch a *prose* claim, but it's how stale docs like the old
  `derive(Json)` (which never existed; use `derive(Reflect)` + `json.stringify`)
  slip in.

## The one rule: parity

Two backends, **zero silent divergence**. Any observable behavior must work
(or loudly error) identically on both. Add a differential test in
`src/example_tests.rs` and, for anything user-visible, a runnable `book/`
example. CI exists (`.github/workflows/ci.yml`: build/clippy/nextest, heap-check
fuzz, parity sweep, e2e, docs, fmt), but the local green gate is
`./scripts/check.sh` (build + clippy `-D warnings` + `nextest --workspace` + the
wasm build) — run it before every commit, and `--full` before a push.

## Optimizations generalize — never special-case a method

Memory / in-place / reclamation optimizations are **driven by the ownership
conventions** (`let`/`var`/`own`, and `frozen`/`unique`) plus the escape/uniqueness
analysis — **one general mechanism for ALL operations.** Do **not** add a
per-method fast path: no new `*_cap` runtime helpers (`dict_insert_cap`,
`list_push_cap`, …) and no new per-method `self_*` recognizers in
`crates/witchy-lower/src/analysis.rs` (`self_insert_args`, `self_set_at`, …). A
per-operation hack does not generalize — every new method would need its own code,
and the ones that don't get it silently regress (that is exactly why `dict.remove`
leaked: it had no `dict_remove_cap`). The conventions already express the ownership
fact (a unique `var` may be mutated/reclaimed in place; a `let` may not escape);
consume that fact uniformly. The existing `*_cap` + `self_*` family is **retained,
not deleted**: RFC-0051 (I3) measured removing it and found the general path
perf-negative — it OOM-traps several benchmarks — so the family is load-bearing.
The forward rule still stands: add **no new** per-method fast paths; the general
mechanism must absorb every *new* operation. (RFC-0016 is the general reclamation
floor; RFC-0051 is why the existing zoo stays.)

## Trait-method dispatch (RFC-0046: typed, table-first)

`show(x)` / `less(x, y)` etc. resolve by reading the receiver's concrete type
from **typeck's `TypeTable`** — the real inference judgment, not a string guess
(`Ctx::type_name` / `Mono::type_name` in `crates/witchy-types/src/traits.rs`,
`table_scope_name` first). RFC-0046 deleted the string "shadow type system"
(`recover_generic_call`, `bind_type_var`, `builtin_ret`): call results
(`list.at(xs,i)`, `xs[i]`, generic returns) are typed by the checker and the
annotate/mono **fixpoint** (`lower_with`), which re-annotates after each round so
a generic helper's bounded call (`iter.collect`) resolves once the helper is
specialized. **A fresh dispatch fix belongs in the typed path** — make the
checker type the expression (a `call_sig` entry, a signature), so the table
carries it — never in a new string-shape table. If a trait call still won't
resolve, the type error guides you (`${x}` / `say` / a typed param). The empty-
table **quiet pre-mono pass** still uses `head_type_name` for local judgment
(literals/ctors/params) and `cap_op_return_type` for chained cap-op results
(bare intrinsics the checker types but the empty table can't surface); those are
the documented residual, not an invitation to grow the shape tables.

## Work selection (release push)

- **Work `scratch/RELEASE-QUEUE.md` top-down.** It is the 0.1 blocking set
  (RFC-0070) as an ordered queue with verify commands and done criteria. Do
  not self-select bugs from `bugs/README.md` outside it without asking — the
  ledger does not encode priority; the queue does. Its §CLOSE-ONLY section
  lists rows already verified fixed: close them, don't re-fix them.
- **Do not touch `impl/rfc-0005-stage2` or any externref work** — main-loop-led
  (RFC-0070 D1).
- **Fix the generator, not the output.** A mistake made twice by an agent is a
  prompt/docs bug: fix it with a line here, in the relevant RFC, or in the
  queue entry — never by only hand-patching the latest instance.
- **Adversarial review is selective.** Before submitting a branch that touches
  parity-sensitive contracts (typeck, codegen, interpreter, analysis.rs, the
  wasm ABI), have a fresh-context reviewer read ONLY the diff and try to
  reject it: every hunk must trace to the stated task, and green tests are not
  proof for changes the differential suite doesn't adjudicate. Skip this
  ceremony for changes the existing suites already adjudicate (docs, std
  functions with differential tests, ledger closes).

## Concurrent agents

This checkout is often shared by multiple coding agents at once. Treat the
worktree and `target/` as shared state, not scratch space.

- Run `git status --short --branch` before editing and again before you claim
  the repo is clean. If files changed while you were working, assume another
  agent or the user made those changes and work with them.
- State which files you are editing in progress updates. If another agent edits
  the same file or hunk, stop and ask for coordination instead of overwriting it.
- Never revert, delete, or format changes you did not make. In particular, do
  not use `git checkout --`, `git reset --hard`, `cargo fmt`, or `rm -rf target`
  as a cleanup shortcut.
- Avoid sharing Cargo's default `target/` for long checks. Use a per-agent
  target dir when running build, clippy, tests, or nextest concurrently:

```sh
CARGO_TARGET_DIR=target-claude cargo nextest run --workspace
CARGO_TARGET_DIR=target-codex cargo test --workspace
CARGO_TARGET_DIR=target-codex cargo clippy --workspace --all-targets -- -D warnings
```

  The normal `./scripts/check.sh` gate is still authoritative, but coordinate
  before running it in the shared `target/` tree. If you need to run it while
  another agent is active, prefer `CARGO_TARGET_DIR=target-<agent> ./scripts/check.sh --fast`.
- Do not kill Cargo, nextest, dev-server, or browser processes unless you
  started them or the user explicitly asks. Check process ownership first.
- Clean up only artifacts you created (`target-codex/`, temp reports, local
  logs). Do not delete another agent's target dir or generated output.

### Merging: the gate coordinator (`scripts/merge-queue.sh`)

**Full operator's guide: `scripts/MERGE-QUEUE.md`** — architecture, command
reference, invariants, sharp edges, testing recipe, and recovery playbook.
Read it before debugging or extending the queue. Summary protocol:

Never run two full gates at once (the long-tail e2e tests stretch each other
and the publish e2e is load-flaky), and never merge to master while a full gate
is running (it invalidates that gate). The coordinator enforces both:

- **In your worktree, run only a focused shard**: `./scripts/check.sh --fast`
  (tests minus e2e, with clippy overlapped in the background — a lint failure
  surfaces at the collect stage after tests), or one of `--e2e` / `--examples` / `--wasm`
  for the section your change touches. A green shard qualifies you for the
  queue; it does not replace the full gate.
- **When your branch is ready**: `./scripts/merge-queue.sh submit <branch>`.
  One coordinator session runs `./scripts/merge-queue.sh run`; it rebases each
  candidate onto latest master in a dedicated warm worktree
  (`.claude/worktrees/merge-gate`), runs the full gate under a lock, and
  fast-forwards master on green. If master moves mid-gate, it re-rebases and
  re-gates instead of merging a stale validation.
- **Any ad-hoc heavyweight suite** (a manual full `check.sh`, `--full`, e2e)
  should share the same lock: `./scripts/merge-queue.sh with-lock -- <cmd>`.
- Queue, journal (`journal.jsonl`), gate logs, and lock all live under
  gitignored `state/merge-queue/`; `./scripts/merge-queue.sh status` prints
  the machine-readable state. `scratch/merge-queue` is retained as a legacy
  symlink after migration, not as a second source of truth. Optional local
  agent diagnostics and handoff notes belong under `state/agents/`; they are
  observational only and must not be treated as file ownership or a lock.
