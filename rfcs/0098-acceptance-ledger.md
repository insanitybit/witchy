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
| RFC design on `master` | DONE | `5f54c23c` contains the proposal; this closeout records the subsequently merged implementation. |
| `impl/rfc0098-syntax` | DONE | Rebased commit `9ab02e93` merged through `mq-126f5826842b74196b346920b0be2a2b6b5355c5` after all six gate stages passed; it has no unresolved queue entry. |
| `impl/rfc0098-projection` | DONE | Semantic commit `81920124` merged in batch tip `032e6f53` through `mq-160d9333399ce52303aca5a80143c8c99f8be36c`; all seven full-gate stages passed and the queue entry is terminal. |
| `rfc/0098-structural-record-width` | OBSOLETE | Authoring commit `cf5bc073` is patch-identical to rebased commit `5f54c23c`; its worktree was swept after merge. |

There are no BROKEN RFC-0098 branches or unresolved RFC-0098 queue entries.
Both implementation slices passed the complete merge gate and merged on
2026-07-19. The obsolete authoring branch is retained only as ancestry evidence.

## Acceptance criteria

| # | Status | Required checked-in evidence |
|---:|---|---|
| 1 | DONE | `structural_record_width_projection_runs_on_both_backends` and `structural_record_width_expected_site_matrix_agrees_on_both_backends` cover annotations, assignments, default/read-only/`let`/`own` arguments, returns/tails, list/tuple/record slots, and `as`. |
| 2 | DONE | `width_conformance_reports_missing_and_mismatched_fields`, `structural_record_width_rejections_are_shared_before_backends`, and `rfc0098_lsp_width_error_points_at_the_projection_site` pin complete reasons and the source line. |
| 3 | DONE | `9ab02e93` landed `normalizes_record_composition_to_the_direct_exact_shape`, `normalizes_generic_record_composition_after_substitution`, `normalizes_composition_beneath_an_ownership_qualifier`, `quote_type_owns_structural_and_borrowed_types`, and `structural_record_type_spread_round_trips_canonically`. |
| 4 | DONE | `9ab02e93` landed `record_composition_collapses_identical_fields_and_rejects_conflicts` and `record_composition_rejects_non_record_bases_and_tracks_cycles`. |
| 5 | DONE | `inference_containers_functions_equality_and_nominals_remain_exact`, composition rejection tests, and the existing transitive structural-capability firewall reject nominal, capability-bearing, existential, tuple, union, and unresolved forms. |
| 6 | DONE | `inference_containers_functions_equality_and_nominals_remain_exact` pins unannotated joins, generic inference, containers, function values, and cross-shape equality as exact. |
| 7 | DONE | `structural_record_projection_observability_agrees_on_both_backends` proves target-only rendering, reflection fields, JSON, equality, and dictionary-key comparison; compiler-generated exact-shape `Eq` enables the compiled key/hash path without user structural impls. |
| 8 | DONE | `checked_width_fact_lowers_to_source_once_and_exact_target_construction` and the `source-id`/`source-label`/`source-note` order in `structural_record_width_projection_runs_on_both_backends`. |
| 9 | DONE | `structural_record_width_projection_runs_on_both_backends` preserves the richer borrowed source; `var_is_invariant_and_own_consumes_the_richer_source` pins use-after-move rejection. |
| 10 | DONE | `var_is_invariant_and_own_consumes_the_richer_source` rejects root, field, index, and nested-place width write-back before reservation. |
| 11 | DONE | The four `structural_record_width_*`/`structural_record_projection_*` examples run the interpreter and compiled Wasm with identical success output; rejection tests fail in shared checking. |
| 12 | DONE | `structural_record_projection_observability_agrees_on_both_backends` exercises String references and compiled dictionary keys; `record_projection` validates authenticated exact identities and constructs fields without casts or universal-slot relabeling. |
| 13 | DONE | `record_projection_resource_bound_is_browser_checkable` and `web/witchy-runtime/witchy-runnable.test.mjs` compare 1 versus 64 projections using exported deterministic counters: 63 extra projections add at most 63 RC/bump allocations and exactly 63 loop-region rewinds. |
| 14 | DONE | `structural_record_type_spread_round_trips_canonically`, quote/normalization tests, `rfc0098_hover_displays_structural_record_composition`, and source-located LSP diagnostics cover formatter, expansion, quoting, hover, and diagnostics. |
| 15 | DONE | `spec/language.md`, runnable `book/src/tour-data.md` behavior/counter/migration guidance, checked `book/examples.json`, this RFC's implemented tracking, and RFC-0078's dated change note are on master. |

## Landed slices

- `9ab02e93` — normalized record-composition syntax, alias/generic/qualified
  resolution, cycle/conflict rejection, formatting, quoting, compiled-Wasm
  parsing, and RFC-0080 regression evidence. The merge queue passed workspace
  tests, Witchy formatting, compile checking, Clippy, the Wasm playground build,
  and all 128 runnable book blocks.
- `81920124` (full-gate batch tip `032e6f53`) — directed expected-site width
  conformance, authenticated shared projection lowering, ownership/calling
  conventions, exact observable target shape, backend differential coverage,
  browser operation counters, tooling, specification, runnable examples, and
  migration guidance. The queue passed all 2,344 workspace tests, Witchy
  formatting, compile checking, Clippy, the freshly built Wasm playground, all
  35 queue-infrastructure tests, and all 130 runnable book blocks.
