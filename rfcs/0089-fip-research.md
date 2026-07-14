---
rfc: 0089
title: "Deferred research: fully in-place functional kernels"
status: deferred
created: 2026-07-14
related:
  - "0016 (reference-counted memory - identifies static FIP as an opt-mode rung)"
  - "0026 (unique qualifier - supplies the prospective ownership surface)"
  - "0029 (performance-tier contract - opt mode may reject missed proofs)"
  - "0088 (ownership-aware extraction - a nearer-term imperative optimization)"
tracking: revive only after the workload and prototype gates in this note are met
---

# RFC-0089: Deferred research: fully in-place functional kernels

## Summary

Record fully in-place functional programming (FIP) as a credible `mode opt`
research direction, without making it a release or implementation commitment.
The target is a statically checked class of pure, linear recursive functions
that execute with no allocation/deallocation and constant stack under their
declared ownership preconditions.

This is not RFC-0088's extraction optimization, not ordinary `var` mutation,
and not a promise that arbitrary functional Witchy code becomes allocation-free.
The work remains deferred until a representative Witchy workload and a compiled
prototype justify language and compiler complexity.

## Motivation

Witchy's precise RC and uniqueness facts can reuse storage dynamically. FIP asks
for a stronger, static guarantee: when a recursive transformation consumes
linear inputs and produces an equal-size result, the compiler proves that each
constructor reuses available storage and that recursion uses bounded stack.

That guarantee could make pure tree, parser, and compiler transforms competitive
with carefully written imperative code while preserving value semantics. It is
also expensive: it needs a checkable source contract, constructor/reuse analysis,
and a compiled resource oracle. No current flagship Witchy program demonstrates
that this is more valuable than improving the normal ownership floor.

## Research scope

The first experiment is intentionally narrow:

- pure functions over a linear recursive algebraic data type;
- `unique` inputs or an equivalent unshared proof;
- zipper/tail-recursive structure where bounded stack is demonstrable;
- no escaping closures, async suspension, generator state, capability effects,
  persistent iterators, or host-owned resources;
- equal value results between ordinary and FIP executions.

General recursion, persistent sharing, size-increasing transforms, and automatic
conversion of imperative `var` code are out of scope.

## Required prototype

Revival requires one checked-in, non-toy workload such as a tree rewrite or
parser AST normalization that matters to Witchy itself. The prototype must:

1. express the ownership/reuse obligation without exposing runtime pointers;
2. reject a deliberately shared input and a deliberately non-FIP recursive
   shape with actionable diagnostics;
3. produce the same value as the ordinary implementation;
4. prove zero allocator and deallocator calls in the measured kernel;
5. prove constant stack over increasing input depth; and
6. show an end-to-end improvement after checker and code-size costs.

The interpreter is only the value oracle. It deep-clones values and does not
implement the compiled Perceus machinery, so resource guarantees are measured
in compiled Wasm with allocator/deallocator counters, stack high-water marks,
and generated-WIR inspection.

## Questions before revival

- Is FIP inferred, explicitly requested at a function, or attached to an
  existing `mode opt` performance contract?
- Is the useful guarantee exactly zero allocation/deallocation, or a bounded
  budget that composes better with real programs?
- How are size-changing branches and constructor mismatches diagnosed?
- Does a zipper transformation belong in source, typed IR, or an optimizer?
- Can one proof compose across trait and indirect calls without whole-program
  specialization?
- Does the guarantee outperform normal-mode RC reuse on a real workload?

## Relationship to current work

RFC-0087's uniform `var` cut and RFC-0088's ownership-aware extraction proceed
without FIP. They handle imperative collection updates and runtime ownership
fallbacks. FIP is a stronger opt-mode proof for a narrower pure-functional
class and must not delay 0.1.

RFC-0083 lifetime work may supply shared Facts/CFG machinery, but borrowed views
are neither a prerequisite nor a substitute for linear constructor reuse.

## Alternatives

- **Rely on Perceus reuse and uniqueness facts.** This is the default while the
  research is deferred; it captures much of the benefit without a new theorem
  in the source contract.
- **Promise FIP for every `unique` function.** Too broad: uniqueness alone does
  not prove balanced constructor reuse or bounded recursion.
- **Fold FIP into RFC-0088.** Extraction and FIP have different source classes,
  proof obligations, and resource oracles.
- **Use interpreter allocation counts.** Invalid because the interpreter's
  representation and cloning strategy intentionally differ from compiled Wasm.

## Drawbacks

- A future design may need syntax or diagnostics that normal users never need.
- The proof can reject semantically valid programs for representation reasons.
- Constant-stack conversion may obscure simple recursive source or increase
  generated code.
- Research and benchmark effort may show little gain over the existing RC floor.

## Prior art

[FP2: Fully in-Place Functional Programming](../external-refs/fip-fully-in-place-2023/)
provides the formal target: linear functional programs with no allocation or
deallocation and constant stack under its stated conditions. [Perceus](../external-refs/perceus-2021/)
and Lean 4 provide the dynamic reuse baseline against which a Witchy static
guarantee must earn its complexity.
