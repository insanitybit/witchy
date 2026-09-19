# Agile Agent Playbook

This document is the single source of truth for agent-driven contributor workflow.
Its intent is fast, predictable progress without weakening gate guarantees.

Last updated: 2026-08-20

## 1) Standard contribution pipeline

Agent changes should follow the feature pipeline and keep one agent-owned slice to
one boundary whenever possible.

1. **Syntax** (`crates/witchy-syntax`): parser, AST, and formatter edits.
2. **Rewrite/link** (`crates/witchy-syntax`, `crates/witchy-types`):
   normalization and symbol/linkage work.
3. **Diagnostics** (`crates/witchy-types`): explicit error expectations and
   message-level intent.
4. **Canonical conformance** (`crates/witchy-lower`, `crates/witchy-wir`,
   `src/example_tests.rs`): independent expectation, canonical SafeWIR
   interpretation, and admitted-Wasm execution.
5. **Docs/spec and tests** (`spec/*`, `book/*`, `rfcs/*`, `README.md`): update
   runnable examples and acceptance notes at the same time as behavior changes.

Boundary ownership is explicit in every task handoff:

- `syntax`: `crates/witchy-syntax` only.
- `keyword_args`: add/adjust parser, type parser, and related diagnostics in
  `crates/witchy-syntax` + `crates/witchy-types`.
- `typeck`: `crates/witchy-types` type relation and error surface.
- `lower`: `crates/witchy-lower` lowering and ABI boundaries.
- `canonical-interp`: `crates/witchy-wir` SafeWIR interpreter behavior and
  canonical-runtime acceptance evidence.
- `source-interp`: frozen `crates/witchy-interp` compiler-service compatibility
  only; never a new language-semantics implementation or acceptance oracle.
- `spec`: `spec/*` and runnable docs artifacts.
- `tests`: `src/example_tests.rs`, `tests/*`, and any touched fixture.

### Canonical runtime policy

RFC-0167 supersedes RFC-0135's dual source-runtime policy. Checked canonical
SafeWIR is the sole executable language contract; every semantic
slice carries:

- an independently derived observable result or expected diagnostic;
- a focused canonical SafeWIR interpreter check;
- an admitted unoptimized-Wasm check; and
- heap, authority, provenance, or structural audits required by the boundary.

The source tree walker is frozen for language semantics. Compatibility and
security repairs may preserve its temporary comptime/build responsibilities,
but new syntax and runtime behavior must not be implemented there. Eventually
comptime/build evaluation lowers through the canonical route too; until then,
any source-walker debt is compiler-service debt, not language-semantics debt.

Pure optimizations compare optimized Wasm with admitted unoptimized Wasm, the
canonical interpreter, and the independent expectation. Capability,
confinement, and ABI security remain fail-closed on the canonical route.

## 2) Required artifacts per task

For each task, include:

1. **Error examples / regressions**
   - One short reproducer in issue format or failing test fixture.
2. **Expected diagnostics**
   - Exact messages for any user-facing error path (at least one assertion).
3. **Smoke checks**
   - One focused shard plus one cross-boundary artifact update where needed.
4. **Task handoff**
   - `branch`, `files`, `boundaries`, and `verifying commands`.

## 3) Focused checks by boundary

Use `./scripts/agent-check.sh` for local checks:

- `./scripts/agent-check.sh edit` after committing the exact tip to submit
- `./scripts/agent-check.sh target --package witchy-syntax`
- `./scripts/agent-check.sh target --package witchy-types`
- `./scripts/agent-check.sh paths <path...>`
- `./scripts/agent-check.sh syntax`
- `./scripts/agent-check.sh link`
- `./scripts/agent-check.sh parity`
- `./scripts/test-for-paths.sh --run <path...>` (for unusual touched sets)

When the branch is ready to queue, use the combined command from its own
worktree:

```sh
./scripts/submit-with-presubmit.sh [branch] [note]
```

It runs the exact-tip edit checks and submits that same tip, preserving the
receipt join used to measure and prevent failure-driven rework. Use the
separate `agent-check.sh edit` then `merge-queue.sh submit` commands only when
you need to inspect or stage those steps independently.

When run without arguments from the branch worktree, it infers the current
branch and submits it without a note.

Defaults:

- No command here runs the full gate.
- A focused check is mandatory before queue submission.
- A clean-tree `agent-check.sh edit` records the commit-keyed presubmit receipt.
  Explicit path arguments remain useful while editing but do not certify the
  complete branch diff.
- Full checks remain for `merge-queue` gate and coordinator pre-merge reviews.

## 4) Target command behavior

- `agent-check.sh target --package <name> [--filter <cargo-filter>]` runs the
  package test target in isolated agent env:
  `cargo test -p <name>`.
- In a linked worktree, `agent-check.sh` reuses that worktree's seeded `target/`.
  In the shared main checkout it defaults to `target-codex` so concurrent agents
  do not share Cargo's build lock. An explicit `CARGO_TARGET_DIR` still wins.
- `agent-check.sh paths <path-pattern...>` routes paths through
  `scripts/test-for-paths.sh --run`.
- `agent-check.sh syntax` / `agent-check.sh link` / `agent-check.sh parity`
  are aliases for the most common shard families.
- `just metrics` records build and test-compilation timings; add
  `--with-tests` to include the full test stage.
- `just perf-health` combines the latest local timings with merge-queue
  throughput and gate latency.
- `./scripts/structure-health.sh` reports the largest source files and warns
  before files become difficult to edit safely.
- `./scripts/perf-health.sh --json` is the machine-readable status form for
  dashboards and handoffs.
- `./scripts/metric-compare.sh <before.json> <after.json>` compares build,
  compile, test-compilation, and test-stage speedups between two snapshots.

All `target` checks are invoked under:

- `env -u RUSTC_WRAPPER`
- `CARGO_BUILD_RUSTC_WRAPPER=`
- `CARGO_TARGET_DIR=<agent dir>`

The defaults keep agents out of the shared default `target/` namespace.

## 5) Merge-queue alignment and handoff

Before editing:

1. Capture `git status --short --branch`.
2. Capture `./scripts/merge-queue.sh status`.
3. Confirm branch intent and ownership in-progress updates.

After edits:

1. Run boundary-scoped checks from section 3.
2. Update all affected docs/artifacts for scope-defined files.
3. Add the branch to the queue only when green on the selected shard:
   `./scripts/merge-queue.sh submit <branch>`.

If blocked by gate dependency, start the next disjoint unit immediately instead
of waiting idle. This improves batching and reduces full-gate contention.

## 6) State transitions and handoff format

### Accepted state transitions

`discovered -> scoped -> implemented -> locally-verified -> queued -> merged`

### Rejected state transitions

`implemented -> reviewer_rejected -> corrected` or
`queued -> red -> rework`.

Rejection is only terminal when it has:

- concrete failing check output,
- explicit fix plan,
- and ownership assignment for retry.

### Recommended handoff block

```
## Task handoff
- branch: <branch>
- scope: <syntax|link|typeck|lower|interp|spec|tests>
- touched files:
  - <file1>
  - <file2>
- required checks run:
  - ./scripts/agent-check.sh ...
  - ./scripts/check.sh --fast / --wasm (if listed by scope)
- expected behavior:
  - ...
- blocker notes / follow-ups:
  - ...
```

## 7) Accepted vs rejected examples

### Accepted

- Syntax-only change in `crates/witchy-syntax/src` + `crates/witchy-syntax/Cargo.toml`
  with `./scripts/agent-check.sh syntax` green and a targeted negative test.
- Rewrite/link change in syntax+types with required `target --package witchy-types`
  check and a regression fixture in `src/example_tests.rs` when behavior is visible.
- Stable semantic change with a matching independent expectation, canonical
  SafeWIR interpreter check, admitted-Wasm check, and required boundary audits.

### Rejected

- Changing `crates/witchy-lower` without canonical validation/interpreter and
  admitted-Wasm evidence for the affected semantic boundary.
- Editing multiple ownership boundaries without updating the handoff block.
- Running only docs edits and skipping merge-queue submission while the branch is
  already green.

## 8) Change durability

Handoffs preserve intent in:

- this playbook,
- `rfcs/0113-agent-contributor-velocity.md`,
- branch-scoped diffs and queue journals.

When work is interrupted, keep artifacts and handoff state; never overwrite mixed
or ambiguous edits in place.
