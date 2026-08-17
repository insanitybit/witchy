# Agile Agent Playbook

This document is the single source of truth for agent-driven contributor workflow.
Its intent is fast, predictable progress without weakening gate guarantees.

Last updated: 2026-08-02

## 1) Standard contribution pipeline

Agent changes should follow the feature pipeline and keep one agent-owned slice to
one boundary whenever possible.

1. **Syntax** (`crates/witchy-syntax`): parser, AST, and formatter edits.
2. **Rewrite/link** (`crates/witchy-syntax`, `crates/witchy-types`):
   normalization and symbol/linkage work.
3. **Diagnostics** (`crates/witchy-types`): explicit error expectations and
   message-level intent.
4. **Backend parity** (`crates/witchy-interp`, `crates/witchy-lower`,
   `src/example_tests.rs`): behavior alignment and observable result parity.
5. **Docs/spec and tests** (`spec/*`, `book/*`, `rfcs/*`, `README.md`): update
   runnable examples and acceptance notes at the same time as behavior changes.

Boundary ownership is explicit in every task handoff:

- `syntax`: `crates/witchy-syntax` only.
- `keyword_args`: add/adjust parser, type parser, and related diagnostics in
  `crates/witchy-syntax` + `crates/witchy-types`.
- `typeck`: `crates/witchy-types` type relation and error surface.
- `lower`: `crates/witchy-lower` lowering and ABI boundaries.
- `interp`: `crates/witchy-interp` behavior implementation.
- `spec`: `spec/*` and runnable docs artifacts.
- `tests`: `src/example_tests.rs`, `tests/*`, and any touched fixture.

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

### RFC-0122 experimental opt-mode policy: Wasm first

While the explicit-reference carrier ABI is changing, RFC-0122 aggregate,
list, and exclusive-reference fixtures are **compiled-Wasm-first**. A focused
fixture must execute through `wasm_run_reowns`; do not hold a lowering slice
for interpreter parity. The interpreter remains the semantic convergence target,
but parity is mandatory only after the carrier contract is stable, and before
an RFC-0122 acceptance row is marked `PROVEN` or experimental opt mode exits.

Every Wasm-first row carries an explicit interpreter debt:

| fixture | missing interpreter boundary | convergence milestone |
| --- | --- | --- |
| `direct_shared_reference_return_preserves_the_runtime_place_on_both_backends` | direct borrowed call/return carrier | `ReferenceKind` call ABI frozen |
| `mutable_to_shared_reference_return_preserves_the_runtime_place_on_both_backends` | mutable-to-shared reborrow carrier | reborrow representation frozen |
| `shared_reference_return_preserves_the_runtime_place_on_both_backends` | direct shared return root | root/provenance ABI frozen |
| `shared_reference_tuple_preserves_each_owner_root_on_both_backends` | tuple construction, copy, destructure, field projection | aggregate carrier ABI frozen |
| `shared_reference_list_preserves_each_owner_root_on_both_backends` | list construction, return, index projection | list carrier ABI frozen |
| `exclusive_reference_list_projection_writes_the_selected_owner_on_both_backends` | indexed exclusive reference write | direct-place write ABI frozen |
| `exclusive_reference_tuple_destructure_writes_the_selected_owner_on_both_backends` | destructured exclusive reference write | aggregate write ABI frozen |

Keep these debts in the RFC-0122 acceptance ledger as `WASM PROVEN /
INTERPRETER DEBT`; replace each with named interpreter-plus-Wasm evidence during
the convergence milestone. No fixture may be weakened or removed to defer that
debt.

## 3) Focused checks by boundary

Use `./scripts/agent-check.sh` for local checks:

- `./scripts/agent-check.sh target --package witchy-syntax`
- `./scripts/agent-check.sh target --package witchy-types`
- `./scripts/agent-check.sh paths <path...>`
- `./scripts/agent-check.sh syntax`
- `./scripts/agent-check.sh link`
- `./scripts/agent-check.sh parity`
- `./scripts/test-for-paths.sh --run <path...>` (for unusual touched sets)

Defaults:

- No command here runs the full gate.
- A focused check is mandatory before queue submission.
- Full checks remain for `merge-queue` gate and coordinator pre-merge reviews.

## 4) Target command behavior

- `agent-check.sh target --package <name> [--filter <cargo-filter>]` runs the
  package test target in isolated agent env:
  `cargo test -p <name>`.
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
- Parity-sensitive backend change with `./scripts/agent-check.sh parity` and a
  matching example test update.

### Rejected

- Changing `crates/witchy-lower` without a corresponding interpreter slice.
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
