---
rfc: 0127
title: "Value ownership, mutation, and the opt-mode access contract"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical consolidation RFC. Value semantics and RFC-0122 references are implemented and proven. Promotion requires a curated installed opt-mode workflow and measured claims for each promised performance class; future representation work remains explicit rather than weakening the contract."
predecessors:
  - "[0024](0024-unified-facts-lattice.md), [0025](0025-frozen-deep-immutability.md), [0026](0026-unique-qualifier.md), [0033](0033-place-based-uniqueness.md) (ownership facts and qualifiers)"
  - "[0028](0028-ergonomic-mutable-value-semantics.md), [0043](0043-declared-mutation-writeback.md), [0064](0064-complete-mutation-classification.md), [0087](0087-fused-mutators.md) (mutation and write-back)"
  - "[0029](0029-performance-tier-contract.md), [0030](0030-perf-correctness-infra.md) (normal/opt tiers and evidence)"
  - "[0034](0034-closing-the-compute-gap.md), [0051](0051-memory-safety-invariants.md), [0062](0062-closure-escape-elision.md) (runtime cost model and allocation invariants)"
  - "[0088](0088-ownership-aware-extraction.md), [0089](0089-functional-in-place.md), [0090](0090-proper-tail-calls.md) (no-copy extraction and state kernels)"
  - "[0083](0083-opt-mode-lifetimes.md), [0110](0110-opt-ownership-access-abi.md), [0111](0111-cross-boundary-specialized-layouts.md), [0112](0112-borrowed-aggregate-types.md), [0122](0122-uniform-borrow-relations.md) (references, layouts, and access ABI)"
related:
  - "[0114](0114-must-consume-obligations.md) (deferred linear resource obligations)"
---

# RFC-0127: Value ownership, mutation, and the opt-mode access contract

## Decision

Witchy has one value-semantic language with two source modes over one ownership
analysis.

- Normal mode presents ordinary values and `let`/`var`/`own` calls. A missed
  uniqueness proof may copy but cannot change the result or reject otherwise
  valid source.
- `mode opt` lets advanced code state and depend on access, layout, lifetime,
  and no-copy contracts. A missing proof is a diagnostic because silently
  copying would violate the requested performance contract.

Normal Witchy never pays the cognitive cost of references or named lifetimes.
Opt mode is an explicit drop into lower-level control, not a second language.

## Conventions and qualifiers

- `let x: T` grants nonescaping shared access for the call.
- `var x: T` grants exclusive move-in/write-back access to a caller place.
- `own x: T` consumes the caller's value and ownership state.
- `move x` makes a consuming transfer explicit where the syntax requires it.
- `unique T` states sole ownership; `local unique T` additionally forbids
  escape; `frozen T` states deep immutability.
- `packed T` selects a checked fixed-layout representation where the complete
  boundary supports it.

These are orthogonal dimensions. A parameter convention describes the call; a
qualifier describes the value contract; a reference describes access to an
existing place.

## Explicit references

Only an opt-mode module may name:

```witchy-static
&'a T
&'a mut T
```

Shared references are copyable read access. Exclusive references are affine
read/write access. Named lifetimes state relations between inputs, results, and
fields; concrete durations are inferred from use. References may travel through
typed aggregates, generics, traits, closures, and function values while their
owner roots and access kind remain intact.

A normal caller sees only a value-oriented interface. The compiler selects a
proven no-copy entry or a copy-correct repair entry without exposing references
to normal source. Owner-backed results detach before normal code can observe
aliasing.

## Mutation and extraction

Mutation remains value-semantic. A `var` call commits one final value on normal
return and commits nothing after a trap. Proven-unique containers may update or
extract in place. Shared containers take copy-on-write in normal mode and are
rejected where an opt-mode no-copy contract requires uniqueness.

Discarded status or displaced-value results may select a result-free lowering
only when source explicitly discards them under RFC-0125. Used results preserve
the complete `Option`/`Result` semantics.

## Performance claims

Opt syntax earns its place only when it unlocks a categorical property:

- guaranteed no-copy mutation or extraction;
- fixed-layout or unboxed storage;
- destination passing;
- bounded allocation and constant-stack state kernels;
- zero-copy borrowed access; or
- a similarly auditable memory/layout result.

An opt annotation that buys only a vague constant-factor hope does not become a
language feature. Every claim receives deterministic counters, forced-copy or
de-optimized comparison, and measured representative workloads.

## Acceptance

1. Normal code cannot name references and cannot fail because an optimization
   loan was imprecise.
2. Opt references preserve provenance, access kind, lifetime identity, and
   affine state through every accepted type and callable boundary.
3. Normal-to-opt calls preserve value semantics, mutation write-back, function
   identity, and owned results.
4. Interpreter, optimized Wasm, and forced-copy Wasm agree for the complete
   accepted reference and ownership matrix.
5. Each no-copy, layout, or bounded-resource promise has a deterministic
   executable counter and a value oracle.
6. The installed distribution includes one curated opt-mode workflow with an
   actionable negative diagnostic and measured payoff.
7. Future CFG/SSA precision, layout expansion, and SIMD work strengthens this
   contract without adding reference burden to normal mode.
