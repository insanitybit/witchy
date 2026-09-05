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
| B2 | partial | Branch probes cover owned pointer-slot storage classes, initialized pointer-slot movement, recursive list destruction, RC/static sharing, duplicate-owned-return refusal, pointer-parent reclaim refusal, dynamic-prefix leftover-slot refusal, five-local recursive destructor frames, and zero/max-RC clone traps. Two concrete child-ownership leaks found by adversarial review are fixed in `BUG-0165-pointer-child-reclaim-accounting.md`. | Complete clone/move/replace/drop/reclaim manifest for strings and scalar/string lists and merged gate evidence. |
| B3 | partial | List and UTF-8 string indexing emit runtime `Guard`s; out-of-bounds admitted Wasm traps; header/body confinement and length/capacity checks have focused tests. | Guard near-misses, mutable list length/capacity transitions, and merged gate evidence. |
| B4 | candidate-green | Direct and indirect `let`/`own` reference calls execute; `var` uses a structural write-back result token; proper-tail calls move owned arguments without residual caller cleanup; CFG release placement handles branch cleanup and simultaneous loop permutations. | Complete compact supported-call manifest and merged gate evidence. The 300,000-transition stress campaign is post-foundation work. |
| B5 | candidate-green | Canonical host checking uses authenticated import contracts, rejects generic value roles carrying references or capabilities, and directly refuses integer and linear-memory-reference values disguised as `Cap<K>` arguments. Broader capability APIs are outside the foundation subset. | Merged gate evidence. |
| B6 | partial | The independent interpreter agrees with admitted Wasm for focused scalar, signed-width, RC/static, list, string, and call probes. | Digest-bound foundation corpus, adversarial ownership/bounds cases, and merged gate evidence. |
| B7 | partial | The canonical backend accepts validated authority and focused raw/mismatched forms reject. | Constructor/reachability mechanical gate, reviewed exhaustive SafeWIR-to-WasmIR mapping, removal of backend semantic rewrites, and merged gate evidence. |
| B8 | candidate-green | Ordinary checked-source production compilation invokes no optional optimization: it lowers, validates initially, performs the mandatory tail-call legalization, validates finally, and admits. The optional optimization session was deleted from this path. | Mechanical registry evidence for mandatory legalization and merged gate evidence. |
| B9 | partial | The compact canonical pipeline selector is 43/43 green across scalars, UTF-8 strings, scalar/string lists, runtime-checked indexing, direct/indirect calls, `let`/`var`/`own`, loops, proper tails, mutation/reclamation, and packed scalar records. Deferred v1 qualifiers fail closed before normalization in canonical function signatures, local ascriptions, region annotations, lambda signatures, and expression type ascriptions; capturing closures remain intentionally unsupported. | Bind precise diagnostics for every remaining excluded family and obtain merged evidence. Canonical specialization completeness currently rejects an excluded async fixture during checked-source construction rather than at canonical lowering; preserving the obligation without weakening callable authority remains required. |
| B10 | candidate-green | `compile_checked_module_binary` admits exclusively through canonical SafeWIR and returns only bytes projected from `AdmittedCanonicalArtifact`; checked generated-test, development/Glamour, island execution, and build-module entrypoints fail closed before legacy lowering; the raw module encoder remains test-feature-gated. `emit-wat` is diagnostic text only and has no internal compiler publish path. Explicit external-module execution through `Runtime::spawn` is outside the compiler-generated-artifact claim. | Update gate expectations for intentionally unsupported workflows and obtain merged evidence. |
| B11 | candidate-green | Launch metadata is derived from checked source, included in SafeWIR identity, emitted before admission, matched exactly, and mutation-refused. Ordinary packaging no longer appends bytes; ordinary run carries `AdmittedCanonicalArtifact` into `spawn_admitted`; unauthenticated production Wasm caches are disabled. Checked synthetic emitters are sealed pending canonical migration; external modules use an explicitly separate runtime API. | Merged gate evidence. |
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
- `canonical_pipeline::tests::var_list_write_back_replacement_reaches_admitted_execution`
- `canonical_pipeline::tests::list_string_push_moves_owned_elements_and_executes`
- `canonical_pipeline::tests::admitted_artifact_carries_checked_launch_contract_and_refuses_mutation`
- `canonical_pipeline::tests::production_entry_emits_only_admitted_canonical_bytes`
- `canonical_source_witness_tests::browser_compile_preserves_the_single_admitted_launch_section`
- `rfc0165_route_gate` (10/10)
- `canonical_lower::tests::unique_contracts_stay_fail_closed`
- `canonical_lower::tests::local_unique_ascriptions_stay_fail_closed`
- `canonical_safewir::check::host::tests::integer_cannot_impersonate_a_capability_argument`
- `canonical_safewir::check::host::tests::linear_memory_reference_cannot_impersonate_a_capability_argument`
- `canonical_safewir::check::memory::tests::pointer_bearing_reclaim_requires_recursive_drop`
- `canonical_safewir::check::memory::tests::dynamic_pointer_prefix_cannot_hide_an_unaccounted_child_slot`

These probes establish only their named behavior on the implementation branch.
They do not establish corpus coverage, backend correspondence, optimization
equivalence, source type-system soundness, or compiler-wide memory safety.
