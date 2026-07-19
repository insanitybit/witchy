# RFC-0098 acceptance ledger

This ledger is the checked-in completion record for
[RFC-0098](0098-structural-record-width.md). A criterion becomes **DONE** only
after the cited executable evidence is on `master`; a clean worktree or queued
branch is only **READY**.

Status meanings:

- **DONE** — merged behavior with checked-in evidence.
- **READY** — specified and unblocked, but evidence is not yet merged.
- **BROKEN** — current behavior fails the criterion or an attempted
  implementation has exact failing evidence.
- **OBSOLETE** — superseded work that must not be used as an implementation
  parent.

## Branch ledger

| Work | Status | Evidence / disposition |
|---|---|---|
| RFC design on `master` | DONE | `5f54c23c` contains the proposed RFC; implementation tracking still says not started. |
| `impl/rfc0098-syntax` | READY | Syntax/normalization slice is based directly on `master` at `5f54c23c`; focused syntax, RFC-0080 regression, and two-backend RFC-0098 tests are green. No queue dependency. |
| `rfc/0098-structural-record-width` | OBSOLETE | Authoring commit `cf5bc073` is patch-identical to rebased commit `5f54c23c`; its worktree was swept after merge. |

There are no BROKEN RFC-0098 branches. The RFC authoring branch was submitted
without an `--after` dependency, passed the complete merge gate, merged on
2026-07-19, and has no stale queue entry. Current queue failures belong to
unrelated work.

## Acceptance criteria

| # | Status | Required checked-in evidence |
|---:|---|---|
| 1 | BROKEN | Width projection does not exist at annotations, assignments, default/`let`/`own` arguments, returns/tails, typed aggregate slots, or `as`. |
| 2 | BROKEN | No source-located CLI/LSP conformance diagnostics exist for missing or mismatched fields. |
| 3 | READY | `impl/rfc0098-syntax` parses and normalizes `.{..X, c: Int}` to exact anonymous-record identity through generic aliases, field reorderings, ownership qualification, formatting, and compiler-owned type quotes. |
| 4 | READY | `impl/rfc0098-syntax` collapses identical duplicates and rejects conflicting duplicates, cycles, and non-record bases before either backend. |
| 5 | READY | Rejections for nominal records, capabilities, existentials, tuples, unions, and unconstrained variables. |
| 6 | READY | Exact unannotated joins, generic inference, existing containers, function values, and cross-shape equality. |
| 7 | BROKEN | Exact-record rendering/equality exists, but no projected value can prove target-only reflection, JSON, equality, or hashing. |
| 8 | BROKEN | No projection exists to prove exactly-once, source-order evaluation of selected and omitted fields. |
| 9 | BROKEN | General borrow preservation and `own` moves exist, but neither is implemented across record projection. |
| 10 | READY | Invariant `var` rejection at roots, fields, indexes, and nested places with no write-back. |
| 11 | BROKEN | Exact anonymous records have parity coverage; record projection has no interpreter/compiled-Wasm evidence. |
| 12 | BROKEN | Typed reference aggregates exist, but there is no projection lowering or guard against layout relabeling/slot laundering. |
| 13 | READY | Browser-visible optimization counters for shallow/deep projections, or loud `mode opt` rejection. |
| 14 | BROKEN | Formatter, expansion, quoting, hover, and diagnostics do not understand type spread or projection. |
| 15 | BROKEN | The spec/book state the opposite behavior; migration guidance, RFC notes/status, runnable example, and manifest evidence are missing. |

## Landed slices

No implementation slice has landed yet. Add exact commit IDs and test names
here as each independently green slice reaches `master`.
