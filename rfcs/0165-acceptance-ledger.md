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
| B1 | candidate-green | The operation-to-rule manifest exists and is mechanical (2026-09-07): `crates/witchy-wir/src/canonical_safewir/manifest.rs` (rendered as `rfcs/0165-rule-manifest.md`, drift-checked) lists every `CanonicalRuleId` exactly once in declaration order (a new variant fails to compile until it has a row), names each rule's checker judgment, predicate, interpreter semantics, and backend correspondence rule, and cites the focused admitting and refusing tests, each of which must exist in the file it is attributed to. `Retain` is the one rule the checker refuses and the backend does not map (`retain_is_refused_in_v1`). | Landed 2026-09-07 (`53e168b22`). Still open: an independent review of the rendered manifest against the RFC's rule list. The slice-40 reviews (census) challenged the closure, host-buffer, copy, and loop-lane boundaries in code and by probe; none read the manifest against the RFC row by row, so this row does not close on them. |
| B2 | merged-green | The B2 table in the same manifest records clone, move, replace, drop, reclaim, count overflow, and count underflow for `String`, `List(Int)`, and `List(String)` with a verdict and evidence per cell (`ownership_manifest_covers_every_transfer_of_every_storage_class`); replacement of a stored reference is refused rather than leaked (`store_cannot_overwrite_a_stored_reference`); pointer-bearing prefixes clone by retaining every element and drop recursively; counts trap at the i32 maximum and below zero. | Merged gate evidence. |
| B3 | merged-green | Guard near-misses are pinned end to end (`admitted_guard_near_misses_trap_exactly_at_the_boundary`, 2026-09-07): the last valid index reads, the index equal to the length traps, a negative index traps, an empty list's first element traps, and a push past the capacity grows. Header bootstrap and length/capacity transitions remain pinned by `logical_length_guard_is_not_satisfied_by_capacity_guard`, `update_count_grows_logical_length_only_after_capacity_guard`, and the header-projection tests. | Merged gate evidence. |
| B4 | merged-green | The B4 table in the manifest is the compact supported-call manifest: direct calls by scalar value, `let` (cloned token), `own` (moved token), and `var` (write-back lane); indirect scalar calls; proper tail calls direct and indirect (300,000-transition probes); host calls only with a capability value of the declared kind; memory-bearing value arguments, indirect reference arguments, and undischarged or doubly discharged tail sites refuse; `CallBorrow` does not exist in v1. | Merged gate evidence. |
| B5 | merged-green | Canonical host checking uses authenticated import contracts, rejects generic value roles carrying references or capabilities, and directly refuses integer and linear-memory-reference values disguised as `Cap<K>` arguments. Broader capability APIs are outside the foundation subset. | Merged gate evidence. |
| B6 | merged-green | The digest-bound foundation corpus exists (2026-09-07): `rfcs/0165-safewir-corpus.json` names 43 programs under `tests/rfc0165/corpus/` with their SHA-256 and an independently written expected stdout; `tests/misc/rfc0165_corpus.rs` refuses digest drift, admits every program through canonical SafeWIR, and requires the SafeWIR oracle, the admitted Wasm, and the expected stdout to agree with no leaked objects. Adversarial ownership and bounds probes are the manifest's refusing evidence (B1-B4) plus the near-miss traps (B3). | Merged gate evidence. |
| B7 | candidate-green | The SafeWIR-to-WasmIR mapping is one reviewed table keyed by rule, `canonical_wasmir::rule_backend` (2026-09-07): every rule maps under exactly one correspondence rule or is refused (`Retain`), `operation_rule` delegates to it, `check_correspondence` requires one mapping per operation and terminator, and the manifest's backend column is checked equal to it. Admission refusals carry the encoder's own diagnostics; the structuring backend accepts reducible control flow only. | Landed 2026-09-07 (`53e168b22`). Still open: an independent review of the rendered rule-to-backend mapping as a whole. The slice-40 closure review confirmed the encoder and plan mirror token-identical for the callable lane, the write buffer, and the tail call through a closure; the rest of the table has not been reviewed independently. |
| B8 | merged-green | Ordinary checked-source production compilation invokes no optional optimization: it lowers, validates initially, performs the mandatory tail-call legalization, validates finally, and admits. The optional optimization session was deleted from this path. | Landed 2026-09-07 (`53e168b22`). There is no separate legalization pass to register: `canonical_pipeline::validate_checked_module` is lower, `check_initial_with_witness`, `validate_final`, with no transformation between them, and proper tail calls are emitted by the lowering as `TailCall`/`VarTailCall` terminators at exactly the checked manifest's sites, which the checker enforces (`MissingTailObligation`, `TailSiteFunctionMismatch`, `UnexpectedTailSite` in `check.rs`). Evidence: `checked_proper_tail_call_reaches_return_call_execution`, `checked_function_value_tail_call_reaches_admitted_execution`, `indirect_own_list_tail_call_reaches_admitted_execution` (`canonical_pipeline.rs`), and `admitted_proper_tail_calls_inside_if_and_match_arms_run_in_constant_stack` (launch). |
| B9 | merged-green | The digest-bound corpus (B6) covers scalars, control flow (`if`/`match`/`while`/`for`/`while let`/`break`/`continue`), UTF-8 strings, scalar/string lists, runtime indexing, mutation, direct/indirect top-level calls, `let`/`var`/`own`, loops, proper tails, reclamation, packed scalar records, and (beyond the compact list) heap records, sums, tuples, and lists of aggregates. Every excluded family has a precise canonical refusal recorded in the census (`CENSUS-2026-09-05-canonical-examples.md`, 42 slices; 127/163 examples check, 71 runnable agree). | Merged gate evidence. |
| B10 | partial | `compile_checked_module_binary` admits exclusively through canonical SafeWIR and returns only bytes projected from `AdmittedCanonicalArtifact`; checked generated-test, development/Glamour, island execution, and build-module entrypoints fail closed before legacy lowering; the raw module encoder remains test-feature-gated. `emit-wat` is diagnostic text only and has no internal compiler publish path. Explicit external-module execution through `Runtime::spawn` is outside the compiler-generated-artifact claim. Until 2026-09-05 the entry-scope predicate excluded a bare `main` from any program not in a module named `main`, so admitted artifacts omitted the program (`BUG-canonical-entry-scope-drops-bare-main.md`); one shared predicate now governs the ABI manifest, tail ordinals, layouts, and lowering. | Since the `witchy.entry` section (2026-09-06) `Runtime::spawn` enters admitted bytes through their own entry, so `witchy stats` runs an admitted example (verified on `examples/hello`); the bench harness follows the same spawn path. Update gate expectations for intentionally unsupported workflows and obtain merged evidence. |
| B11 | partial | Launch metadata is derived from checked source, included in SafeWIR identity, emitted before admission, matched exactly, and mutation-refused. Ordinary packaging no longer appends bytes; unauthenticated production Wasm caches are disabled. The admitted artifact records a validated launch entry (`CanonicalEntry`: export, capability parameters, result convention) derived inside admission from the checked `main` ABI and validated SafeWIR signature; `Runtime::spawn_admitted` enters it, minting one opaque host token per capability parameter and printing an `Int`/`Float` result under the frozen exit-code convention. Before 2026-09-05 no canonical program could run from the CLI: the runtime looked for a `run` export canonical modules never had. `tests/misc/rfc0165_admitted_launch.rs` runs admitted artifacts through the real runtime. | Since 2026-09-06 the entry is bound: admission emits it as the `witchy.entry` section, matches it exactly, and the runtime launches a `.wasm` file from that section (`admitted_bytes_launch_from_their_entry_section_without_the_artifact`; `witchy compile --out` then `witchy app.wasm` exits with the `Int` result). Merged gate evidence is still owed. |
| B12 | partial | The native `witchy-wir` test target compiles again after stale backend test callers were repaired; focused native tests execute. | Required CI/merge-queue jobs for native `witchy-wir` and `witchy-lower`, with compile failure treated as red, and terminal merged evidence. |

## Known red tests on the branch (classified, not hidden)

- `witchy-lower` (2026-09-07): 10 of 429 crate tests are red, all in the
  legacy `codegen::assembly::checked_codegen_boundary_tests` and dict
  fixtures (`forced_copy_dict_update_…`, `dynamic_descriptor_…`). They
  compile modules without a checked source witness or outside the subset
  and now refuse with `MissingCheckedSourceWitness` or an unsupported
  layout: the production-policy class recorded in the landing
  classification below, not a foundation defect. The foundation shard is
  `cargo nextest run -p witchy-lower canonical` (92 green).

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

**Coverage since the classification (2026-09-07).** Twenty-one dated slices in
`security-eval/findings/CENSUS-2026-09-05-canonical-examples.md` since the
classification added, inside the compact subset: loops whose exit is decided
after the header (`&&`/`||` conditions, early returns, drop blocks on exit
edges), string `==`/`!=`, `string.char_count`, `string.substring`,
`string.trim`, nested scalar lists stored by copy, lists of strings pushed in
branch arms and across loops, `math.sqrt`/`math.to_int`/`math.to_float` (one
`FloatUnary` operation), the `Bool` render, and the duration identities.
Later the same day: scalar-field records as immutable reference-counted heap
aggregates; sum types and scalar `Option`/`Result` as a tag plus one slot per
variant field (one shape shared by the layout catalog, the lowering, and the
ABI authority); constructor patterns, `??`, `?`; `starts_with`/`ends_with`/
`contains`/`find`/`to_int` and string ordering; a memo of proven path facts in
the encoder. On 2026-09-07 (slices 25-28): reference payloads in records and
sums, tuples, lists of aggregates/tuples/floats behind owned pointer slots, a
pointer-bearing `CloneRange` that retains every element (list growth clones the
old prefix; a `var` passed to a callee is copied), dynamic `string.chars`/
`string.split`/`string.replace` as two shared lowering kernels (a UTF-8
sequence loop and a match-scan loop over owned accumulator lanes), and a
checker dataflow that holds a block's failure until its incoming states
converge; Bool lists, `for` over loaded rows, `while let`, `break`/`continue`,
and capture-free scalar lambdas as function values bound to their closure
witness (slices 29-31); lambdas over references, `as`, ASCII case mapping,
`@render` inside lambdas, nested constructor patterns (a payload test behind
the tag test), `var` write-back lanes in loops, and the structural render of
sums, records, tuples, and lists, and `Float` render through the
authenticated `float_to_str` write-buffer import, and dictionaries as tuples
of two lists with `unique` on a `var` parameter read as the write-back
convention, loops that thread only the names they rebind, and closures as
records of code and `Int` captures, `Bytes` as the byte-buffer layout with
its kernels, and `Dir`/`Env` capability host calls (slices 32-42). The census
stands at 127 of 163 examples checking and 71 runnable examples agreeing
under three-legged parity; 69 launch tests pin the slices. Two
silent-wrong-answer lowering defects (a `for` loop dropping a binding rebound
inside an arm; a header fact cached across arms) were caught by `witchy
parity`, not by the oracle, which executes the same SafeWIR: independent
expected results remain the adjudicator. The landing decision above is
unchanged; the branch stays unsubmitted with every foundation suite green.

## Targeted confirmation of the landing classification (2026-09-07)

`cargo nextest run --workspace --no-fail-fast` on the branch at 19e7eb1ba's
parent (the two preserved untracked `witchy-wir` test files set aside for the
build, restored after): **4288 tests, 3928 passed, 359 failed, 1 timed out, 19
skipped**, 38.0 minutes. `scripts/rfc0165-classify-reds.py` buckets every
failure by the refusal signature in its captured output:

| Signature | Failing tests |
|---|---|
| `unsupported canonical v1 layout type` | 116 |
| `unsupported lowering` (other constructs) | 106 |
| `canonical SafeWIR pipeline/lowering failed` | 100 |
| no refusal signature in the output | 37 |

The 37 without a signature, read one by one: 13 `witchy::stats` tests assert
optimization statistics (in-place updates, SROA, format arenas) the
optimizer-free route never produces; 11 `witchy::e2e` flows report that the
`coven`/`coven-web` server never started, because those programs are outside
the subset and do not compile; 5 build-step tests (`rfc0103`, the compiled
build-step tests, `build_steps_are_default_deny_even_when_safe`) hit a
checked-source refusal spelled `canonical runtime function `build.build` lacks
a finalized signature`, an imprecise message for an unsupported program rather
than a defect; 2 `witchy-lower` boundary tests are the `MissingCheckedSourceWitness`
class already recorded; `emit-wasm` in `cli_subcommands`, the in-language test
framework sweep, and the `rfc0105` fixture transcript fail because the program
under test does not compile. Four are not policy consequences and are recorded
here as findings: `architecture::stage_crates_follow_the_declared_dependency_dag`
(the branch made `witchy-types` depend on `witchy-caps` for the launch-contract
payload without amending the contract; the edge is now explicit in
`tests/misc/architecture.rs` and `spec/architecture.md`, and the graph stays
acyclic because `witchy-caps` depends on nothing above the syntax stage),
`sandbox_vm::parity_file_agrees_on_matching_aborts` (an admitted program's
out-of-bounds abort is a Wasm trap without the interpreter's diagnostic text, a
known behavioral difference of the canonical route to decide, not a memory-safety
defect), and `test_for_paths`/`merge_queue` script expectations plus two
`sandbox_grants` confinement tests that depend on the host environment and need
a run on `master` for comparison. `rfc_status_hygiene` failed on
`0165-memory-mode-boundary.md`, now a companion document.

The classification therefore holds for 353 of 359 reds by construction and for
the rest by inspection; the gate-coverage decision remains the user's.

## Test selection shared by the gate and CI (2026-09-07)

The deferred-test exclusion is produced by one script,
`scripts/rfc0165-deferred-filter.sh`, from `scripts/rfc0165-deferred-tests.txt`;
`scripts/check.sh` and `.github/workflows/ci.yml` both pass its output as the
nextest filterset, so the two cannot drift (the first landing attempt was a
drift of exactly this kind). The script refuses any entry that would defer a
native security suite: the whole `witchy-wir` crate, the canonical
`witchy-lower` tests, and the `rfc0165_*` tests stay mandatory everywhere.

## Landed (2026-09-07)

The merge queue recorded `merged impl/rfc0165-interpreter` at
2026-09-07T20:00:18Z, sha `53e168b22`, gate log
`state/merge-queue/logs/20260907-154921-impl~rfc0165-interpreter-38402-10.log`:
tests (workspace) green (3757 run, 365 skipped by the deferred list and the
default filter), `witchy fmt`, prose style, compile check, clippy bug lints,
wasm playground build, queue infrastructure, and Rust-class paired correctness
all green. Rows B2-B6, B8, and B9 are `merged-green` on that event. B1 and B7
stay `candidate-green`: their remaining obligation is an independent review of
the rendered manifest and mapping, which the slice-40 reviews did not perform.
B10-B12 stay partial (their remaining evidence is CI, not this landing).

## Landing decision (2026-09-07)

`cargo nextest run --workspace --no-fail-fast` on the branch after slice 40 (the
two preserved untracked `witchy-wir` test files set aside for the build and
restored byte-identically): **4326 tests, 3971 passed, 354 failed, 1 timed
out, 19 skipped**, 38.4 minutes. `scripts/rfc0165-classify-reds.py` buckets 315
failures by a canonical refusal signature; the 40 without one are the same
classes as the targeted confirmation above (17 optimizer-statistics tests, 11
end-to-end flows whose server program does not compile, 5 build-step programs,
2 legacy boundary tests, the abort-diagnostic parity difference, the
in-language test framework sweep, the preserved owner fixture
`canonical_owned_slot_storage`, and two gate-script tests).

The two gate-script tests were defects of this branch, now fixed:
`merge_queue::gate_behavior::fast_gate_emits_structured_foreground_and_background_timings`
tripped `check.sh`'s empty `native_feature_args` array under bash 3.2's `set
-u`, and `test_for_paths::lower_path_keeps_wasm_but_drops_covered_nextest_checks`
no longer recognised the `--features witchy-wir/native` form of the covered
`example_tests` command.

**Decision.** The blocking gate is the whole workspace minus exactly the tests
in `scripts/rfc0165-deferred-tests.txt` (351 entries, one `binary-id test-name`
per line, generated from this run and reviewed class by class above; the
preserved untracked fixture is not listed, because nextest refuses a filter
naming a binary absent from the committed tree, which is exactly what the
first gate attempt hit). Every
one exercises a deferred language or platform family, optimizer statistics the
optimizer-free route never produces, or a preserved fixture whose owner has not
migrated it. `check.sh` excludes them by exact binary id and test name
(`WITCHY_GATE_DEFERRED`, mirroring the ungated-project families); `--full` and
an empty `WITCHY_GATE_DEFERRED` run them, so they are preserved, not hidden. A
test leaves the list when its family is admitted, never to hide a defect in an
admitted family. Everything else remains blocking, including the foundation's
security suites, which `scripts/rfc0165-foundation.sh` runs on their own: the
`witchy-wir` native library tests (checker, encoder and plan mirror, oracle,
authority, rule manifest; 567), the canonical lowering tests (91), and the
`rfc0165_*` admitted-launch (57), corpus, and oracle tests.

Re-run after the review fixes and the first two gate attempts (the filterset
naming the untracked fixture; an intrinsic in statement position): **4327
tests, 3974 passed, 352 failed, 1 timed out, 19 skipped**, 38.1 minutes. Every
failure is in the deferred list or is the untracked fixture; no deferred test
started passing.

What this decision does not claim: compiler-wide memory safety. The reviews in
slice 40 of the census challenged the latest boundaries and closed what they
found; the rows above stay `candidate-green` until the merge queue records the
landing, and B10-B12 remain partial.

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
