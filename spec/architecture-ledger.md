# Architecture and redundancy ledger

This ledger records the implementation boundaries that exist today and the
small, independently mergeable changes required to make those boundaries
coherent. It is an execution ledger, not an aspirational architecture diagram.
The durable pipeline contract remains in [architecture.md](architecture.md).

Classifications used below:

- **KEEP**: one clear owner already matches the architecture.
- **CONSOLIDATE**: equivalent paths should have one implementation.
- **EXTRACT**: a responsibility needs a named module or crate boundary.
- **NARROW**: callers see more implementation than their contract requires.
- **DELETE**: a compatibility path or residue has no durable owner.

The test `misc::architecture::stage_crates_follow_the_declared_dependency_dag`
reads `cargo metadata` and rejects new stage edges. Removing an allowed edge is
always legal. Adding one requires changing the test and this ledger together,
so architecture growth is explicit and reviewable.

## Stage dependency contract

The root package is the composition layer and may depend on every stage. The
stage crates have this maximum direct-dependency graph:

| Stage | Allowed stage dependencies | Classification | Next boundary change |
|---|---|---|---|
| `witchy-syntax` | none | KEEP | Keep source, AST, diagnostics, expansion, and base lowering self-contained. |
| `witchy-types` | `syntax` | KEEP | Expose checked modules and stable type/witness identities instead of pass internals. |
| `witchy-wir` | `syntax` | NARROW | Retain only the shared diagnostic-template dependency; keep AST and type-system concepts out of WIR. |
| `witchy-caps` | `syntax` | KEEP | Keep source-footprint analysis compiler-side; split reusable runtime policy data only when needed. |
| `witchy-lower` | `syntax`, `types`, `wir` | KEEP | Lower typed source to WIR through explicit checked/type-information inputs. |
| `witchy-runtime` | `caps`, `syntax`, `wir` | EXTRACT | Compiler implementation and Wasmtime bridging now have distinct `native/compiler.rs` and `runtime/compiler.rs` seams; inject the service implementation from above until post-compilation enforcement has no parser/type/WIR implementation dependency. |
| `witchy-interp` | `caps`, `runtime`, `syntax`, `types` | NARROW | Consume runtime values and policy interfaces without importing the native Wasm sandbox. |

This graph describes allowed coupling, not desired coupling. In particular,
the four compiler-stage dependencies of `witchy-runtime` are transitional and
must shrink during the runtime-kernel phase.

## Trust boundaries

| Boundary | Current owner | Classification | Contract and evidence |
|---|---|---|---|
| Compiler trust boundary | lexer through type checking and lowering/codegen | KEEP | Source safety depends on these stages producing valid, capability-correct Wasm. Differential tests and Wasm validation adjudicate the result. |
| Runtime enforcement TCB | the `witchy-runtime::runtime` kernel plus its `runtime::host::*` family registrars, confinement/network policy, Wasmtime | EXTRACT | Every capability host family now registers through a task-shaped `host::*` registrar with private handlers; the Wasm kernel coordinates admission (`link_capability_imports`), grants, VM construction, resource limits, and execution. Capability denial and confinement tests are unchanged. Next: inject the compiler-service interface so post-compilation enforcement drops its parser/type/WIR implementation dependencies. |
| Compiler services offered to trusted Witchy programs | the compiler-native impl (`footprint`/`diff`/`doc`/`try_doc`) lives in `witchy-interp::compiler_natives`, above the kernel; it installs a fn-pointer vtable into `witchy-runtime::native` that both `native::lookup` (interpreter) and the compiled `CompilerServices` default read | KEEP | Done: `witchy-runtime` no longer depends on `witchy-types` (DAG test narrowed); the impl carries the parser/type/caps deps above the kernel. `witchy-caps`/`-syntax`/`-wir` edges remain load-bearing (host address/path policy, `intrinsics`, `layout`). Both backends verified to agree on `compiler.footprint`. |
| Shared host ABI and runtime values | `witchy-runtime::{native,value}` plus representation constants in WIR/lowering | CONSOLIDATE | Define one narrow ABI/policy vocabulary. Do not duplicate representation catalogs while breaking the dependency cycle. |

The compiler remains part of Witchy's overall language-security TCB. Isolating
the runtime kernel reduces the code required to enforce an already compiled
program; it does not make compiler correctness untrusted or optional.

## Public and compatibility interfaces

| Surface | Current state | Classification | Planned evidence |
|---|---|---|---|
| Stage crate `pub mod` trees | Census-narrowed (Lane S, `state/agents/narrow-witchy-*-census.md`): caps/types/wir/interp/lower narrowed and merged (incl. `reachability`/`tagged`/`escape`/`lexer`/`optimize` mods), wir_helpers narrowed earlier; remaining pub surface is resolved out-of-crate consumers. | NARROW | witchy-syntax narrowing pending a fixture-rewrite fix; contested hotspots (typeck/traits/interpreter/codegen/analysis/linker) deferred to their RFC lanes. |
| Root `src/lib.rs` façade | Re-exports nearly every stage module so historical `witchy::ast`, `witchy::typeck`, and similar paths keep compiling. | DELETE | Record intentional browser/library API, migrate internal callers to owning crates or explicit façade functions, remove unused re-exports incrementally. |
| Binary `src/main.rs` façade | Re-exports the root façade again so the monolithic binary can use old crate-relative paths. | DELETE | Command extraction imports its actual dependencies; tests move beside owners. |
| Browser library API | `resolve_std_only_checked`, compile/run playground functions, formatting and documentation entrypoints | KEEP | Treat these task-shaped functions as intentional. Browser build and runnable-book checks guard them. |
| CLI/compiler pipeline API | Linking, checking, compilation, caching, execution, and parity are private functions mixed into `main.rs`. | EXTRACT | Give each workflow one native module and test its output, exit status, diagnostics, and artifacts. |

Compatibility exports are migration aids, not permanent stage APIs. Removal is
incremental: every slice includes a resolved call-site census and deletes the
old path after its callers move.

## Command and pipeline ownership

| Responsibility | Current owner | Classification | Target owner and acceptance evidence |
|---|---|---|---|
| Argument parsing, help, dispatch | `src/cli.rs` owns help/version presentation + flag/secret decoding; `src/commands/dispatch.rs` owns argv routing (lifted from `main.rs`). `main.rs` is a 220-line composition root (install services + call dispatch). | KEEP | Golden help/flag/exit behavior preserved byte-for-byte (CLI subprocess suite). |
| Project and source loading | `src/source.rs` owns native project discovery, bundled lookup, dependency-aware file loading, linking, checked linking, and expansion; browser resolution in `lib.rs` and LSP loading remain separate adapters | CONSOLIDATE | Introduce filesystem and bundled-source providers so browser and LSP reuse the canonical loader without importing CLI policy. Linking/checking tests cover dependency and diagnostic behavior. |
| Check, expand, docs, capability reports | `src/commands/{frontend,capabilities}.rs` over `src/source.rs` and checked compiler services | NARROW | Preserve the command-service boundary while typed top-level dispatch replaces repeated argv polling. CLI tests lock stdout, stderr, and status. |
| AST to Wasm compilation and cache | `src/commands/compile.rs` | KEEP | One native compilation service owns checked artifact emission, trusted-exe packaging, and embedded/source cache publication. Wasm/parity and CLI subprocess tests guard bytes and behavior. |
| Compiled execution and parity | `src/commands/execution.rs` (`run_linked_compiled`, `parity_check`) + `src/commands/wasm_exec.rs` (`run_wasm_*`, trusted-app launch) | KEEP | Extracted from the composition root; differential and exact-error tests remain authoritative. |
| Sandbox, grants, trusted apps | `src/commands/sandbox.rs`, `trusted_exe`, `witchy-runtime::runtime` | KEEP | Policy adapters extracted from the composition root above the runtime kernel; denial/confinement/trust/e2e tests unchanged. |
| Build-step execution | `src/commands/build_steps.rs` | KEEP | Dedicated build-step service extracted from the composition root; compiled build-step tests preserve behavior. |
| Embedded PM and Coven integration | `src/commands/embedded_pm.rs` + self-hosted `projects/` sources | KEEP | Native launcher extracted from the composition root; the Witchy programs remain product source. E2E workflows guard it. |

There must ultimately be one canonical path for loading, checked compilation,
execution, bundled lookup, capability policy, test lifecycle, and WIR helper
registration. Adapters may vary inputs; they must not copy the implementation.

## Test evidence ownership

| Evidence | Current state | Classification | Target boundary |
|---|---|---|---|
| Differential language matrix | `src/example_tests.rs` (~25k lines) + `src/example_tests/` domain submodules | EXTRACT | The module root owns the shared parity harness (`link_run`/`wasm_run`/`interp`/…); six domain submodules extracted so far (concurrency, traits, records, comptime, quote, ownership — 99 tests, byte-identical, inventory-accounted). Continue moving topical runs (capabilities, pm/coven, crypto/net, stdlib) per the handoff; the interleaved front of the file is last, per-test. |
| Product workflows | `tests/e2e.rs` (48 lines) + `tests/e2e/` domain modules over `tests/support/*` | EXTRACT | Decomposed into 8 domain modules (trust_and_publishing, capability_widening, resolution, build_steps, example_workspaces, coven_web, pm_coven_lifecycle, sandbox_grants); 82-test inventory byte-identical. Note: the merge gate does not run e2e (BUG-577/578), so moves are compile+inventory adjudicated. |
| CLI tests | Presentation and secret-decoding tests live beside `src/cli.rs`; expansion tests live beside `src/source.rs`; four command/service test modules remain inline in `main.rs` | EXTRACT | Move each remaining suite with its command/service owner; preserve assertions and test intent. |
| Stage tests | Unit/integration suites under each owning crate | KEEP | New regressions live at the narrowest stage, plus differential evidence for observable semantics. |
| Browser/Glamour/misc drivers | Consolidated integration binaries with domain files | KEEP | Preserve the compile-throughput-oriented driver pattern. |

Before moving a test corpus, record the fully qualified test inventory. After
the move, account for every addition, deletion, and intentional path rename;
test counts alone are not coverage evidence.

The first CLI slice accounts for its complete test movement:

- `version_tests::{local_builds_report_the_package_version,release_builds_report_the_exact_embedded_commit}`
  moved to `cli::version_tests::*`.
- `cli_flag_tests::{mode_flags_before_the_file_are_global,mode_flags_in_guest_argv_are_ignored}`
  moved to `cli::cli_flag_tests::*`.
- `cli::secret_arg_tests::{inline_secret_preserves_equals_and_use_only,use_only_is_only_an_exact_trailing_modifier}`
  are new unit contracts.
- `cli_subcommands::{cli_help_version_and_bare_invocation_are_stable,missing_secret_values_are_exact_usage_errors,secret_file_argument_preserves_exact_bytes}`
  are new process-level contracts for stdout, stderr, exit status, and secret-file bytes.

The source-loading slice preserves all three expansion tests while moving their
ownership paths:

- `expand_command_tests::expand_file_prints_generated_items_without_comptime_blocks`
  moved to `source::tests::expand_file_prints_generated_items_without_comptime_blocks`.
- `expand_command_tests::rfc0081_expand_preserves_existential_types_and_calls`
  moved to `source::tests::rfc0081_expand_preserves_existential_types_and_calls`.
- `expand_command_tests::expand_file_uses_sibling_modules_for_imported_tags`
  moved to `source::tests::expand_file_uses_sibling_modules_for_imported_tags`.

## Bundled and product sources

| Source class | Current state | Classification | Target boundary |
|---|---|---|---|
| `std/` | Standard library embedded by the linker and documented from source comments | KEEP | A standard-library registry with generated API docs. |
| Browser/playground modules | `linker::std_source` is now a pure standard-library registry; `linker::playground_source` owns the glamour/markdown experiments; `linker::bundled_source` (std ∪ playground) is the general import resolver. Browser/native/LSP resolution uses `bundled_source`. | KEEP | Provenance is now explicit (StandardLibrary vs Playground); std_source no longer conflates glamour. |
| `examples/` | Teaching programs and executable parity evidence | KEEP | Remain outside bundled library identity. |
| `projects/pm`, `projects/coven`, `projects/coven-web` | Self-hosted ecosystem applications | KEEP | Product/project registry and explicit support classification. |
| `projects/glamour` and experiments | Product experiments also used by browser/e2e fixtures | NARROW | Keep experimental status explicit; inject into playgrounds without calling it standard library. |
| Tracked generated-looking fixtures | Mixed placement, including source-controlled fixture output | CONSOLIDATE | Inventory before moving; distinguish retained evidence, source fixture, reproducible output, and accidental residue. |

Standard-library method aliases and other duplicate public spellings require a
type-resolved census. Any removal changes source compatibility and therefore
lands as an explicit migration slice with diagnostics and executable docs, not
inside a source move.

## Hotspots and decomposition ledger

Line counts are orientation evidence, not design targets. A file leaves this
list only when responsibilities and interfaces improve, not when its lines are
distributed mechanically.

| Hotspot (master snapshot) | Size | Classification | Responsibility boundary |
|---|---:|---|---|
| `src/example_tests{.rs,/**}` | ~25k + 6 domain modules | EXTRACT | Root owns the parity harness; 6 domains extracted (99 tests). ~825 tests remain (handoff: `state/agents/example-tests-decomposition-handoff.md`). |
| `witchy-lower/codegen/mod.rs` | ~7,345 + ten modules | EXTRACT | Reduced 12,084 → ~7,345. Responsibility modules under `codegen/`: `assembly`, `builtins`, `helpers`, `passes`, `types`, `loans` (loan-root/event collection), `type_vars` (type-variable/devirtualization analysis), `expr_lower` (the ~2k-line `lower_expr` expression→WIR method), `match_lower` (pattern/match lowering), and `block_lower` (block-statement lowering) — each an `impl Codegen` continuation or free-function owner. What remains in mod.rs is the cohesive lowering core: the `Codegen` struct + its private lowering context, function/signature emission, and the type/representation/layout helpers that share that context intimately — JUSTIFIED-CORE for size. The one open refinement is grouping the `Codegen` struct's *state fields* into typed scope/local, representation/layout, capability-import, ownership, and structural-metadata contexts (a typed-context change, not a further file split). |
| `witchy-types/typeck.rs` | ~8,268 + five modules | EXTRACT | Reduced 9,727 → ~8,268. Extracted side-concern passes under `typeck/`: `coverage` (pattern-exhaustiveness), `cap_rights` (capability rights-set parsing), `uniqueness` (duplicate-declaration checks), `existential` (RFC-0081 `dyn Trait` dyn-safety validation), and `compiler_syntax` (`meta.*` compile-time-syntax gating). What remains is the cohesive type-checking/inference engine — the mutually-recursive check/infer/unify judgment that is one responsibility; every separable side-concern pass has been lifted out. JUSTIFIED-CORE for size. |
| `witchy-interp/interpreter{.rs,/**}` | ~2,114 + eleven modules | KEEP | Reduced 6,899 → ~2,114 (a 69% reduction). Responsibility modules under `interpreter/`: `reflection` (syntax-reflection payload decoding), `capability_values` (Dir/File/Net capability adapters), `ast_walk` (analysis helpers), `assignment_plan` + `places` (memory-place planning, capture, read/store, write-back), `environment` (the lexical environment), `tail_analysis` (tail-edge/tail-position analysis), `value_ops` (pattern match, binary ops, comparison, native conversion), `calls` (call/closure evaluation dispatch), `builtins` (the ~2.5k-line builtin-call dispatcher), and `runners` (the public `run_*` execution façade). What remains in interpreter.rs is the core evaluator (`eval`/`eval_block`/`eval_tail_expr`/`eval_function_block`), value/error types, the `Interpreter` struct + construction, and rendering — a cohesive evaluator core. |
| `witchy-wir/wir_helpers/**` | 34-line facade; domain modules ~18-1,210 | NARROW | The facade now only declares and re-exports responsibility modules. The typed registry, runtime diagnostics, memory/RC, byte buffers, list operations, dictionary projections, numeric operations, string inspection/transformation and host construction/output, encoding/crypto, filesystem/process/environment, networking, VM, and helper-builder domains each have an explicit owner. Dictionary mutation/capacity and registry catalogs remain deliberately cohesive. The public surface is narrowed after a resolved-call-site census (`scratch/wir-helper-census.md`): only the typed registry entry points (`wir_helper`, `WirHelperSpec`), `abort_nodes`, the memory check gates (`heap_check_enabled`, `type_check_enabled`), the VM trampolines (`galloc_helper`, `call_idx_helper`, `call2_helper`), and `print_str_helper` remain `pub`; every other constructor is `pub(crate)` (in-crate test consumers) or module-tree-local, and the facade globs carry matching visibility. |
| `witchy-types/traits.rs` | ~4,296 + three modules | EXTRACT | Reduced 5,524 → ~4,296. Extracted under `traits/`: `conversions` (From/TryFrom error-conversion rewrite), `anon_union` (anonymous-union impl synthesis), and `mono` (the `impl Mono` substitution-directed monomorphization walk). What remains is trait validation and method resolution — the core RFC-0046 typed-dispatch judgment. JUSTIFIED-CORE for size. |
| `tests/e2e{.rs,/**}` | 48 + 8 domain modules over `tests/support/*` | KEEP | Decomposed into 8 domain modules with byte-identical inventory; support helpers own lifecycle/registry/sandbox. |
| `witchy-syntax/linker.rs` | ~4,900 | NARROW | Module graph/linking versus bundled-source registry and expansion orchestration. |
| `src/main.rs` (composition root) + `src/{cli,source}.rs` + `src/commands/**` | ~220 + ~205 + ~272 + ~4,600 (10 command modules) | KEEP | main.rs reduced 2,783 -> 220 (a genuine composition root): dispatch, execution, wasm-exec, sandbox/grants, build-steps, embedded-pm, frontend, capabilities, compile all own distinct `commands/*` modules. |
| `witchy-lower/analysis.rs` | ~4,300 | NARROW | Separate ownership facts/summaries from diagnostics and optimization consumers only where interfaces become smaller. |
| `witchy-runtime/runtime{.rs,/compiler.rs,/host/**}` | ~1,195 + ~105 + ~2,690 | EXTRACT | Every host-import family (crypto, clock/env/rand, network, filesystem/exec, build-step, VM-worker, secret, console, staging) has a distinct registrar owner under `runtime/host/`. The kernel retains grant admission, VM construction, the checked-heap shadow, the inline field-length stubs, the shared guest-memory ABI helpers, and execution policy. Remaining EXTRACT work is the injected compiler-service interface, not further family moves. |

Active worktree overlap is an admission check, not checked-in ownership. Before
every hotspot slice, run `scripts/worktree-status.sh`, inspect branch diffs, and
state owned files. A worktree's existence is not a lock, but overlapping dirty
or recently active semantic work requires coordination or selection of another
slice. The RFC-0080/0081 semantic worktrees that previously overlapped
interpreter, traits, type checking, and lowering/codegen are no longer active,
so hotspot decomposition on those files is unblocked and substantially executed
(see the rows above); each slice is a verbatim, single-responsibility move
validated on the serialized gate.

## Redundancy and obsolete-path ledger

| Item | Classification | Removal condition |
|---|---|---|
| Root and binary stage-module re-export chains | DELETE | Direct/task-shaped callers migrated; browser API named explicitly. |
| Repeated `link_file*` and module-loading variants | CONSOLIDATE | One provider-driven loader preserves filesystem, dependency, and bundled diagnostics. |
| Checked versus unchecked compilation helpers | CONSOLIDATE | Public execution paths require checked input; unchecked helpers remain only where tests deliberately exercise rejection. |
| Multiple Wasm execution wrappers | CONSOLIDATE | One executor owns instantiation, imports, results, and diagnostic observation. |
| `std_source` used as a general bundled lookup concept | DONE | `std_source` = stdlib only; `playground_source` = experiments; `bundled_source` = the general resolver. Standard and playground provenance are represented separately. |
| WIR helper implementation and dependency metadata | NARROW | Responsibility extraction and compatibility narrowing are complete: one typed registry owns dependency metadata, domain modules own constructors, and the helper surface is reduced to nine `pub` items via a resolved-call-site census (`scratch/wir-helper-census.md`) — no catalog duplication. New helpers default to `pub(super)`/`pub(crate)`; add `pub` only with an out-of-crate call site. |
| Repeated test process/temp/server lifecycle code | CONSOLIDATE | Shared harnesses preserve explicit assertions and cleanup behavior. |
| Obsolete runtime-spike and hand-written-Wasm headers | DELETE | Removed from both the native command composition root and the Wasmtime runtime kernel; each module now states its current responsibility. |

## Mergeable execution order

| Slice | Files owned | Depends on | Risk | Acceptance evidence |
|---|---|---|---|---|
| A. Architecture contract | this ledger, `spec/architecture.md`, architecture integration test | none | low | Cargo metadata test, docs review, merge gate |
| B. WIR helper registry/domain extraction | `witchy-wir/src/wir_helpers/**` | A; responsibility extraction and compatibility narrowing complete | medium | WIR tests, differential matrix, Wasm shard, adversarial diff review |
| C. CLI shell and one command family at a time | `src/main.rs`, `src/cli.rs`, `src/commands/**` | active semantic ownership clear; presentation, source loading, frontend/capability, and compilation slices complete | medium | CLI test inventory, exact output/status tests, focused source-entrypoint shard |
| D. Example-test shared harness and one domain at a time | `src/example_tests/**` plus driver | active semantic ownership clear | low-medium | before/after inventory and focused domain/parity execution |
| E. E2E shared harness and workflow modules | `tests/e2e/**` plus driver | none after ownership check | medium | before/after inventory and e2e shard |
| F. Runtime kernel dependency reduction | `witchy-runtime/src/runtime{.rs,/**}` plus adapter crates and manifests | A, stable ABI contracts; crypto registrar complete | high | dependency test, security denials, confinement, parity, e2e, adversarial review |
| G. Traits/interpreter/codegen contexts | one hotspot and new owner modules per slice | active semantics landed; interpreter reflection decoding boundary complete | high | owning stage, differential/Wasm evidence, adversarial review |
| H. Bundled-source provenance | linker/provider/root/browser call sites | loader boundary stable | medium | std resolution, browser, LSP, runnable book, project tests |
| I. Compatibility and stdlib API cleanup | façade or stdlib slice plus migration diagnostics/docs | resolved census and separate approval | high/source-breaking | compiler census, migration tests, runnable docs, both backends |

Each slice updates this ledger when it changes a classification or removes an
edge. Architecture claims move into [architecture.md](architecture.md) only
when the implementation and executable evidence make them true.
