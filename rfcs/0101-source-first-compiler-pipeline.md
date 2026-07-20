---
rfc: 0101
title: source-first compiler pipeline
status: proposed
created: 2026-07-20
tracking: "implementation active: destructive source lowerings use staged proofs, and handwritten plus comptime-emitted source remains unlowered through expansion; full linked semantic checking and trait lowering remain open"
related:
  - "[0070](0070-0-1-blocking-set.md) (terminal 0.1 decision record and checked-module seam)"
  - "BUG-428 / BUG-429 / BUG-434 / BUG-436 (closed regression classes)"
---

# RFC-0101: source-first compiler pipeline

## Status and implementation progress

Proposed, with the first proof boundary implemented. The destructive sequence
is represented by opaque `SourceCheckedModule`, `GeneratorsLoweredModule`,
`AsyncLoweredModule`, and terminal `SourceLoweredModule` typestates. Both the
ordinary linker and compile-time emitted-item normalization enter that sequence
through the non-destructive source check, and executable guards pin the
function signatures and handwritten/generated diagnostic parity.

This does not yet prove the complete contract. Traits and method dispatch are
checked only after linking, and the proof currently covers source-only
generator/async safety rules rather than the full imported-name and type
semantics. Those are the next implementation slices. The RFC remains proposed
until every destructive pass is behind the proof and the linked source checker
owns the complete semantic contract.

Implemented evidence:

- `witchy_syntax::source_check::check` is the sole constructor of the initial
  proof; generator, async, and record lowering each require the immediately
  preceding typestate, so no public caller can skip a destructive stage.
- The production linker checks every initially supplied module, projects only
  the imports introduced by lowering, and retains handwritten source nodes
  through expansion. Comptime normalization applies the same rule to merged
  emitted source.
- Focused tests reject `yield` inside `region:` and async tail `region:` before
  lowering, inspect the destructive entrypoint signatures, and prove emitted
  and handwritten generators receive the same source diagnostic.
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
  accounting uses lowered projections without replacing the source AST, excludes temporary
  derive blocks, and keeps projected implicit std imports visible to later
  compile-time blocks. The linker reclassifies an unshadowed
  positional `module.function(...)` or constructor against that final import
  scope before source checking, type resolution, and sealing; lexical locals
  still shadow the module only within their real scopes. Generated async
  lowering receives the same whole-link borrowed-view facts as handwritten
  async code. Expansion and bundled-module discovery complete across the link
  set before handwritten or generated async is destructively lowered, so
  module order cannot hide providers. The fact set includes qualified and
  `from`-imported functions plus the exact source-visible alias selected from
  public inherent methods.

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

## Remaining migration

The linker still interleaves standard-library discovery, compile-time
expansion, name/type resolution, and destructive transforms. The remaining
work must move those boundaries incrementally without creating a second
semantic pipeline or weakening the existing fail-closed checks. In dependency
order: establish non-destructive linked name/trait/method resolution; move
complete source type checking before trait desugaring; then remove raw
production `Module` escape hatches and promote the RFC only after backend and
diagnostic-origin criteria are green.

Expansion provenance also has two concrete residuals: derive-synthetic
`comptime` blocks must inherit their source type's origin, and cached expanded
std modules must retain the same `OriginTable` as a cold expansion. A labeled
module call still requires its import to be present in parsed source; method
labels remain rejected rather than being reinterpreted after an import appears.
