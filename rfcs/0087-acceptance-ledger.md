# RFC-0087 / RFC-0088 current-truth ledger

Audit date: 2026-07-16

Baseline: local canonical `master` at `3d48c580`

Statuses:

- **PROVEN** — direct current executable or structural evidence exists.
- **MISSING** — the required durable evidence or implementation does not exist.
- **FAILING** — current behavior contradicts the accepted rule.
- **EXTERNALLY OWNED** — closing the gap requires a file currently modified in
  another active worktree; the reproducer remains on
  `test/rfc0087-conformance-matrix`.

## Ownership exclusions observed before editing

| Owner/worktree | Current files relevant to this audit |
| --- | --- |
| `worktree-perf-round4` | `crates/witchy-interp/src/interpreter.rs`, interpreter tests, Cargo metadata |
| `impl/rfc0081-existential-frontend` | `crates/witchy-types/src/typeck.rs`, compiled lowering/codegen, parser/linker/type representation, `spec/language.md` |
| `impl/rfc0050-container-method-surface` | `src/example_tests.rs`, `std/list.witchy`, `std/dict.witchy`, `std/set.witchy` |
| `fix/gate-nextest-concurrency` | merge-queue/nextest gate behavior; explicitly outside this goal |
| RFC-0080 worktrees | metaprogramming frontend and docs; explicitly outside this goal |

## RFC-0087 acceptance criteria

| # | Status | Current evidence / gap |
| ---: | --- | --- |
| 1 | **PROVEN** | Current-master `cargo test --workspace rfc0087` passes the five type-checker tests accepting arbitrary `var` positions and result shapes. |
| 2 | **PROVEN** | Current-master RFC-0087 tests and the dedicated matrix cover tail, explicit `return`, callee `?`, success/error multi-`var` envelopes, and the Result/Option receiver classification edge on both backends. |
| 3 | **EXTERNALLY OWNED** | Immutable, temporary, `move`, default, async/gen, and overlap rejection all execute. Exact current diagnostics are pinned and contain no ABI markers. The RFC-required immutable/temporary wording still does not name the resolved parameter; improving it requires actively owned `typeck.rs`/parser surfaces. |
| 4 | **PROVEN** | Dedicated and existing differential tests cover nested field/list/dict places and evaluate computed coordinates exactly once. |
| 5 | **PROVEN** | Existing and dedicated differential tests cover call, operator, tuple/list, comprehension/filter, interpolation, short-circuiting, `??`, match, if, method receiver, and assignment order. |
| 6 | **PROVEN** | Dedicated method/free `List.pop` cases and existing user/std tests produce identical results and write-backs on both backends. |
| 7 | **EXTERNALLY OWNED** | Typed trait dispatch with `var self` passes on both backends and convention mismatch rejects. Evidence that the diagnostic points to both trait and impl declarations is missing and requires the actively owned type-checker diagnostic path. |
| 8 | **PROVEN** | Named functions, lambdas, indirect calls, nested indirect places, and convention type identity execute/reject as specified on both backends. |
| 9 | **PROVEN** | Async/gen `var` parameters reject; synchronous `var` calls work on mutable locals across the shipped async segment seam and between generator yields on both backends. |
| 10 | **PROVEN** | Bare resolved-`var` calls discard auxiliary results and commit; non-`var` non-`Nil` calls reject; explicit discard remains legal. |
| 11 | **PROVEN** | Current differential coverage includes zero/one/multiple `var`s, generic and alias-equal returns, same-typed auxiliary results, caller/callee `?`, and `??`. |
| 12 | **MISSING** | Corpus compilation shows the migrated tree type-checks, but there is no stable compiler-produced classification/count report proving every obsolete call-site class is zero. |
| 13 | **PROVEN** | Current `spec/language.md` states the convention, exclusivity, function-type identity, and evaluation order; generated stdlib freshness is enforced. Further wording edits are currently owned by RFC-0081. |
| 14 | **PROVEN** | RFC-0088 stats/differential tests provide extraction aliasing, refcount, heap, one-search, forced-copy, and no-copy evidence without adding a new `*_cap` family. |
| 15 | **MISSING** | The 2026-07-14 report records three timings and four completion claims, but no focused current-master harness compares all seven named kernels under optimized versus `WITCHY_OPT=-inplace` execution. |
| 16 | **MISSING** | `rfcs/0087-migration-report.md` is checked in, but its call-site counts came from a review plus corpus success, not a durable compiler/type-resolved census with stable classifications. |
| 17 | **MISSING** | RFC/status/changelog/release text has not yet been re-audited against the gaps above; both RFC-0087 and RFC-0088 are already marked implemented despite missing migration/performance gates and the failure below. |

## RFC-0088 amendment disposition

| Rule | Status | Current truth |
| --- | --- | --- |
| Commit on callee `?` | **PROVEN** | RFC-0087 is the normative home and both backends commit partial progress identically. RFC-0088 correctly delegates source semantics to RFC-0087. |
| Captured assignment projections use current-root bounds behavior | **FAILING** | Minimal reproducer: `var values = [1]; values[0] = shrink(values)` where `shrink(var values)` pops the only element. Compiled Wasm traps with ``runtime error: `main`, line 8: list index 0 out of bounds (length 0)``. The interpreter incorrectly returns normally and prints `unreachable`. |
| No duplicated source-semantic home | **PROVEN** | RFC-0088 states RFC-0087 is the sole source contract and confines itself to ownership-aware extraction. |
| Future view lifetime work stays with its owner | **PROVEN** | RFC-0088 references RFC-0083 loan facts without redefining the view-lifetime model. |
| Implementation status waits for all release gates | **FAILING** | Both RFCs are marked implemented before the compiler census, current performance harness, docs/status audit, and stale-projection parity failure are closed. |

## Blocked semantic reproducer

Expected rule:

> Assignment captures destination coordinates once, evaluates the RHS, then
> applies those coordinates to the current root. If RHS write-back invalidates
> the projection, the final store follows ordinary bounds behavior.

Observed:

- Interpreter: `Ok(["unreachable"])`
- Compiled Wasm:
  ``Err("runtime error: `main`, line 8: list index 0 out of bounds (length 0)")``

Owners:

- Interpreter fix surface:
  `worktree-perf-round4` currently modifies
  `crates/witchy-interp/src/interpreter.rs`.
- Compiled/type surfaces:
  `impl/rfc0081-existential-frontend` currently modifies
  `crates/witchy-lower/src/codegen/mod.rs`,
  `crates/witchy-lower/src/codegen/assembly.rs`, and
  `crates/witchy-types/src/typeck.rs`.

The failing differential test
`stale_assignment_projection_uses_ordinary_bounds_behavior` remains committed
on the blocked branch `test/rfc0087-conformance-matrix` at `5f0f97ff`. It is
intentionally absent from the green, mergeable conformance-evidence branch.
