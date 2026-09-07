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
| B4 | partial | Direct and indirect `let`/`own` reference calls execute; `var` uses a structural write-back result token; proper-tail calls move owned arguments without residual caller cleanup; CFG release placement handles branch cleanup and simultaneous loop permutations. The backend lowers tail sites to `return_call`/`return_call_indirect`; the production engine now admits the tail-call proposal, which it previously disabled so every canonical program with a proper tail site failed at Wasmtime compile (`BUG-canonical-tail-call-engine-disabled.md`). | Complete compact supported-call manifest and merged gate evidence. RFC-0090 allows native tail calls only under differential and resource tests: on 2026-09-06 a 300,000-transition probe (self and mutual recursion inside `if`/`match` arms, 256-slot budget) runs through `spawn_admitted` in `tests/misc/rfc0165_admitted_launch.rs`, and `tests/rfc0090/tail_positions.rs` passes on the canonical route; tail sites nested in `if`/`match`/block arms are lowered along the manifest path since that date. Merged gate evidence is still owed. |
| B5 | candidate-green | Canonical host checking uses authenticated import contracts, rejects generic value roles carrying references or capabilities, and directly refuses integer and linear-memory-reference values disguised as `Cap<K>` arguments. Broader capability APIs are outside the foundation subset. | Merged gate evidence. |
| B6 | partial | The independent interpreter agrees with admitted Wasm for focused scalar, signed-width, RC/static, list, string, and call probes. | Digest-bound foundation corpus, adversarial ownership/bounds cases, and merged gate evidence. |
| B7 | partial | The canonical backend accepts validated authority and focused raw/mismatched forms reject. Admission refusals now carry the encoder's own diagnostic instead of a bare `Lowering` tag. Since 2026-09-06 an executable SafeWIR oracle (checker semantics, backend-independent) runs every admitted program with a heap-leak audit on both sides; `witchy parity` requires agreement. | Constructor/reachability mechanical gate, reviewed exhaustive SafeWIR-to-WasmIR mapping, removal of backend semantic rewrites, and merged gate evidence. The scalar `match` refusal (`BUG-canonical-match-cfg-unstructured-block.md`) was a lowering defect, now fixed; one encoder limit remains: a `BranchIf` whose one side only terminates finds no join (`find_join`), so an exhaustive `match` without a wildcard is refused. |
| B8 | candidate-green | Ordinary checked-source production compilation invokes no optional optimization: it lowers, validates initially, performs the mandatory tail-call legalization, validates finally, and admits. The optional optimization session was deleted from this path. | Mechanical registry evidence for mandatory legalization and merged gate evidence. |
| B9 | partial | The compact canonical pipeline selector is green across scalars, UTF-8 strings, scalar/string lists, runtime-checked indexing, direct/indirect calls, `let`/`var`/`own`, loops, proper tails, mutation/reclamation, and packed scalar records. Deferred v1 qualifiers fail closed before normalization in canonical function signatures, local ascriptions, region annotations, lambda signatures, and expression type ascriptions; capturing closures remain intentionally unsupported. Checked-source construction no longer refuses valid programs: library generic specializations outside the entry scope, closure-witness precision limits, and callable-ABI encoding limits are recorded as sorted `CanonicalObligation`s, digest-bound into the source witness and SafeWIR authority, and refused by canonical lowering as `UnsupportedCanonicalObligation`; entry-scope forged specializations and witness-internal inconsistencies still refuse at construction. A `witchy check` census over `examples/*/src/*.witchy` (2026-09-05, `CENSUS-2026-09-05-canonical-examples.md`) admitted 0 of the 107 runnable examples at the start of 2026-09-05 and 3 (`app`, `ownership`, `strings`) by its end; the 56 files that report `ok` without running have no `main`, so canonical lowering never ran on them. Every failure is now a precise lowering or admission refusal rather than a type error, except one compiler panic (`let` parameter convention, `canonical_lower.rs` `unreachable!`). Thirteen in-subset defects are enumerated in the census (string interpolation, `if` without `else`, `if` whose arms are effects, loops in a function holding a capability, `match` on literals, helper-returned `String` release placement, `var` parameters, packed scalar records, cross-module calls, short-circuit `&&`/`||`, `return` in a block, `Float` arithmetic, the `let`-convention panic, and `own` parameters: an `own Int` reaches the checker and is refused `UnknownCallableContract`, an `own String` fails owned release placement; `canonical_lower::tests::ordinary_own_identity_transfers_reference_without_clone_and_admits` pins that pair and is deliberately red). By the end of 2026-09-05 the `let` panic, capability-in-loop, `if` without `else`, effect arms, short-circuit operators, literal-to-owned-parameter materialization, early `return`, `own` scalars, range `for` loops, and `Float` arithmetic/comparison are closed with launch tests in `tests/misc/rfc0165_admitted_launch.rs`; string interpolation, packed records under qualified type names, `var` scalar parameters, nested loops, and early `return` inside a loop are closed on 2026-09-06 (`examples/{hello,fizzbuzz,compute,constants,mutate,guard,ranges,subscript}` run; 12 runnable examples admit with parity as of the seventh slice on 2026-09-06); list patterns and the shared-versus-fresh join followed on 2026-09-06 (13 runnable examples admit with parity); ordinary heap records, packed records as parameters, closures, and std cross-module calls remain outside the compact subset. | Close the thirteen in-subset defects in the census, starting with the `let`-convention panic and the capability-in-loop ownership refusal; make every remaining refusal name the source construct rather than an internal id; then rerun the census and record the runnable admission count. |
| B10 | partial | `compile_checked_module_binary` admits exclusively through canonical SafeWIR and returns only bytes projected from `AdmittedCanonicalArtifact`; checked generated-test, development/Glamour, island execution, and build-module entrypoints fail closed before legacy lowering; the raw module encoder remains test-feature-gated. `emit-wat` is diagnostic text only and has no internal compiler publish path. Explicit external-module execution through `Runtime::spawn` is outside the compiler-generated-artifact claim. Until 2026-09-05 the entry-scope predicate excluded a bare `main` from any program not in a module named `main`, so admitted artifacts omitted the program (`BUG-canonical-entry-scope-drops-bare-main.md`); one shared predicate now governs the ABI manifest, tail ordinals, layouts, and lowering. | Since the `witchy.entry` section (2026-09-06) `Runtime::spawn` enters admitted bytes through their own entry, so `witchy stats` runs an admitted example (verified on `examples/hello`); the bench harness follows the same spawn path. Update gate expectations for intentionally unsupported workflows and obtain merged evidence. |
| B11 | partial | Launch metadata is derived from checked source, included in SafeWIR identity, emitted before admission, matched exactly, and mutation-refused. Ordinary packaging no longer appends bytes; unauthenticated production Wasm caches are disabled. The admitted artifact records a validated launch entry (`CanonicalEntry`: export, capability parameters, result convention) derived inside admission from the checked `main` ABI and validated SafeWIR signature; `Runtime::spawn_admitted` enters it, minting one opaque host token per capability parameter and printing an `Int`/`Float` result under the frozen exit-code convention. Before 2026-09-05 no canonical program could run from the CLI: the runtime looked for a `run` export canonical modules never had. `tests/misc/rfc0165_admitted_launch.rs` runs admitted artifacts through the real runtime. | Since 2026-09-06 the entry is bound: admission emits it as the `witchy.entry` section, matches it exactly, and the runtime launches a `.wasm` file from that section (`admitted_bytes_launch_from_their_entry_section_without_the_artifact`; `witchy compile --out` then `witchy app.wasm` exits with the `Int` result). Merged gate evidence is still owed. |
| B12 | partial | The native `witchy-wir` test target compiles again after stale backend test callers were repaired; focused native tests execute. | Required CI/merge-queue jobs for native `witchy-wir` and `witchy-lower`, with compile failure treated as red, and terminal merged evidence. |

## Known red tests on the branch (classified, not hidden)

- None as of 2026-09-06. The nine `witchy-wir` tests red at the handoff
  `HEAD` were fixtures predating the callee-drops ownership model; they are
  restored with their intent (see the census, twelfth slice). Restoring the
  pointer-list fixture exposed and fixed a destructor scratch-slot defect in
  the encoder and its plan mirror.
- The untracked `crates/witchy-wir/tests/canonical_optimization_execution.rs`
  references APIs the optimizer-free baseline removed and breaks
  `cargo test -p witchy-wir --tests`; it predates the handoff and is preserved
  unmodified by instruction.

## Landing classification (full workspace suite, 2026-09-06)

`cargo nextest run --workspace --no-fail-fast` on a clean worktree of the
branch HEAD (7807c0dfc), 41.6 minutes: **4268 tests, 3903 passed, 364 failed,
1 timed out, 19 skipped.** The reds are not defects of the foundation work;
they are the consequence of the branch's production policy, which admits
programs only through canonical SafeWIR and refuses everything outside the
compact subset instead of falling back to the raw encoder.

| Class | Failing tests | What they do | Refusal they hit |
|---|---|---|---|
| `witchy::glamour dom` | 74 | render the real book/docs bundle through the compiled playground | `cannot compile to WASM: unsupported lowering` (layout types such as `Dict`, records, `Option`) |
| `witchy::bin commands::web::tests` | 46 | the web dev command compiles playground programs | same, surfaced as `WebCommandError` |
| `witchy::misc typed_errors` | 18 | typed error programs (`Result`, `Bytes`) on both backends | layout types outside the subset |
| `witchy::misc rfc0087_uniform_var` | 18 | uniform `var` envelopes with records/lists of records | `compile RFC-0087 program: unsupported lowering` |
| `witchy::misc rfc0105_fixture_cli`, `rfc0077`, `rfc0152`, `rfc0129`, `rfc0130`, `region_copyout`, `sealed_task_handles` | ~46 | CLI and fixture flows that compile async, channel, region, and task programs | `Command failed: .../witchy ...` wrapping the same refusal |
| trusted application e2e, minigrep distribution | 10 | build and run trusted native applications from full programs | the application program is outside the subset |
| `example_tests::example_sweeps`, `semantic_conformance`, `examples_index`, `architecture`, `rfc_status_hygiene` | ~10 | sweep every example/book block; ledger hygiene | sweeps include refused programs; hygiene checks see the new companion docs |
| `rfc0129_scalar_executor_performance` | 6 (1 timed out) | executor benchmarks over compiled programs | refused programs, and one timeout |

Reason signatures across all failure output: `cannot compile to WASM:
unsupported lowering` 126 occurrences, `unsupported canonical v1 layout type`
358, `canonical SafeWIR` 498, `MissingCheckedSourceWitness` 202 (paths that
compile without a checked source witness, such as WAT or externally built
modules), `Command failed` 60.

**What this means for landing.** The branch cannot pass the gate as it stands,
and the two ways to make it pass are both decisions outside this work's
authority: restore the raw production encoder as a fallback (explicitly
forbidden for this milestone), or classify the listed suites as
outside-the-foundation and remove them from the blocking gate until coverage
reaches them (which is scope policy, not a fix, and `#[ignore]` is not the
mechanism). The recommendation is the second, applied per class above with
this table as the record, followed by queue submission. Until that decision,
the branch stays unsubmitted and every foundation suite listed in this ledger
is green.

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
