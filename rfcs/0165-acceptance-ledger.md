# RFC-0165 acceptance ledger

This ledger tracks the first production milestone in
`0165-safewir-canonical-ir.md`. A focused probe on an implementation branch is
development evidence, not acceptance. A row is complete only after all named
evidence is current on `master` and the merge queue records a terminal merged
event for the cutover candidate.

Status values are `missing`, `partial`, `candidate-green`, and `merged-green`.
Only `merged-green` completes a row.

## Current coverage (2026-09-09)

Both `impl/rfc0165-coverage-next` and
`impl/rfc0165-dynamic-plan-integration` have terminal merged events at
`c299719d618b71830712222b37ec66547198f96c`, also confirmed as master. This lands
File host support, typed managed closure captures, the source-bound dynamic
plan (not dynamic execution), and the structural/nominal selector repair.

The single post-integration census reports **140/163 checking, 83 runnable
parity agreements, zero disagreements**. It used the existing production
binary for this revision; the measurement worktree differed only in manifest
code, documentation, and the unintegrated callable-review finding. Command:
`WITCHY_DEV_TARGET=/private/tmp/witchy-rfc0165-drop-worklist-target bash
scripts/rfc0165-dev/census.sh`. Log:
`/private/tmp/rfc0165-post-closure-file-census.log`. Its final grep exits one
when there are no disagreement lines; that is not a failed program probe.

Six in-scope first refusals remain: `dedup`, `generators`, `lazy_fib`, and
`pascal` (recursive Iter/function storage), `dynamic_reflection` (Dynamic
lowering), and `plugin_host` (function collections first; later capability
capture obligations remain). The other 17 are nine task/channel programs,
six Net programs, regex, and opt mode. This supersedes the older counts below,
not the remaining acceptance obligations. Recursive callable patches remain
withheld pending signature-correspondence and cross-graph cycle checks.

## Coverage integration landed (2026-09-08)

The merge queue landed `impl/rfc0165-concat-integration` on master as
`5bd7f6bb73de856e2fec1c52c2e9f4a3a6fd0a2e` at 17:13:17Z. Workspace tests,
Witchy formatting, prose, compile check, bug lints, playground build, and
Rust-class paired correctness passed in the serialized gate.

- Recursive nominal and concrete generic layouts, nullable active-payload
  cloning, and bounded-stack destruction are integrated. `95dbdba91` binds
  proper-tail obligations to the concrete runtime-function inventory;
  the real `generic_stack` program agrees across source, oracle, and Wasm.
- `297ae14d2` admits raw hex decoding with independent alphabet and byte-range
  guards. All four encoding probes pass, including malformed ASCII, UTF-8,
  and odd-length raw inputs, private-helper rejection, and leak checks.
- `1bd530bf7` adds generic `list.concat`. The real `bst` example agrees between
  interpreter and Wasm (three output lines). Scalar and string concat inputs
  also pass a persistent three-way execution and zero-leak probe.
- The gate for `297ae14d2` stopped on an obsolete test requiring recursive
  source to refuse. That same nullable-variant fixture now checks execution
  and reclamation on all three legs. The earlier test-binary budget failure
  is repaired by consolidation, not by raising the budget or dropping tests.
- Reproduce the changed execution families with `cargo test --test misc
  rfc0165_encoding`, `cargo test --test misc
  list_concat_preserves_owned_inputs_across_three_legs`, and `cargo test
  --test misc recursive_source_graph_executes_and_reclaims_nullable_variants`.
  Use the existing warmed absolute target:
  `CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target`.

The fresh measurement on that exact master revision admits 132 of 163
examples, with 75 runnable parity agreements and zero disagreements. The first
attempt could not create its report directory; these numbers come from the
recorded retry, not that failed reporting attempt.

The 31 first refusals classify as nine task/channel cases, six Net cases, one
regex case, one opt-mode case, and fourteen remaining in-scope first refusals:
five recursive `List(Json)` layouts, four iterator/generator layouts, and one
each for branded capabilities, dynamic reflection, File, a list of function
values, and raw base64 decoding. Later refusals may further classify a program;
this is not a claim that fixing its first refusal completes it.

Still required: the remaining in-scope coverage and review obligations, then
final current-master census and queue evidence. No additional deferred-test
exclusions were added. These results establish neither optimizer correctness
nor compiler-wide memory safety.

| ID | Status | Current evidence | Evidence still required |
|---|---|---|---|
| B1 | candidate-green | `canonical_safewir/manifest.rs` enumerates implementation rules, backend decisions, and cited tests. Its RFC crosswalk explicitly records aliases, composites, restrictions, deferred operations, and refusals. On 2026-09-09 the completeness test was strengthened to compare directly against the RFC's V1 operation-kernel block, not only a second Rust enum/list. The native manifest shard passed all eight tests; the generated Markdown is current. The first run passed seven tests but could not write the generated document under the sandbox; the authorized direct-binary retry passed eight. | Terminal merged evidence for the strengthened crosswalk and independent review of its dispositions. Table completeness does not establish the correctness of checker predicates or emitted instructions. |
| B2 | merged-green | The B2 table in the same manifest records clone, move, replace, drop, reclaim, count overflow, and count underflow for `String`, `List(Int)`, and `List(String)` with a verdict and evidence per cell (`ownership_manifest_covers_every_transfer_of_every_storage_class`); replacement of a stored reference is refused rather than leaked (`store_cannot_overwrite_a_stored_reference`); pointer-bearing prefixes clone by retaining every element and drop recursively; counts trap at the i32 maximum and below zero. | Merged gate evidence. |
| B3 | merged-green | Guard near-misses are pinned end to end (`admitted_guard_near_misses_trap_exactly_at_the_boundary`, 2026-09-07): the last valid index reads, the index equal to the length traps, a negative index traps, an empty list's first element traps, and a push past the capacity grows. Header bootstrap and length/capacity transitions remain pinned by `logical_length_guard_is_not_satisfied_by_capacity_guard`, `update_count_grows_logical_length_only_after_capacity_guard`, and the header-projection tests. | Merged gate evidence. |
| B4 | merged-green | The B4 table in the manifest is the compact supported-call manifest: direct calls by scalar value, `let` (cloned token), `own` (moved token), and `var` (write-back lane); indirect scalar and explicit owned/shared reference calls; proper tail calls direct and indirect (300,000-transition probes); host calls only with a capability value of the declared kind; memory-bearing Value arguments and undischarged or doubly discharged tail sites refuse; reference Move arguments require checked ownership transfer. `CallBorrow` does not exist in v1. | Merged gate evidence. |
| B5 | merged-green | Canonical host checking uses authenticated import contracts, rejects generic value roles carrying references or capabilities, and directly refuses integer and linear-memory-reference values disguised as `Cap<K>` arguments. Broader capability APIs are outside the foundation subset. | Merged gate evidence. |
| B6 | merged-green | The digest-bound foundation corpus exists (2026-09-07): `rfcs/0165-safewir-corpus.json` names 43 programs under `tests/rfc0165/corpus/` with their SHA-256 and an independently written expected stdout; `tests/misc/rfc0165_corpus.rs` refuses digest drift, admits every program through canonical SafeWIR, and requires the SafeWIR oracle, the admitted Wasm, and the expected stdout to agree with no leaked objects. Adversarial ownership and bounds probes are the manifest's refusing evidence (B1-B4) plus the near-miss traps (B3). | Merged gate evidence. |
| B7 | candidate-green | The SafeWIR-to-WasmIR mapping is one reviewed table keyed by rule, `canonical_wasmir::rule_backend` (2026-09-07): every rule maps under exactly one correspondence rule or is refused (`Retain`), `operation_rule` delegates to it, `check_correspondence` requires one mapping per operation and terminator, and the manifest's backend column is checked equal to it. Admission refusals carry the encoder's own diagnostics; the structuring backend accepts reducible control flow only. | Independent review on 2026-09-08 covered the then-current 39 rule identities, the rendered manifest, rule_backend, and check_correspondence; no mapping-row mismatch was found. ClosureSlot is a Load/Store subjudgment, not a standalone receipt; Retain remains explicitly refused. The 2026-09-09 bounded review below records the current 43-rule metadata. These reviews establish metadata/table correspondence only, not correctness of checker predicates, interpreter semantics, or emitted Wasm; broader B1/B7 evidence obligations remain open. |
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

### Follow-up integration evidence (2026-09-08)

- `impl/rfc0165-coverage-next` at `9a642d234` is submitted as `mq-218b9530edf45572b2354c51860789e0efece89e`; landing is not yet established. Ongoing implementation is isolated on `impl/rfc0165-unicode-decode`.
- `806b0abc8` reserves recursive nominal identities before binding bodies. Two layout-order/closure tests and the recursive list/tuple three-leg execution probe passed, with zero leaked references in the execution probe.
- `46940b7d8` and `20796ed4a` supply the reviewed RFC-kernel crosswalk. Ten manifest-family tests passed. This resolves the previously recorded B1 crosswalk gap at candidate-green, not merged-green; it does not independently certify runtime semantics.
- `e6045165f` preserves divergent valued-if arms without fake values. The scalar/string surviving-arm execution probe passed all three legs with zero leaks. Both-arms-divergent value forms remain unsupported.
- The existing typed-error family at this source revision ran nine tests: Bytes and OAuth passed; seven refused. `9a642d234` removes only OAuth's deferred entry, with its source execution, exact expected output, and parity assertions unchanged.
- Remaining typed-error refusals: duration owned-edge mismatch; URL block statement; encoding and JWT raw Base64 decoders; time incompatible memory join; HTTP divergent value block; semver missing checked closure type. These are not passing evidence or new exclusions.
- `config_merge` advanced past recursive-container layout and valued-if lowering to `MissingBinding("string.from_code")`. No execution agreement is claimed for it yet.
- Unicode construction integrated as `cd341fcd8` (worker `71fe823f3`): focused Rust-scalar expected encoding, source interpreter, SafeWIR oracle, admitted Wasm, and zero-leak probe passed. Bounded independent review found no concrete defect; adjacent scalar boundaries remain additional test opportunities. The real `config_merge` forward probe now refuses `UnsupportedType(Named("t0", []))`, not the missing Unicode intrinsic.

### Control-flow landing (2026-09-08T19:01:10Z)

- `impl/rfc0165-match-and-decoder` landed at `8665462e11c13e01f3b3183262dd266e59ca450f`, queue item `mq-c2490d1edce25c5273e496e8b3aa4e02a4c53579`. Required workspace tests, prose, compile check, and bug clippy stages passed; no flakes were reported.
- Returning match arms retain their terminators. Valued matches derive their result kind from actual continuing values, including an early String return with an Int continuation. The exact integrated valued-match test passed one test with source/oracle/Wasm agreement and zero leaks.
- Structured loop-exit lowering admits the focused multiple-owned-return case and the original duration typed-error test. Only duration's now-passing deferred entry was removed. The terminal-tail walk is structural, not a proof of runtime termination.
- HTTP typed errors now reach `UnsupportedStatement("match has no continuing arm")`; they remain deferred. Anonymous-record `reflect.reflect_one(.{x: 7})` still refuses unresolved `t0`, while scalar Json conversion admits and ordinary generic destructuring passes parity.
- Base64 decoding is not part of this landing. Its candidate passes fixed-expected oracle/Wasm cases, but executable source/native-reference evidence is being restored before submission. No additional census or compiler-wide safety claim follows from these focused results.

### Base64 and concrete record fields (candidate evidence)

- `fd9ae4d6a` and `fb31100ec` implement shared standard/URL raw Base64 decoding and compare it with the audited native reference. Logical count starts at zero and advances only after bounded byte stores; the validator was not relaxed. Alphabet offsets, vertical-tab truncation, permitted whitespace, padding, incomplete groups, and invalid-tail behavior are covered.
- `939254707` adds public-wrapper source/oracle/Wasm checks and empty/padding/digit raw cases. The integrated `cargo test --test misc base64 -- --nocapture` run passed three tests with explicit expected results and zero-leak assertions.
- `dcfdec488` obtains generic record field types from the checked receiver's instantiated aggregate shape, rather than raw declaration parameters. The strengthened focused test observes both `Int` and owned `String` instantiations through JSON, agrees across source/oracle/Wasm, and reports zero leaks. The real `config_merge` parity probe passed on the worker's equivalent production code.
- Exploratory anonymous-record traits changes were unnecessary and are preserved separately, not integrated. The initial receiver-selection/catalog hypotheses were superseded by the traced `lower_record_field` defect.
- Forward refusals remain: encoding typed errors need `encoding.base64url_encode_bytes`; JWT reaches `InvalidOwnership` in function 23; time/encoding reaches `IncompatibleMemoryJoin` in function 21. Those tests remain deferred. No full-corpus or compiler-wide safety conclusion follows from these focused cases.

### Integration wave: byte codecs and shared terminal joins

Candidate `bf2d076b0` is submitted as `mq-45d3d78c217094ae29bf4b89a8a4213e195c31bd`; submission is not landing evidence. The post-codec/concrete-record census at `dcfdec488` measured 132/163 checking, 75 runnable agreements, and zero disagreements: config_merge was newly admitted, but queens regressed, so the totals did not improve. Repair `f00792f9a` subsequently restored the full queens source/oracle/admitted-Wasm probe (92 solutions, exact board, zero heap leaks), with an explicit 20-million-step oracle budget. The original duration typed-error probe also passed after the repair. These focused results are not a new master census.

Integrated byte Base64/Base64URL encoder probe passed at `bf2d076b0`, including empty and non-UTF-8 byte inputs, padding, URL alphabet, three execution legs, and zero leaks. The original encoding typed-error probe advanced to `MissingBinding("encoding.hex_encode_bytes")`; it remains deferred, not passing. The queued wave retains independent validation and exact-byte admission without changing their rules.

A read-only review identified a remaining shared terminal-tail entry case with two loop-member predecessors. It remains a fail-closed structural-emission concern, not an observed unsafe emission; see `security-eval/findings/BUG-canonical-shared-terminal-entry.md`. JWT/jq shared-result extent reauthentication is the next lowerer task. Compiler-wide memory safety and optimizer correctness remain unestablished.

The codec/concrete-field/queens wave subsequently merged at `bf2d076b0` on 2026-09-08 at 19:47:12Z, terminal event `mq-45d3d78c217094ae29bf4b89a8a4213e195c31bd`, with workspace tests, prose, compile check and bug-clippy stages green. This supersedes its submission-only state above.

Follow-up candidate `f91205862` adds the catalogued hex byte encoder. `typed_errors::encoding_typed_errors_are_matchable_and_bridge_to_string` passed exactly one selected test (1.60 seconds); its exact deferred-list entry is removed. Worker raw-byte hex source/oracle/Wasm evidence includes empty, 00, ff, mixed bytes and zero leaks. The separate shared-if extent reload at `78e0f8eb3` does not close JWT: the original JWT typed-error test still refuses f23/v441. Debug evidence requires authenticated header loads on incoming arms before nested shared-phi transport. Time separately refuses a Unique/Shared ownership meet for the same RC String extent; neither test is removed from the deferred list.

### Shared extent and nested-continuation repair

The original JWT typed-error probe now passes (4.47 seconds), along with the full queens source/oracle/Wasm probe (0.65 seconds, exact output and no leaks) and duration (0.93 seconds). JWT's exact deferral is removed. The checker transports only exact, constant-equal, or same-object authenticated Length definitions across shared phis; HeaderPrefix and unproved capacity substitutions still reject. Four focused shared-phi tests pass, and the directly executed memory-rule shard passes all 51 tests. Independent review covered both transport and the capacity/length attack regression.

This repair also closes a discovered invalid checker judgment: physical Capacity must not be equated with the conservative capacity bound retained by a shared meet. See `BUG-canonical-shared-capacity-length-equivalence.md`. The broader worker reload patch is preserved, not integrated, at `/private/tmp/rfc0165-jwt-broad-match-preserved.patch`; the final lowerer preserves existing list lanes while reauthenticating string joins, including coalesce. Backend terminal-side classification now respects the enclosing continuation; entry exclusivity also rejects multiple external tail entries. Encoder and plan apply the same structural conditions.

A focused production check of jq now admits; this is not yet jq execution parity or a fresh corpus measurement. Time's Unique/Shared join and URL's tuple binding in a value block remain open. These follow-up changes are candidate evidence until a terminal merge event is recorded.
### jq full-program parity (candidate evidence, 2026-09-08)

- `tests/misc/rfc0165_jq.rs` now pins the actual jq source program through checked source interpretation, the independent SafeWIR oracle, and admitted Wasm. The focused run recorded seven expected output lines and zero live heap objects (`/private/tmp/rfc0165-jq-three-legged.log`, 7.23s). This is focused candidate evidence only; it makes no new census claim.
- Terminal-tail control-flow repair `d3edf2e1c` merged at 2026-09-08T21:20:30Z as queue item `mq-a7998f753674e9f6be9dbf221442c13d7d34bf2f`; its serialized gate completed in 1307s.

### Integration slice: explicit tail returns and ownership joins (2026-09-09)

- Candidate `97e43f55e`, queue `mq-7bb14b192a08a58838e5040b3044bd6b35625b56`; merged green at 2026-09-09T14:31:42Z (252-second gate, no recorded flakes).
- `time_typed_errors_are_matchable_and_bridge_to_string`: passed on the current executable; its deferred entry is removed.
- Same-identity ownership-join probes: 2 passed, including alias/consumption refusals and shared-write refusal.
- Explicit-return tail-manifest probes: 2 passed (statement and value `if`); tuple-binding three-leg probe: 1 passed.
- Independent bounded review found no concrete issue in the join rule or exact-node tail-path mapping. Corpus totals are unchanged here because no fresh census was run.

### URL extent reauthentication (2026-09-09)

- `impl/rfc0165-url-next`: focused-green, not yet confirmed landed.
- `typed_errors::url_typed_errors_are_matchable_and_bridge_to_string`: passed (2.97 seconds); its deferral is removed.
- Dynamic `if` joins reload live bound strings through existing checked headers and guards. Validator rules and list count/capacity identities are unchanged.
- Bounded independent review found no concrete soundness or correctness objection. No fresh census totals or broad soundness claim are inferred from this probe.

### Nullable capability coverage slice (2026-09-09)

- String-join predecessor: `12bd84baf` merged at 15:04:08Z, queue `mq-0c3d012e3312b605411c1100a7bd916e3f1c5515`, full gate green.
- Nullable capability core and lowering: direct checked brands and optional externrefs, no linear-memory authority or nullable host role. Queue pending.
- Focused evidence: two `rfc0165_admitted_launch::nullable_capabilities` tests pass, including expected source/oracle/admitted-Wasm presence and absence; two host-role refusal probes pass; absent-unwrap/kind oracle probe passes; resolved-capability versus guest-tail manifest probe passes.
- Production example: `witchy parity examples/branded_caps/src/branded_caps.witchy` agrees on one output line. This CLI result is interpreter/Wasm evidence, not a separate oracle run for that file.
- Bounded review findings on host roles, expected byte plans, and bare-name tail filtering were corrected and reviewed closed before submission.
- Last full classified census remains 133/163 checks, 76 runnable agreements, zero disagreements, measured before the string-join and nullable-capability fixes. No new total or milestone-completion claim.

### Compiler-service coverage integration (2026-09-09)

- The jq shared-extent repair and nullable-capability slice landed as
  `89e4c095c7e8cd2927aa8f12e8149bd8f5596aec`, queue event
  `2026-09-09T15:54:44Z`, with a green gate and no flakes.
- The compiler-service candidate adds authenticated, stateless footprint/diff
  buffer imports, strict UTF-8, unsigned wasm32 ranges, and all-or-nothing writes.
  Its source/oracle/admitted-Wasm fixture passes with zero oracle and Wasm leaks.
- Existing `caps_audit_example_audits_a_rune_in_witchy` and
  `caps_guard_example_blocks_a_widening_in_witchy` each pass and are removed from
  the deferred list. Both example files also pass CLI parity.
- Fresh candidate census: 138/163 check; 81 runnable examples agree, zero
  disagreements. This is candidate evidence, not a fresh master census.
- Remaining in-scope refusals: coven_check (owned-prefix initialization at a
  dynamic join), four Iter/generator examples, dynamic reflection, File(Read),
  and plugin_host's list of function values. Tasks/channels, Net, regex, and
  opt_mode remain the explicit deferred families.
- Compiler-service queue validation is pending. Coverage does not establish
  compiler-wide memory safety or optimizer correctness.

### Owned-prefix initialization repair (2026-09-09)

- Three focused native tests pass: count growth cannot reuse old initialization,
  moved-from contents lose their summary, and shared-phi completeness requires
  predecessor evidence. Bounded review found no concrete defect in the repair.
- CLI parity passes for hello, wrap, jq, and coven_check. The exact existing
  coven_check test passes and its deferral is removed.
- The last full census above predates this repair; no updated total is claimed.
- Compiler services required generated public ABI catalog refresh after a red
  gate. The exact catalog test now passes; `a8251fb25` is resubmitted as
  `impl/rfc0165-compiler-services-catalog-fix`. The ownership repair stacks on it.

### B1/B7 bounded metadata review, 2026-09-09

The current manifest contains 43 rule identities; the 2026-09-08 review's 39
is historical. `db66d9ec6` added `CapabilityNone`, `CapabilitySome`,
`CapabilityIsPresent`, and `CapabilityUnwrap`: all four manifest backend rows
agree with `rule_backend` as `ScalarGuard`. The RFC-kernel crosswalk has an
existing completeness test against the RFC text (`8c6732f80`), and the reviewed
MakeClosure/VarCall disposition corrections are recorded in `cff3640e9`.
This bounded read-only comparison found no concrete backend/manifest mapping
mismatch; `ClosureSlot` remains a Load/Store subjudgment and `Retain` remains
refused. This is metadata correspondence evidence only, not validation of
checker predicates, interpreter semantics, or emitted instructions. No tests
were run and no new terminal queue evidence was obtained. B1/B7 statuses and
broader semantic/landing evidence obligations remain unchanged.

### Fresh master example census (2026-09-09)

One fresh census at `0a6db8b073492978cc8c4bef1d07637a621776ed` recorded
140/163 passing checks (23 refusals), 83 successful source-interpreter /
SafeWIR-oracle / admitted-Wasm agreements, zero disagreements, and zero
matching-error outcomes. The other 57 passing checks are check-only, not
execution evidence. The census supplies no independent expected-output
assertions. No deferrals were removed; historical measurements and acceptance
statuses remain unchanged.

The six remaining in-scope examples are `dedup`, `generators`, `lazy_fib`,
`pascal`, `dynamic_reflection`, and `plugin_host`. The first four refuse Own
conventions on aggregate/Iter signatures, not Fn storage or Own Fn support
(which has landed). Dynamic refuses its payload layout; plugin_host refuses
typed Console capture storage. Task/channel, Net, regex, and opt_mode refusals
remain separately deferred; check-only results do not close any family.

The matched binary is
`/private/tmp/witchy-rfc0165-current-census-artifacts/bin/debug/witchy`, SHA-256
`527c1b6a9d7e043be2839e4349680e04df702248b1994cf14838644bd24bf6b4`.
Artifacts under `/private/tmp/witchy-rfc0165-current-census-artifacts/` are
`identity.txt`, `summary.txt`, `raw-trace.log`, `parity-stats.txt`,
`census-fail.txt`, and `census-parity.txt`. The isolated worktree is
`/private/tmp/witchy-rfc0165-current-census`; the warmed target is
`/private/tmp/witchy-rfc0165-reflect-specialization-target`, with build log
`/private/tmp/witchy-rfc0165-current-census-build.log`. One matched build and
one existing `scripts/rfc0165-dev/census.sh` run produced this evidence; the
helper's exit 1 is its final grep finding no disagreements. This documentation
handoff adds no rerun, source workaround, or queue evidence.
