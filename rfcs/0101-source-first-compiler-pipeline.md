---
rfc: 0101
title: source-first compiler pipeline
status: implemented
created: 2026-07-20
tracking: "implemented: complete handwritten and comptime-emitted semantics are proved before destructive lowering, production sinks require proof, and focused interpreter/Wasm plus RFC-0080 origin matrices pin the complete boundary"
related:
  - "[0070](0070-0-1-blocking-set.md) (terminal 0.1 decision record and checked-module seam)"
  - "BUG-428 / BUG-429 / BUG-434 / BUG-436 (closed regression classes)"
---

# RFC-0101: source-first compiler pipeline

## Status and implementation progress

Implemented. Source and linked-semantic proof boundaries cover every production
entrypoint. The destructive sequence
is represented by opaque `SourceCheckedModule`, `GeneratorsLoweredModule`,
`AsyncLoweredModule`, and terminal `SourceLoweredModule` typestates. Both the
ordinary linker and compile-time emitted-item normalization enter that sequence
through the non-destructive source check, and executable guards pin the
function signatures and handwritten/generated diagnostic parity. The expanded
link set then crosses an opaque `SemanticallyCheckedSource` boundary; destructive
linked-source lowering cannot accept bare `ResolvedSource`.

The linked-source proof now owns complete declaration and body semantics. It
builds the ordinary runtime projection on a clone, runs the existing type and
trait checker, and only then permits the authoritative expanded source to enter
generator, async, record, trait, and impl lowering. The same proof runs after
compile-time expansion, so emitted bodies cannot join downstream of checking.
The proved runtime projection is the semantic result: production no longer
rechecks the destructively lowered module. Interpreter construction is private,
ordinary runtime and codegen entrypoints require `CheckedModule`, and the two
compiler-generated Glamour executables cross their own private checked wrapper
before backend lowering. Raw `Module` runners and lowerer entries remain
available only behind the explicit test feature.

Implemented evidence:

- `witchy_syntax::source_check::check` is the sole constructor of the initial
  proof; generator, async, and record lowering each require the immediately
  preceding typestate, so no public caller can skip a destructive stage.
- The injected linked-source checker produces the only value accepted by
  `lower_expanded_source`: `SemanticallyCheckedSource`. A source inspection
  guard pins that signature and the proof construction after successful
  checking.
- The production linker checks every initially supplied module, projects only
  the imports introduced by lowering, and retains handwritten source nodes
  through expansion. Comptime normalization applies the same rule to merged
  emitted source.
- Focused tests reject `yield` inside `region:` and async tail `region:` before
  lowering, inspect the destructive entrypoint signatures, and prove emitted
  and handwritten generators receive the same source diagnostic.
- A generator's declared result must have the source shape `Iter(T)`. That
  contract is checked before lowering for both handwritten declarations and
  declarations appended by compile-time expansion; the emitted-source test
  proves an invalid declaration cannot reach generator lowering.
- Checked-link APIs inject the type layer at the resolved expanded-source
  boundary. Duplicate callable, declaration, and parameter contracts now run
  there while generator, async, record, trait, and impl nodes are intact;
  legacy raw linking names its no-op path explicitly. An emitted-generator
  probe proves the injected checker observes generated source and can stop the
  pipeline before lowering.
- The resolved-source proof now validates type names and arity, trait names,
  existential declaration safety, and `PublicState` implementation shape over
  one read-only aggregate of the complete link set. Imported canonical
  identities are therefore checked while async/generator/record/impl nodes are
  intact. `checked_link_rejects_resolved_signature_semantics_before_source_lowering`
  pins a wrong imported async-signature arity to `PipelineStage::Source`.
- The proof now runs complete function-body and method-dispatch inference over
  the production runtime projection before destructive lowering. Focused tests
  pin ordinary and async body failures to `PipelineStage::Source` while source
  nodes are still intact, without reimplementing linker or checker semantics.
- A compile-time expander that appends a type-invalid function body receives
  that same source-stage diagnostic before lowering, proving generated bodies
  re-enter the complete semantic boundary rather than only declaration checks.
- Strict and lenient record lowering are proof-gated at linker, type-checker,
  interpreter, and Wasm assembly entrypoints; projection and record-update
  backend parity tests remain green.
- The source-check proof has no public raw-AST escape. Record lowering returns
  the terminal `SourceLoweredModule`, which is the only public recovery point
  for the runtime `Module`.
- Compile-time emitted generator, async, and record nodes are validated after
  each merge but remain source-shaped through the complete expansion pass.
  The linker then applies the staged lowering sequence once and remaps origin
  ancestry across generator helpers and async segments, rebuilding structural
  child paths against the final lowered item trees. Runtime item-limit
  accounting uses lowered projections without replacing the source AST,
  excludes temporary derive blocks, and keeps projected implicit std imports
  visible to later compile-time blocks. The linker reclassifies an unshadowed
  positional `module.function(...)` or constructor against that final import
  scope before source checking, type resolution, and sealing; lexical locals
  still shadow the module only within their real scopes. Generated async
  lowering receives the same whole-link borrowed-view facts as handwritten
  async code. Expansion and bundled-module discovery complete across the link
  set before handwritten or generated async is destructively lowered, so
  module order cannot hide providers. The fact set includes qualified and
  `from`-imported functions plus the exact source-visible alias selected from
  public inherent methods.
- Expanded bundled-module cache entries retain the lowered module and its
  `OriginTable` as one versioned artifact. Cold and warm links therefore expose
  identical generated-node provenance instead of dropping cached std origins.
- Cold runtime projection restores module-local names throughout generic
  structural-alias bodies before recursively linking the projection, so an
  alias declaration and the aliases it composes re-enter the same namespace.
  `resolved_generic_structural_aliases_survive_cold_runtime_projection` pins
  that behavior without relying on a warm linker cache.
- Functions produced through typed `emit_item` retain an internal typed-item
  ownership marker while the cold projection is rebuilt. That unforgeable
  marker preserves compiler-owned `meta.fresh` bindings across the recursive
  source check; the source parser rejects attempts to spell the marker.
  `typed_generated_fresh_bindings_survive_cold_runtime_projection` pins both
  the generated-item path and the source-forgery rejection.
- Synthetic derive blocks inherit the annotated type's item line as their
  invocation site, so derive-generated implementations retain source ancestry
  through comptime expansion and final lowering.
- The browser compiler and embedded-program compiler now carry the canonical
  `CheckedModule` proof into a dedicated checked-codegen entrypoint. Raw AST
  lowering remains available for lowerer tests and explicitly synthesized
  compiler modules, but these production surfaces can no longer separate a
  successful type check from the module passed to code generation.
- The file compiler's `compile`, `check`, and `emit-wasm` paths retain that same
  proof from filesystem linking through artifact generation. Type-invalid files
  therefore cannot construct the value accepted by their codegen entrypoint;
  focused CLI tests pin both the accepted and rejected boundaries.
- The checked proof no longer implements `Clone` or exposes the redundant
  consuming `into_module`/`into_linked` accessors. Checked interpreter execution
  borrows the proof, while analysis and codegen retain a borrowed view of its
  linked AST. The interpreter's direct constructor is compiled only for its
  internal unit tests. Raw runner and lowerer functions are gated by
  `raw-module-test-api`, which the production root dependency does not enable.
- `link_checked_with` now returns the artifact established by
  `check_linked_source_semantics` without rerunning `typeck::check` after source
  destruction. `checked_link_does_not_recheck_the_lowered_projection` pins both
  the pre-lowering callback and the absence of a post-lowering checker call.
- Compiler-rewritten Glamour applications and generated adapters are combined,
  semantically checked into a private `CheckedGeneratedModule`, and only then
  passed to backend lowering. A focused negative test proves a type-invalid
  generated body is rejected at that boundary rather than reaching codegen.
- `source_only_semantics_survive_the_checked_interpreter_and_wasm_pipeline`
  drives handwritten and comptime-emitted generator, async, region, and impl
  method programs through the production checked resolver, then runs the same
  proof artifact on the interpreter and compiled Wasm backend.
- Generated `witchy test` drivers receive a distinct
  `CheckedTestDriverModule` proof. Its constructor admits only replacement of
  the already checked program's ordinary `main`; it cannot mint the general
  `CheckedModule` accepted by production sinks.
- `rfc0080_diagnostic_and_origin_ancestry_survive_the_checked_pipeline` proves
  typed item and nested expression-hole ancestry remains queryable on the
  checked artifact, executes that artifact on both backends, and pins tagged
  literal definition, invocation, expansion-trace, and hole-local diagnostics.

## Required contract

1. Every user module is semantically checked while generator, async, region,
   impl-method, and other source-only nodes still exist.
2. Compile-time emitted items re-enter exactly that source-checking entrypoint;
   they do not join after a relevant check or lowering pass.
3. Destructive lowering accepts a proof wrapper produced only by the source
   checker. Runtime code generation continues to require the existing checked
   linked-module proof (or its explicit successor).
4. Imported names, standard-library ownership, aliases, traits, and method
   lookup are resolved without destructively replacing the source nodes whose
   rules are being checked.
5. Diagnostic source lines and RFC-0080 origin ancestry survive both boundaries.

## Acceptance evidence

- A phase-order test injects a deliberately invalid source-only construct and
  proves its diagnostic occurs before the corresponding lowering function is
  called.
- A compile-time program emits the same invalid construct and receives the same
  diagnostic and origin ancestry as handwritten source.
- Generator/async region regressions, impl-method shape checks, and generated
  lowering tests remain green on interpreter and compiled backends.
- A source inspection guard proves each destructive lowering entrypoint accepts
  only the source-checked proof wrapper.
- All production compiler entrypoints end at a checked codegen boundary; no raw
  `Module` escape hatch bypasses either proof.

| Required contract | Status | Executable evidence |
|---|---|---|
| 1. Complete user semantics precede destructive lowering | **PROVEN** | `source_only_semantics_survive_the_checked_interpreter_and_wasm_pipeline` covers generator, async, region, and impl-method programs through the production checked resolver and both execution backends. |
| 2. Compile-time items re-enter the same checker | **PROVEN** | The same matrix emits generator, async, and region-bearing bodies at compile time and executes their checked result on interpreter and compiled Wasm; `comptime_emitted_body_reenters_semantic_proof_before_source_lowering` pins the negative source-stage boundary. `typed_generated_fresh_bindings_survive_cold_runtime_projection` proves a typed emitted item retains its unforgeable compiler-ownership marker and fresh binding through a cold recursive projection while source forgery remains rejected. |
| 3. Destructive lowering and production sinks require proof | **PROVEN** | `destructive_source_lowerers_require_the_proof_wrapper`, `checked_link_does_not_recheck_the_lowered_projection`, and `compiler_generated_executable_is_checked_before_lowering` pin the typestate and generated-executable boundaries. `test_driver_proof_allows_only_checked_entry_replacement` proves a post-link test driver cannot rewrite program bodies or mint the general production proof. |
| 4. Resolution preserves source-only nodes | **PROVEN** | `linked_source_proof_retains_and_resolves_source_nodes`, `checked_link_rejects_resolved_signature_semantics_before_source_lowering`, and the checked generator impl-method matrix cover source-shaped local and imported resolution. `resolved_generic_structural_aliases_survive_cold_runtime_projection` proves recursive cold projection restores generic structural-alias declarations and references to one module-local namespace. |
| 5. Lines and RFC-0080 ancestry survive both boundaries | **PROVEN** | `rfc0080_diagnostic_and_origin_ancestry_survive_the_checked_pipeline` checks persistent typed item/expression ancestry, both backends, and exact definition/invocation/nested-hole diagnostics. |

## Remaining migration

None for the source-first production boundary. The explicitly enabled raw test
feature remains test infrastructure. Production `witchy test` drivers use a
separate entry-only proof and task-specific interpreter/codegen sinks.

A labeled module call still requires its import to be present in parsed source;
method labels remain rejected rather than being reinterpreted after an import
appears.
