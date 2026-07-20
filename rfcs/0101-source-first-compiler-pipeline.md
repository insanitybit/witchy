---
rfc: 0101
title: source-first compiler pipeline
status: deferred
created: 2026-07-20
tracking: "deferred architecture residual split from RFC-0070 D6; revive before adding a destructive pre-check lowering, or immediately if a source-only rejection can again be erased before the checked-module boundary"
related:
  - "[0070](0070-0-1-blocking-set.md) (terminal 0.1 decision record and checked-module seam)"
  - "BUG-428 / BUG-429 / BUG-434 / BUG-436 (closed regression classes)"
---

# RFC-0101: source-first compiler pipeline

## Status and revival condition

Deferred. The current pipeline still lowers generators, async functions, and
records before the linked runtime module enters `typeck::check`. The
`CheckedModule` boundary prevents selected production code generators from
omitting that check, and focused guards cover the known erasure failures, but
neither fact proves the stronger source-first invariant.

Revive this RFC before introducing any new destructive transform ahead of the
checked boundary. Revive it immediately if a regression demonstrates that a
diagnostic expressible only on source syntax can again disappear during an
existing transform. A release claim that all source is fully checked before
lowering also requires this RFC to be implemented first.

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

## Why this is deferred

The current linker interleaves standard-library discovery, compile-time
expansion, name/type resolution, and destructive transforms. Moving only one
transform would create a second partial pipeline and could weaken the existing
fail-closed checks. The known security and correctness failures are covered by
executable regressions today, so the safe terminal outcome is to preserve those
guards and revive this architectural cut only under the concrete triggers
above.
