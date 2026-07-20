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
| `witchy-runtime` | `caps`, `syntax`, `types`, `wir` | EXTRACT | Compiler implementation and Wasmtime bridging now have distinct `native/compiler.rs` and `runtime/compiler.rs` seams; inject the service implementation from above until post-compilation enforcement has no parser/type/WIR implementation dependency. |
| `witchy-interp` | `caps`, `runtime`, `syntax`, `types` | NARROW | Consume runtime values and policy interfaces without importing the native Wasm sandbox. |

This graph describes allowed coupling, not desired coupling. In particular,
the four compiler-stage dependencies of `witchy-runtime` are transitional and
must shrink during the runtime-kernel phase.

## Trust boundaries

| Boundary | Current owner | Classification | Contract and evidence |
|---|---|---|---|
| Compiler trust boundary | lexer through type checking and lowering/codegen | KEEP | Source safety depends on these stages producing valid, capability-correct Wasm. Differential tests and Wasm validation adjudicate the result. |
| Runtime enforcement TCB | `witchy-runtime::runtime`, `runtime::host::crypto`, confinement/network policy, remaining capability host functions, Wasmtime | EXTRACT | Crypto hash, verify, sign, public-key, and reveal imports now have one registrar and private handlers. Continue moving host families behind task-shaped registrars until the Wasm kernel only coordinates admission, grants, confinement, resource limits, and execution. Capability denial and confinement tests must remain unchanged. |
| Compiler services offered to trusted Witchy programs | `witchy-runtime::native::compiler` owns parsing, docs, checking, and footprint implementation; `witchy-runtime::runtime::compiler` owns Wasmtime import registration, guest decoding, and result staging | EXTRACT | Replace their direct registry lookup with an injected service interface owned above the runtime kernel. Preserve compiler capability checks and exact diagnostics. |
| Shared host ABI and runtime values | `witchy-runtime::{native,value}` plus representation constants in WIR/lowering | CONSOLIDATE | Define one narrow ABI/policy vocabulary. Do not duplicate representation catalogs while breaking the dependency cycle. |

The compiler remains part of Witchy's overall language-security TCB. Isolating
the runtime kernel reduces the code required to enforce an already compiled
program; it does not make compiler correctness untrusted or optional.

## Public and compatibility interfaces

| Surface | Current state | Classification | Planned evidence |
|---|---|---|---|
| Stage crate `pub mod` trees | Most implementation modules are public because the root migration façade exposes them wholesale. | NARROW | Census downstream uses, introduce task-shaped entrypoints, migrate callers, then reduce visibility. Workspace build and stage tests prove each step. |
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
| Argument parsing, help, dispatch | `src/cli.rs` owns help/version presentation plus shared mode, value, and secret decoding; `src/main.rs` still owns command-specific parsing and dispatch | EXTRACT | Extend `src/cli` toward typed commands while preserving golden help/flag/exit behavior byte-for-byte. |
| Project and source loading | `src/source.rs` owns native project discovery, bundled lookup, dependency-aware file loading, linking, checked linking, and expansion; browser resolution in `lib.rs` and LSP loading remain separate adapters | CONSOLIDATE | Introduce filesystem and bundled-source providers so browser and LSP reuse the canonical loader without importing CLI policy. Linking/checking tests cover dependency and diagnostic behavior. |
| Check, expand, docs, capability reports | `src/commands/{frontend,capabilities}.rs` over `src/source.rs` and checked compiler services | NARROW | Preserve the command-service boundary while typed top-level dispatch replaces repeated argv polling. CLI tests lock stdout, stderr, and status. |
| AST to Wasm compilation and cache | `src/commands/compile.rs` | KEEP | One native compilation service owns checked artifact emission, trusted-exe packaging, and embedded/source cache publication. Wasm/parity and CLI subprocess tests guard bytes and behavior. |
| Compiled execution and parity | `main.rs` (`run_linked_compiled`, `run_wasm_*`, `parity_check`) | CONSOLIDATE | One execution service parameterized by grants and observation mode. Differential and exact-error tests remain authoritative. |
| Sandbox, grants, trusted apps | `main.rs`, `trusted_exe`, `witchy-runtime::runtime` | EXTRACT | CLI policy adapters above a small runtime kernel. Existing denial, confinement, trust, and e2e tests remain unchanged. |
| Build-step execution | `main.rs` plus interpreter pipeline | EXTRACT | Dedicated build-step service with explicit environment/grant inputs. Compiled build-step tests preserve behavior. |
| Embedded PM and Coven integration | `main.rs` and self-hosted `projects/` sources | EXTRACT | One native adapter; the Witchy programs remain product source rather than Rust command logic. E2E workflows guard it. |

There must ultimately be one canonical path for loading, checked compilation,
execution, bundled lookup, capability policy, test lifecycle, and WIR helper
registration. Adapters may vary inputs; they must not copy the implementation.

## Test evidence ownership

| Evidence | Current state | Classification | Target boundary |
|---|---|---|---|
| Differential language matrix | `src/example_tests.rs` (about 26k lines) | EXTRACT | Shared parity harness with frontend, types/traits, ownership, collections, capabilities, stdlib, diagnostics, and project-fixture modules. |
| Product workflows | `tests/e2e.rs` (about 5k lines) | EXTRACT | Shared process/registry lifecycle harness with package, trust, Coven, sandbox, publishing, and self-hosted workflow modules. |
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
| Browser/playground modules | Browser resolution calls `linker::std_source`; special modules can enter through root `bundled_module` fallback paths. | EXTRACT | A bundled-module provider that identifies `StandardLibrary` versus `Playground` provenance. |
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
| `src/example_tests.rs` | ~25,900 | EXTRACT | Domain suites over one parity harness. |
| `witchy-lower/codegen/mod.rs` | ~12,100 | EXTRACT | Typed scope/local, representation/layout, helper, capability-import, ownership, and structural-metadata contexts. |
| `witchy-types/typeck.rs` | ~9,700 | EXTRACT | Constraint/inference, checking, diagnostics, capability convention, and checked-output boundaries after active semantic work clears. |
| `witchy-interp/interpreter.rs` | ~7,600 | EXTRACT | Environment, evaluator, reflection conversion, capability adapters, errors, and launch APIs. |
| `witchy-wir/wir_helpers/**` | 34-line facade; domain modules ~18-1,210 | NARROW | The facade now only declares and re-exports responsibility modules. The typed registry, runtime diagnostics, memory/RC, byte buffers, list operations, dictionary projections, numeric operations, string inspection/transformation and host construction/output, encoding/crypto, filesystem/process/environment, networking, VM, and helper-builder domains each have an explicit owner. Dictionary mutation/capacity and registry catalogs remain deliberately cohesive; future work should narrow public exports rather than redistribute their lines. |
| `witchy-types/traits.rs` | ~5,700 | EXTRACT | Validation, method resolution, anonymous unions, refinement/conversion, and monomorphization. |
| `tests/e2e.rs` | ~4,030 plus `tests/support/{coven,package_manager}.rs` | EXTRACT | Coven and package-manager lifecycle support now have explicit owners; continue moving product workflows over the shared driver without weakening assertions. |
| `witchy-syntax/linker.rs` | ~4,900 | NARROW | Module graph/linking versus bundled-source registry and expansion orchestration. |
| `src/main.rs`, `src/{cli,source}.rs`, `src/commands/**` | ~3,208 + ~205 + ~272 + ~900 | EXTRACT | CLI presentation, source loading, frontend/capability commands, and compilation/cache/artifact emission have distinct owners. Continue separating execution/parity, sandbox/grants, build steps, and embedded product adapters from the composition root. |
| `witchy-lower/analysis.rs` | ~4,300 | NARROW | Separate ownership facts/summaries from diagnostics and optimization consumers only where interfaces become smaller. |
| `witchy-runtime/runtime{.rs,/compiler.rs,/host/**}` | ~3,290 + ~105 + ~350 | EXTRACT | Compiler-service imports and crypto host imports have distinct adapter/registrar owners. Continue separating filesystem/process, environment/time/random, network, VM-worker, and core diagnostic/memory families from grant admission and execution policy. |

Active worktree overlap is an admission check, not checked-in ownership. Before
every hotspot slice, run `scripts/worktree-status.sh`, inspect branch diffs, and
state owned files. A worktree's existence is not a lock, but overlapping dirty
or recently active semantic work requires coordination or selection of another
slice. At this snapshot, RFC-0080/0081 worktrees overlap interpreter, traits,
type checking, lowering/codegen, `main.rs`, and the differential matrix, so the
first structural work should use unowned contracts or WIR-helper domains.

## Redundancy and obsolete-path ledger

| Item | Classification | Removal condition |
|---|---|---|
| Root and binary stage-module re-export chains | DELETE | Direct/task-shaped callers migrated; browser API named explicitly. |
| Repeated `link_file*` and module-loading variants | CONSOLIDATE | One provider-driven loader preserves filesystem, dependency, and bundled diagnostics. |
| Checked versus unchecked compilation helpers | CONSOLIDATE | Public execution paths require checked input; unchecked helpers remain only where tests deliberately exercise rejection. |
| Multiple Wasm execution wrappers | CONSOLIDATE | One executor owns instantiation, imports, results, and diagnostic observation. |
| `std_source` used as a general bundled lookup concept | NARROW | Standard and playground provenance represented separately. |
| WIR helper implementation and dependency metadata | NARROW | Responsibility extraction is complete: one typed registry owns dependency metadata and domain modules own constructors. Narrow compatibility re-exports only through a separate resolved-call-site census; do not duplicate the catalog. |
| Repeated test process/temp/server lifecycle code | CONSOLIDATE | Shared harnesses preserve explicit assertions and cleanup behavior. |
| Obsolete runtime-spike and hand-written-Wasm headers | DELETE | Removed from both the native command composition root and the Wasmtime runtime kernel; each module now states its current responsibility. |

## Mergeable execution order

| Slice | Files owned | Depends on | Risk | Acceptance evidence |
|---|---|---|---|---|
| A. Architecture contract | this ledger, `spec/architecture.md`, architecture integration test | none | low | Cargo metadata test, docs review, merge gate |
| B. WIR helper registry/domain extraction | `witchy-wir/src/wir_helpers/**` | A; responsibility extraction complete, compatibility narrowing remains independent | medium | WIR tests, differential matrix, Wasm shard, adversarial diff review |
| C. CLI shell and one command family at a time | `src/main.rs`, `src/cli.rs`, `src/commands/**` | active semantic ownership clear; presentation, source loading, frontend/capability, and compilation slices complete | medium | CLI test inventory, exact output/status tests, focused source-entrypoint shard |
| D. Example-test shared harness and one domain at a time | `src/example_tests/**` plus driver | active semantic ownership clear | low-medium | before/after inventory and focused domain/parity execution |
| E. E2E shared harness and workflow modules | `tests/e2e/**` plus driver | none after ownership check | medium | before/after inventory and e2e shard |
| F. Runtime kernel dependency reduction | `witchy-runtime/src/runtime{.rs,/**}` plus adapter crates and manifests | A, stable ABI contracts; crypto registrar complete | high | dependency test, security denials, confinement, parity, e2e, adversarial review |
| G. Traits/interpreter/codegen contexts | one hotspot and new owner modules per slice | active semantics landed | high | owning stage, differential/Wasm evidence, adversarial review |
| H. Bundled-source provenance | linker/provider/root/browser call sites | loader boundary stable | medium | std resolution, browser, LSP, runnable book, project tests |
| I. Compatibility and stdlib API cleanup | façade or stdlib slice plus migration diagnostics/docs | resolved census and separate approval | high/source-breaking | compiler census, migration tests, runnable docs, both backends |

Each slice updates this ledger when it changes a classification or removes an
edge. Architecture claims move into [architecture.md](architecture.md) only
when the implementation and executable evidence make them true.
