# RFC-0167 implementation and adoption ledger

Baseline: `418a69b9ca75ae70fae5e96f797057e161f93aeb` (2026-09-16).
The RFC is a complete target-design draft, not an implementation closeout.
All rows below are open until their exact current production paths are tested.
Existing tests and historical reports remain useful evidence inputs, not blanket
proof of this new contract. Optimization expansion is outside this cut.

## Dependency graph

```text
review/adopt contract
  -> syntax + migration + tooling
  -> ownership/reference/callable type contracts
       -> normal/opt entry ABI -> canonical lowering + runtime
       -> interpreter semantics
  -> independent expected-result and diagnostic matrices
  -> installed workflows + current spec/book -> implementation closeout
```

Syntax/tooling, test specification, and frontend contract work can proceed in
parallel once the contract is adopted. Backend tracks share a frozen call
envelope: owned result, ordered var outputs, owner/loan relations, affine state,
source mode, and authenticated authority. No optimization is needed for soundness.

| ID | Contract and necessary evidence | Baseline gap / state |
|---|---|---|
| A | Review and adopt the exact RFC choices; record unresolved objections before changing the compiler | OPEN: proposed design, no implied user approval of every choice |
| B | Canonical parser/formatter/LSP/Tree-sitter/reflection agree; legacy borrow forms only in migration grammar | OPEN: `params` and `ty_inner` in syntax parser still admit legacy forms |
| C | `()`/omitted-unit signatures, inferred private returns, layout boundaries, explicit `let _ =` discard; negative old spellings | OPEN: inherited documents contradict one another; audit source corpus before migration |
| D | `own` consumes in both modes, including shared/exclusive handles; missing result annotation cannot change consumption | OPEN: `transfer_owned_argument_binding` conditions ordinary consumption on opt/must; exclusive-handle repeated-own probes were accepted by frontend |
| E | Unique root versus nested proofs; normal copy repair; opt rejection; no duplicate unique result plus write-back | OPEN: broad unique forms remain outside canonical v1 admission |
| F | Frozen move and whole replacement work; deep mutation rejects; qualifier/reference combinations follow one matrix | OPEN: old blanket frozen `own`/`var` rejection and inconsistent qualified-handle acceptance need changes |
| G | Shared/exclusive loans through nominal lifetimes, aggregates, generics, traits, closures, indirect calls, loops, projections, errors | OPEN: canonical route rejects mutable references and many aggregate/callable forms |
| H | `var` reference handle replacement distinct from `*r` referent write; shortened reborrow and consuming own cases | OPEN: source access ABI currently conflates exclusive writeback and referent access |
| I | Normal -> opt and opt -> opt direct/method/generic/trait/function-value calls; imported mode preserved; aliased snapshots unchanged | OPEN: reference-free examples are insufficient to prove the full precondition/return envelope |
| J | Value-only public boundary rejects nested hidden refs; opt callbacks cannot erase source-contract provenance | OPEN: complete callback/import/adaptor matrix required |
| K | Must/once/pure, Dynamic, capabilities and suspension compose; cancellation cannot drop must captures | OPEN: retained suites must be replayed against the new conventions, including negative cases |
| L | Region copy-out versus opt no-hidden-copy rules; frozen/unique/ref interactions; packed admitted layouts | OPEN: don't inherit the old permanent/unstable region contradiction or claim all layouts compile |
| M | Typed-only code generation, authenticated Dynamic and sealed construction; ordinary and generated code reject retired syntax | OPEN: remove raw emission only with typed replacements for all callers |
| N | Interpreter and unoptimized canonical Wasm match independent expected results for every supported row; optimized output optional cross-check | OPEN: front-end `check` is not compile/run or parity evidence |
| O | Installed `check`, `fmt`, compile, run and parity workflows; migrate std/book/examples atomically with compiler cut | OPEN: documentation-only consolidation leaves current source behavior unchanged |

## Required small counterexamples

- `take(own r: &'a mut String)` followed by a second `take(r)` rejects; ordinary
  `edit(r); edit(r)` reborrows and succeeds when the first loan ended.
- A shared reference may be returned under its input lifetime, never by returning
  a reference to a local or region-born owner.
- Moving an owned shell with `&'a String` fields does not mutate their referents;
  changing its unrelated scalar field is valid.
- `own frozen String` transfers; `var frozen String` replaces; a field/index
  mutation and `&mut frozen String` reject.
- Normal `snapshot = xs; fast.set_first(xs)` leaves `snapshot` unchanged; opt
  code missing the unique proof rejects before execution.
- Repeat these through labels, imports, function values, generics, traits,
  early `?`, inferred and explicit unit returns, and traps.

## Documentation consolidation completion

The docs task is complete when retired bodies are removed, their exact prior
commit and replacement are indexed, internal links resolve, active evidence is
preserved, and RFC hygiene checks pass. That is distinct from rows A-O above.

## Inherited evidence index (historical, not RFC-0167 acceptance)

The following records are preserved from the retired concurrency/generator
capstones. Their PROVEN labels refer to their recorded commits and original
contracts, not to this RFC or all current canonical compiler paths. Re-run them
before using them to close a row above. Other historical companion ledgers and
machine-readable censuses remain in place for the same reason.

### Historical 0129 evidence

| Row | State | Executable evidence |
| --- | --- | --- |
| 1 | PROVEN | `tests/misc/rfc0129_async_lowering.rs::rfc0129_acceptance_row_1_async_lowering_preserves_source_contracts_on_both_backends` runs mutable loop state, repeated suspension, early return, and a source trap on the interpreter and compiled Wasm. It also inspects the typed suspension-carrier catalog for the exact live `Int` slots and proves the trap retains the source callable and line without leaking generated `__async_` identities. |
| 2 | PROVEN | `tests/misc/rfc0129_channel_contract.rs::rfc0129_acceptance_row_2_typed_bounded_selected_cancelled_and_joined_channels_agree` executes `Int` and `String` channels, capacity-one backpressure, first-ready selection, quiescent closure, cancellation of a parked child, and structured scope/join. It requires the exact 18-line schedule to agree on the interpreter and compiled Wasm. |
| 3 | PROVEN | `tests/rfc0129_schedule_parity.rs::rfc0129_acceptance_row_3_deterministic_schedule_backends_agree` executes the canonical 13-line schedule from `tests/fixtures/rfc0129_deterministic_schedule.witchy` on compiled Wasm first and then the interpreter. It covers bounded backpressure, quiescent join release, deterministic select order and closure, cancellation, retained-sender quiescence, structured joins, nominal aggregate/list carriers, and a distinct string carrier. Rows 2 and 3 landed together at `5c040cc0` after their focused 99-test coordinator gate passed. |
| 4 | PROVEN | `tests/rfc0129_scalar_executor_performance.rs::million_message_scalar_executor_meets_resumption_cost_and_allocation_gate` executes the real compiled-Wasm producer/consumer benchmark for one million messages, checks its exact fold, proves the scalar entry bypasses `task.run`, requires zero GC task-heap capacity, rejects 100 or more linear allocations, and enforces a 300 ns/message median ceiling. `aggregate_channel_gc_heap_capacity_is_flat` forces the GC-backed scheduler with nominal `Packet(Int, Int)` messages, checks both folds, and pins the same 65,536-byte backing capacity at 100 and 10,000 messages. Together they show that the promoted scalar and aggregate paths do not grow a continuation chain with the message count. |
| 5 | PROVEN | `tests/misc/rfc0129_negative_matrix.rs::rfc0129_acceptance_row_5_rejects_invalid_concurrency_sources_before_lowering` rejects reference escape across suspension, capability transfer through a worker result, dedicated async-trait syntax, and discarded task results. Every case checks source-level diagnostic fragments and proves rejection occurs at parse or the pre-lowering source semantic boundary. |
| 6 | PROVEN | `tests/misc/rfc0129_cooperative_worker_boundary.rs::rfc0129_acceptance_row_6_cooperative_and_parallel_maps_use_distinct_boundaries` executes both APIs on the interpreter and compiled Wasm. It proves that `chan.par_map` keeps its explicitly passed `Console` in one VM with zero worker imports; pure `vm.par_map` crosses the two-phase `vm_par_map_run`/`vm_par_map_write` worker boundary; a capability-capturing callback stays in the parent VM; and `vm.with_dir` grants exactly one explicit `Dir`. `examples/channels/src/cooperative_map.witchy` and `worker_vm_map.witchy` are separate runnable examples, and their README records the distinct API and cost contracts. The exact measurement is the stable zero-versus-two worker-host-interface boundary, not an elapsed-time claim. The merge-coordinator landing at `22968974` passed all 111 selected tests, including this test and the example parity/validation sweeps. |
| 7 | PROVEN | `scripts/installed-bounded-channels-smoke.sh --witchy <binary>` copies the public binary into a fresh install root, resolves only that copy on `PATH`, and uses `env -i` plus fresh home, temporary, cache, and working directories for `check`, `build`, and `run`. It creates the capacity-one `bounded_channels` project and checks its exact deterministic drain output without repository source, standard-library, package, or cache state. `scripts/release-smoke.sh` invokes the same harness on the extracted archive, and `.github/workflows/release.yml` runs it for every supported native release target. The initial installed-workflow slice passed the 584-test coordinator batch landed at `c8ff194a`; the environment-scrub hardening passed the 111-test coordinator gate landed at `22968974`. |

### Historical 0130 evidence

Rows 1-3 are promoted by the owned-frame implementation and its executable
mutation, control-flow, effect-count, frame-shape, and scaling evidence.

| row | status | executable evidence |
| --- | --- | --- |
| 1 mutation and control flow | **PROVEN** | `rfc0130_generator_owned_frame::rfc0130_rows_1_to_3_generators_resume_owned_frames_once_with_linear_work_on_wasm` exercises mutable top-level and inherent generators with loop and branch control on compiled Wasm. `rfc0130_generator_liveness`, `rfc0130_generator_nested_cfg`, `rfc0130_generator_match_cfg`, `rfc0130_generator_match_binding`, and `rfc0130_generator_while_let_cfg` preserve live locals and branch, match, and `while let` control flow across suspension on both backends. |
| 2 one-time body segments and effects | **PROVEN** | The rows 1-3 fixture checks exact pre-yield capability output for every yielded item, including exhaustion. `generator_owned_frame_delays_post_yield_effect_until_resume`, `generator_direct_multi_yield_loop_resumes_each_segment_once`, and the paired CFG fixtures prove that post-yield, multi-phase, conditional, match, and loop effects resume once without replay. |
| 3 linear pulls and bounded frame storage | **PROVEN** | `rfc0130_generator_scaling::rfc0130_acceptance_row_3_owned_frame_pulls_are_linear_and_storage_is_bounded_on_wasm` compares 8 and 64 compiled-Wasm pulls and pins a fixed direct five-lane lazy-entry carrier. The rows 1-3 fixture independently checks fixed carrier dimensions and live cells, deterministic counters, exactly linear effect count, and no pull-dependent RC, bump-allocation, or reown calls. |
| 4 source diagnostics | **PROVEN** | `rfc0130_surface::rfc0130_row_4_generator_failures_name_source_syntax` covers returned values, yielded-value mismatch, borrowed frame/element types, region escape, unsupported trait syntax, and rejection of generated helper names. `generators::target_availability_tests::residual_suspension_cfg_fails_closed_instead_of_replaying` pins the source-located residual-CFG boundary and proves replay is not offered as a fallback. |
| 5 adapters and collection parity | **PROVEN** | `rfc0130_adapter_collection::rfc0130_row_5_iterator_adapters_and_from_iterator_collections_agree_on_both_backends` composes representative lazy adapters, selects `List`, `Dict`, `Set`, and `String` collection through expected-type `FromIterator` dispatch, bounds an infinite adapter pipeline, and short-circuits an infinite `find` identically on the interpreter and compiled Wasm. |
| 6 syntax preservation | **PROVEN** | `rfc0130_surface::rfc0130_row_6_formatter_reflection_docs_and_editor_surfaces_preserve_generators`, `web/witchy-runtime/witchy-highlight.test.mjs`, and the Zed extension's pinned Tree-sitter revision preserve `gen fn`/`yield` through formatting, AST and quoted-item reflection, generated docs, and editor highlighting. |
| 7 clean installed example | **PROVEN** | `scripts/release-smoke.sh`, invoked by the release workflow on every supported native archive, creates a fresh project through the installed public CLI and executes both `finite(4)` and `naturals().take(5)` with an isolated HOME, cache, working directory, and PATH containing only the extracted toolchain. |
