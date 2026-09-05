# RFC-0165 acceptance ledger

This ledger tracks the first production milestone in
`0165-safewir-canonical-ir.md`. A focused probe on an implementation branch is
development evidence, not acceptance. A row is complete only after all named
evidence is current on `master` and the merge queue records a terminal merged
event for the cutover candidate.

Status values are `missing`, `partial`, `candidate-green`, and `merged-green`.
Only `merged-green` completes a row.

| ID | Status | Current evidence | Evidence still required |
|---|---|---|---|
| B1 | partial | The canonical operation enum and checker exist; focused positive and negative checks cover several scalar, bounds, ownership, call, and host rules. | Exhaustive operation-to-rule manifest, independent rule review, and merged gate evidence. |
| B2 | partial | Branch probes cover owned pointer-slot storage classes, initialized pointer-slot movement, recursive list destruction, RC/static sharing, and duplicate-owned-return refusal. | Complete clone/move/replace/drop/reclaim coverage for strings and scalar/string lists; overflow/underflow attacks; merged gate evidence. |
| B3 | partial | List and UTF-8 string indexing emit runtime `Guard`s; out-of-bounds admitted Wasm traps; header/body confinement and length/capacity checks have focused tests. | Guard near-misses, mutable list length/capacity transitions, and merged gate evidence. |
| B4 | partial | Direct and indirect `let`/`own` reference calls execute; proper-tail reference calls execute; CFG release placement handles branch cleanup and simultaneous loop permutations. | `var` write-back ABI, complete supported-call manifest, 300,000-transition direct and indirect probes, and merged gate evidence. |
| B5 | partial | Canonical host checking rejects generic value roles carrying references or capabilities and uses authenticated import contracts. | Focused refusal of integer/linear-memory values in every supported capability role, precise unsupported-API diagnostics, and merged gate evidence. |
| B6 | partial | The independent interpreter agrees with admitted Wasm for focused scalar, signed-width, RC/static, list, string, and call probes. | Digest-bound foundation corpus, adversarial ownership/bounds cases, and merged gate evidence. |
| B7 | partial | The canonical backend accepts validated authority and focused raw/mismatched forms reject. | Constructor/reachability mechanical gate, reviewed exhaustive SafeWIR-to-WasmIR mapping, removal of backend semantic rewrites, and merged gate evidence. |
| B8 | partial | Constant folding and unreachable pruning are development-only and rechecked; production cutover has not occurred. | Cutover registry proving no optional pass is enabled and every mandatory legalization is separately identified and evidenced. |
| B9 | partial | Branch probes cover most foundation families and packed scalar records; capturing closure witness infrastructure is present but lowering remains intentionally unsupported. | Digest-bound compact corpus covering every named foundation family, including `var`; precise refusal probes for every excluded family; merged gate evidence. |
| B10 | missing | The frozen legacy route remains production before migration cutover. | Atomic cutover commit deleting production adapter/raw/mixed fallback authority plus reachability gate. |
| B11 | partial | Canonical admission binds checked authority to exact encoded bytes in focused tests. | Complete producer/cache/publish/launch audit, mismatch attacks, SEC-128 closure evidence, and merged gate evidence. |
| B12 | partial | The native `witchy-wir` test target compiles again after stale backend test callers were repaired; focused native tests execute. | Required CI/merge-queue jobs for native `witchy-wir` and `witchy-lower`, with compile failure treated as red, and terminal merged evidence. |

## Current focused candidate evidence

- `canonical_pipeline::tests::scalar_unary_source_reaches_admitted_execution`
- `canonical_pipeline::tests::indirect_own_list_call_reaches_admitted_execution`
- `canonical_pipeline::tests::indirect_let_list_call_reaches_admitted_execution`
- `canonical_pipeline::tests::indirect_own_list_tail_call_reaches_admitted_execution`
- `canonical_safewir::check::tests::indirect_reference_value_argument_is_refused`
- `canonical_lower::owned_tests::owned_releases_apply_loop_token_permutations_simultaneously`
- `owned_slots_require_managed_child_storage_before_consuming_move`
- `folding_then_pruning_preserves_source_binding_and_scalar_behavior`
- `authenticated_static_scalar_survives_shared_clone_and_drop`
- `canonical_pipeline::tests::packed_scalar_record_field_reaches_admitted_executable_artifact`
- `canonical_pipeline::tests::non_tail_let_list_call_retains_the_caller_token`
- `canonical_pipeline::tests::proper_tail_let_list_call_moves_the_entry_token`

These probes establish only their named behavior on the implementation branch.
They do not establish corpus coverage, backend correspondence, optimization
equivalence, source type-system soundness, or compiler-wide memory safety.
