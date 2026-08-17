# RFC-0122 acceptance ledger

This is the integration authority for RFC-0122. “PROVEN” requires current
`master` plus the named evidence. A green queue item is not acceptance. Every
other row remains open.

## Wasm-first carrier policy

While the `ReferenceKind` place-carrier ABI is changing, aggregate, list, and
exclusive-reference fixtures are compiled-Wasm-first. A focused lowering or
runtime slice must run through `wasm_run_reowns`; it does not wait for an
interpreter implementation that would freeze the carrier prematurely. The
authoritative row carries an explicit interpreter debt with its fixture, the
missing interpreter boundary, and the ABI convergence milestone.

Once that milestone is reached, the named fixture must pass on both backends
before its row can become `PROVEN` or experimental opt mode can exit. A
Wasm-first result is executable progress, not completion.

| Criterion | fixture | missing interpreter boundary | convergence milestone | current status |
| --- | --- | --- | --- | --- |
| 12 exclusive loans | `rfc0122_local_exclusive_reference_write_agrees_on_both_backends`, `rfc0122_mutable_exclusive_parameter_writes_back_on_both_backends` | writable direct-place/reference-call carrier | direct-place write ABI frozen | converged; remaining affine evidence is separate |
| 18 aggregate affine roots | `shared_reference_tuple_preserves_each_owner_root_on_both_backends`, `shared_reference_list_preserves_each_owner_root_on_both_backends`, `exclusive_reference_list_projection_writes_the_selected_owner_on_both_backends`, `exclusive_reference_list_move_then_projection_writes_the_selected_owner_on_wasm`, `exclusive_reference_list_iteration_writes_each_owner_on_wasm` | loop element binding for a returned `List(&mut T)` | aggregate/list carrier ABI frozen | direct construction/projection fixtures converged; moved-list projection and list iteration are Wasm-first debt |
| 19 interpreter/Wasm parity | `rfc0122_direct_string_reference_uses_the_typed_scalar_carrier`, `rfc0122_function_value_float_reference_uses_the_typed_scalar_carrier`, `rfc0122_references` | typed scalar and callable return carrier | `ReferenceKind` call/result ABI frozen | converged for named carriers; forced-copy, traps, cleanup, and full type matrix remain |

Audit target: `b113ea49527d2e1c51fe92145868d1c373331f8b` on `master`
(2026-08-16). The linked RFC is accepted; this ledger determines when it is
implemented.

| Criterion | Track | State | Required evidence |
| --- | --- | --- | --- |
| 1 normal-mode reference exclusion | boundary | partial | `normal_mode_rejects_every_explicit_reference_surface_before_loan_analysis` covers reference parameter/result/local/container types, borrow and mutable-borrow expressions, and nominal lifetime declarations with the normal-mode diagnostic before loan analysis; parser/link diagnostic coverage remains |
| 2 conventional normal calls | boundary | partial | `rfc0122_normal_callers_use_conventional_opt_access_on_both_backends` proves reference-free `let`/`var`/`own` calls, while `BoundaryEntrySelection` derives proven/repair from the checked access graph and lowering consumes that selection; generated adapter ABI and normal-to-opt repair parity remain |
| 3 normal interface filtering | boundary | partial | normal importers now reject direct and alias-hidden reference functions, reference-bearing nominal types, and traits with reference methods (`be579d67`, `bc39f465`, `c8052518`); reflection, generated adapters, and the complete fixture matrix remain |
| 4 one opt source identity | contract/boundary | partial | `analysis::no_copy_tests::checked_boundary_entry_selection_distinguishes_proven_and_repair_calls` proves proven and repair selections retain one checked callable identity; generated-entry identity evidence remains |
| 5 proven versus repair parity | boundary/evidence | proven | `example_tests::ownership::rfc0122_normal_repair_preserves_alias_and_var_writeback_on_both_backends` proves the aliased normal repair preserves the old alias while committing the opt `var unique` write-back identically on interpreter and compiled Wasm |
| 6 owner-backed normal results | runtime/boundary | open | root-balance, escape, materialization counter tests |
| 7 opt syntax pipeline | contract | partial | merged `57954fe7`; add quote/highlight/full-stage parity |
| 8 uniform reference types | contract | partial | built-in/generic/nominal/container/trait/function-type matrix |
| 9 nominal lifetime versus reference distinction | contract/migration | partial | parser/migration coverage preserves nominal lifetime parameters while migrating direct relations (`rfc0122_reference_migration_distinguishes_nominal_and_direct_lifetimes`); structured shared/mutable `meta.TReference` reflection preserves access kind and lifetime. Full cross-stage type-variable, quote, and linked-module matrix remains |
| 10 migrated fixture parity | evidence | partial | `migrated_direct_shared_call_preserves_interpreter_wasm_behavior` proves migration, type checking, and both-backend behavior for a direct shared call; the broader migration matrix and counters remain |
| 11 shared loans | checker | partial | `loans_tests::shared_reference_handles_copy_but_cannot_be_consumed_or_erased` proves copying plus rejection of `move` and conventional `own` erasure. Direct-reference closure and async escape rejection are covered by `shared_reference_cannot_escape_through_a_closure` and `shared_reference_cannot_cross_async_suspension`; mutable-binding and explicit-drop matrix evidence remains. |
| 12 exclusive loans | checker/runtime/wasm | partial | `rfc0122_local_exclusive_reference_write_agrees_on_both_backends`, `rfc0122_mutable_exclusive_parameter_writes_back_on_both_backends`, `loans_tests::explicit_return_preserves_an_exclusive_reference_relation`, returned-reborrow, function-value, and closure projected-place fixtures prove executable writes, explicit-return transfer, and `var` write-back; affine moves, parent suspension, and no-copy proof remain |
| 13 mutable-to-shared conversion | checker/runtime | partial | merged `bcea90b4` relinquishes the old `&mut` handle; checker covers short shared reborrow; direct, function-value, and closure shortening now agree on interpreter and compiled Wasm (`mutable_to_shared_reference_return_preserves_the_runtime_place_on_both_backends`, `function_value_mutable_to_shared_reborrow_preserves_the_owner_on_wasm`, and `closure_mutable_to_shared_reborrow_preserves_the_owner_on_both_backends`). The remaining shortening matrix is open. |
| 14 owned qualifiers | checker | partial | `loans_tests::exclusive_reference_signature_retains_its_affine_contract` covers accepted `own unique &'a mut T`, rejects both frozen/unique orders and frozen reference targets, and rejects escaping `local unique &'a mut T`; `exclusive_handle_qualifiers_survive_aggregate_and_callable_positions` extends that through tuple and function-value positions. The remaining qualifier matrix is still open. |
| 15 convention/reference orthogonality | contract/checker | partial | `reference_function_types_preserve_parameter_convention_identity` proves a direct `&mut` can satisfy `var &'a mut T` for named and function-value calls, while `fn(let String)` rejects `fn(var String)` rather than erasing the callable convention (`b203d54c`). Handle write-back and `own` behavior remain. |
| 16 no opt-graph erasure | contract | partial | signature preservation; casts/traits/closures/adapters/tails matrix remains |
| 17 CFG precision | checker | partial | existing CFG facts; conditional returns, lending iterators, exact diagnostics remain |
| 18 aggregate affine roots | checker/runtime | partial | `exclusive_reference_aggregates_move_once_and_destructure_without_copying` proves an aggregate built from `&mut` borrows moves into one destructuring use and rejects recovery through the old aggregate spelling. `exclusive_reference_list_move_then_projection_writes_the_selected_owner_on_wasm`, `exclusive_reference_list_iteration_writes_each_owner_on_wasm`, and `exclusive_reference_list_iteration_reborrows_and_resumes_each_element_on_wasm` prove returned-list move, projection, iteration, reborrow, parent resumption, and writes through the compiled carrier; `loans_tests::exclusive_reference_loop_elements_are_affine` proves a loop element is moved, not copied. Interpreter loop-element reference binding is a debt until the aggregate/list carrier ABI freezes. The remaining affine matrix is open. |
| 19 interpreter/Wasm parity | runtime/wasm/evidence | partial | direct `Int`, `Bool`, `Float`, and `String` roots, plus aggregate projected reads/writes, agree on both backends through direct calls, function values, and closures (`rfc0122_*` ownership fixtures). The `String` fixture is `rfc0122_direct_string_reference_uses_the_typed_scalar_carrier`; `Float` direct and function-value fixtures cover the typed scalar carrier. Forced-copy/direct-place, traps, cleanup, and the full type matrix remain. |
| 20 async and escape boundaries | checker/boundary | partial | shared escape cases include direct closure capture, async suspension, channel send, and generator parameter/yield boundaries (`shared_reference_cannot_escape_through_a_closure`, `shared_reference_cannot_cross_async_suspension`, `shared_reference_sent_through_a_channel_is_rejected`, `shared_reference_cannot_cross_generator_suspension`; `14cf682a`, `e1690efb`). Direct exclusive-reference rejection at `Dynamic` has an `.owned()` remedy (`a88bed34`). `explicit_reference_cannot_cross_json_or_reflection_boundaries` covers both shared and exclusive references at the canonical serialization/reflection boundaries; host lease and the remaining closure/exclusive boundary matrix remain. |
| 21 migration command | evidence/migration | partial | `witchy migrate references` rewrites proven direct local typed owner calls and reports ambiguous sites without writing them (`84d9a0da`). It resolves imported signatures and adds a borrow only when same-arity overloads agree (`rfc0122_reference_migration_rewrites_resolved_imported_calls`, `rfc0122_reference_migration_requires_overloads_to_agree_before_borrowing`, `fef9dbf6`). [`0122-migration-report.md`](0122-migration-report.md) records the 307-file source census: no direct-relation spelling remains, and raw `View(` hits are comments, reflection text, or `AccessView`. The command-level ambiguous-site matrix remains. |
| 22 performance telemetry | evidence | partial | merged `b113ea49` pins the loan metric schema plus optimized/forced-copy output and fact rows in `reference_return_telemetry_corpus_pins_schema_and_copy_parity`; retain before/after artifacts for the remaining reference matrix |

## Track contracts

| Interface | Owner track | Consumers |
| --- | --- | --- |
| `ReferenceKind` and lifetime-bearing callable identity | contract | checker, boundary, Wasm |
| `LoanFact` with owner root, projection, kind, origin, and affine state | checker | runtime, Wasm, evidence |
| callable effects and proven/repair adapter ABI | boundary | contract, runtime, Wasm |
| place-reference read/write semantic fixture | runtime | Wasm, evidence |

## Current integration sequence

1. Generalize the executable carrier from `Int` and projected Bool slots to all
   referenceable scalar and aggregate representations, then add structured-exit
   and reborrow fixtures.
2. Complete affine checker state, parent suspension, and no-copy eligibility
   from the frozen `ReferenceKind` / `LoanFact` contract.
3. Integrate runtime and forced-copy/direct-place Wasm semantics against the
   same differential fixtures.
4. Land boundary adapters only against the frozen callable-effect ABI.
5. Close each row with its listed evidence; no row is inferred from a green
   broad gate.
