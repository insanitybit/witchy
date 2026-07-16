# RFC-0087 / RFC-0088 current-truth ledger

Audit date: 2026-07-16

Baseline: local canonical `master` at `f78b80c1`

Statuses:

- **PROVEN** — direct current executable or structural evidence exists.
- **MISSING** — the required durable evidence or implementation does not exist.
- **FAILING** — current behavior contradicts the accepted rule.
- **EXTERNALLY OWNED** — closing the gap requires a file currently modified in
  another active worktree; direct current evidence records the boundary.

## Ownership exclusions observed before editing

| Owner/worktree | Current files relevant to this audit |
| --- | --- |
| `impl/rfc0081-existential-frontend` | `crates/witchy-types/src/typeck.rs`, compiled lowering/codegen, parser/linker/type representation, `spec/language.md` |
| `impl/rfc0050-container-method-surface` | `src/example_tests.rs`, `std/list.witchy`, `std/dict.witchy`, `std/set.witchy` |
| gate-liveness worktrees | merge-queue/nextest gate behavior; explicitly outside this goal |
| RFC-0080 worktrees | metaprogramming frontend and docs; explicitly outside this goal |

## RFC-0087 acceptance criteria

| # | Status | Current evidence / gap |
| ---: | --- | --- |
| 1 | **PROVEN** | Current-master `cargo test --workspace rfc0087` passes the five type-checker tests accepting arbitrary `var` positions and result shapes. |
| 2 | **PROVEN** | Current-master RFC-0087 tests and the dedicated matrix cover tail, explicit `return`, callee `?`, success/error multi-`var` envelopes, and the Result/Option receiver classification edge on both backends. |
| 3 | **EXTERNALLY OWNED** | Immutable, temporary, `move`, default, async/gen, and overlap rejection all execute. Exact current diagnostics are pinned and contain no ABI markers. The RFC-required immutable/temporary wording still does not name the resolved parameter; improving it requires actively owned `typeck.rs`/parser surfaces. |
| 4 | **PROVEN** | Dedicated and existing differential tests cover nested field/list/dict places, evaluate computed coordinates exactly once, and apply captured projections to the post-RHS current root. |
| 5 | **PROVEN** | Existing and dedicated differential tests cover call, operator, tuple/list, comprehension/filter, interpolation, short-circuiting, `??`, match, if, method receiver, and assignment order. |
| 6 | **PROVEN** | Dedicated method/free `List.pop` cases and existing user/std tests produce identical results and write-backs on both backends. |
| 7 | **EXTERNALLY OWNED** | Typed trait dispatch with `var self` passes on both backends and convention mismatch rejects. Evidence that the diagnostic points to both trait and impl declarations is missing and requires the actively owned type-checker diagnostic path. |
| 8 | **PROVEN** | Named functions, lambdas, indirect calls, nested indirect places, and convention type identity execute/reject as specified on both backends. |
| 9 | **PROVEN** | Async/gen `var` parameters reject; synchronous `var` calls work on mutable locals across the shipped async segment seam and between generator yields on both backends. |
| 10 | **PROVEN** | Bare resolved-`var` calls discard auxiliary results and commit; non-`var` non-`Nil` calls reject; explicit discard remains legal. |
| 11 | **PROVEN** | Current differential coverage includes zero/one/multiple `var`s, generic and alias-equal returns, same-typed auxiliary results, caller/callee `?`, and `??`. |
| 12 | **MISSING** | Current master lacks the durable report. The isolated branch `feat/rfc0087-migration-census` produces and snapshot-tests a type-resolved census over 271 Witchy files and 169 README/spec/book blocks: mechanical self-reassignment, immutable `var` arguments, and temporary `var` arguments are all zero; it is not counted as current-master evidence until landed. |
| 13 | **PROVEN** | Current `spec/language.md` states the convention, exclusivity, function-type identity, and evaluation order; generated stdlib freshness is enforced. Further wording edits are currently owned by RFC-0081. |
| 14 | **PROVEN** | RFC-0088 stats/differential tests provide extraction aliasing, refcount, heap, one-search, forced-copy, and no-copy evidence without adding a new `*_cap` family. |
| 15 | **PROVEN** | Current master carries the seven-kernel optimized-versus-`WITCHY_OPT=-inplace` harness, checked-in reference, and `rfcs/0087-performance-report.md`. Locked best-of-three runs prove all four optimized memory-cliff kernels complete while forced-copy traps, and measure 2.669×/1.385×/1.333× firing ratios for `list_index`/`binary_trees`/`expr_eval`; a separate default-policy reference check passes within RFC-0051's 5% threshold. The evidence landed through the full merge gate in batch commit `5974a032`. |
| 16 | **MISSING** | Current master still has review-derived counts. Branch `feat/rfc0087-migration-census` adds the compiler/type-resolved stable census and freshness test, but it is not current-master evidence until landed. |
| 17 | **MISSING** | Branch `docs/rfc0087-evidence-status` reopens both RFC statuses, corrects the RFC-0070/RFC-0026 release claims, and makes RFC-0087 the explicit sole home for the RFC-0088 semantic amendments. It intentionally remains unlanded until the ledger it links and the underlying evidence slices are available. |

## RFC-0088 amendment disposition

| Rule | Status | Current truth |
| --- | --- | --- |
| Commit on callee `?` | **PROVEN** | RFC-0087 is the normative home and both backends commit partial progress identically. RFC-0088 correctly delegates source semantics to RFC-0087. |
| Captured assignment projections use current-root bounds behavior | **PROVEN** | `stale_assignment_projection_uses_ordinary_bounds_behavior` proves exact interpreter/Wasm agreement when RHS write-back invalidates a captured list index, and also proves a nested store preserves structure appended to the same root during RHS evaluation. |
| No duplicated source-semantic home | **PROVEN** | RFC-0088 states RFC-0087 is the sole source contract and confines itself to ownership-aware extraction. |
| Future view lifetime work stays with its owner | **PROVEN** | RFC-0088 references RFC-0083 loan facts without redefining the view-lifetime model. |
| Implementation status waits for all release gates | **FAILING** | Both RFCs are marked implemented before the compiler census and docs/status audit are landed. |

## Resolved semantic differential

Expected rule:

> Assignment captures destination coordinates once, evaluates the RHS, then
> applies those coordinates to the current root. If RHS write-back invalidates
> the projection, the final store follows ordinary bounds behavior.

Observed before this slice:

- Interpreter: `Ok(["unreachable"])`
- Compiled Wasm:
  ``Err("runtime error: `main`, line 8: list index 0 out of bounds (length 0)")``

Root cause:

- Parser place lowering represents the assignment as a root reassignment whose
  private `__set_at` receiver is evaluated before the RHS.
- The interpreter therefore retains the pre-RHS aggregate value, applies the
  captured index to that snapshot, and overwrites the current root after
  `shrink(values)` commits.
- Compiled lowering instead carries a place plan: it evaluates coordinates
  once, evaluates the RHS, reloads the current root, and performs the final
  bounds-checked store.
- The interpreter now recovers only the parser-generated nested place spine,
  evaluates every coordinate once in source order, evaluates the replacement,
  reloads the current root, and performs the final store through the shared
  captured-place machinery.
- Existing safe in-place assignment paths run first, so ordinary guarded
  `dict.insert` accumulation still uses the RFC-0051 fast path.
- `stale_assignment_projection_uses_ordinary_bounds_behavior` now passes on
  both backends with the exact source-facing list-bounds diagnostic. Its nested
  companion proves that post-RHS additions to the same root are retained.
