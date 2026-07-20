---
rfc: 0101
title: source-first compiler pipeline
status: proposed
created: 2026-07-20
tracking: "implementation active: generator, async, and record lowering require a non-destructive source-check proof on handwritten and comptime-emitted paths; full linked semantic checking and trait lowering remain open"
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
- The production linker checks every initially supplied module before those
  lowerings, while comptime normalization checks the merged emitted module
  before lowering generated generator/async items.
- Focused tests reject `yield` inside `region:` and async tail `region:` before
  lowering, inspect the destructive entrypoint signatures, and prove emitted
  and handwritten generators receive the same source diagnostic.
- Strict and lenient record lowering are proof-gated at linker, type-checker,
  interpreter, and Wasm assembly entrypoints; projection and record-update
  backend parity tests remain green.
- The source-check proof has no public raw-AST escape. Record lowering returns
  the terminal `SourceLoweredModule`, which is the only public recovery point
  for the runtime `Module`.

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
