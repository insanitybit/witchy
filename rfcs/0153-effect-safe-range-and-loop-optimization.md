---
rfc: 0153
title: "Effect-Safe Range and Loop Optimization: Typed Facts, Trap Preservation, and Reusable Sequence Plans"
status: planned
created: 2026-08-24
superseded-by:
related:
  - "0034 (compute gap and bounds elision)"
  - "0125 (core language evaluation contract)"
  - "0127 (ownership and opt mode)"
  - "0134 (codegen and compute roadmap)"
  - "0146 (sequence plans and rejected range reducer)"
tracking: "Selected as the next performance cut on 2026-08-25. Tracks 0-5 are merged; attribution, target closure, generalization, and promotion remain open in the RFC-0153 acceptance ledger."
---

# RFC-0153: Effect-Safe Range and Loop Optimization

## Summary

RFC-0146 successfully landed reusable sequence plans and packed-boolean cursor
lowering. Its experimental range reducer was removed because algebraic folds
could erase effectful calls and trapping loads, and because loop-carried
wrapping assignments were modeled unsoundly.

This RFC proposes a replacement built around two explicit contracts:

1. Every WIR expression exposes evaluation properties describing effects,
   suspension, memory access, and possible traps.
2. Range and induction facts are propagated through structured control flow
   with wrap-aware transfer functions and conservative joins.

Optimizations may consume those facts only when they preserve operand
evaluation, trap order, ownership invalidation, and exact integer semantics.
The same facts drive bounds-check elimination, pointer induction, stable-header
reuse, and residual arithmetic simplification across lists, bytes, strings,
and numeric loops.

> **2026-08-25 preparation checkpoint:** RFC-0153 is planned as the next
> performance cut. The fact producers and initial consumers from Tracks 0-5 are
> merged, but this is not an implementation-complete claim. Current fast
> exploration still leaves `collatz`, `nsieve`, and `fannkuch` above the
> matched-Go threshold, and the generalization and promotion evidence is open.
> The acceptance ledger freezes the shared interfaces, dependency graph,
> independent tracks, and transfer rule for costs this RFC does not own.

## Motivation

The 2026-08-24 fast snapshot still showed:

| Workload | Witchy / Go |
|---|---:|
| `collatz` | 1.43x |
| `fannkuch` | 1.19x |
| `nsieve` | 1.16x |
| `list_index` | 1.12x |

These are not one missing instruction. They are residual repeated checks,
address recomputation, conservative signed arithmetic, and facts lost at
control-flow boundaries. The near-parity `loop_sum` and `mandelbrot` controls
show that basic scalar Wasm execution is already competitive.

Real programs contain the same loops: parsers advance cursors through buffers,
serializers fill output spans, image and matrix kernels traverse strided rows,
and validation code carries ranges established by prior checks. A sound fact
system can improve all of them.

## Goals

1. Establish one reusable typed fact model for ranges, induction, sequence
   bounds, and exact evaluation properties.
2. Preserve all host effects, local writes, suspensions, reads, and traps in
   source order.
3. Extend existing `SequenceAccessPlan` rather than create method-specific
   bounds-elision paths.
4. Generate pointer cursors and direct loads/stores for proven affine accesses.
5. Simplify division, remainder, and comparisons only when wrap-aware facts
   prove equivalence.
6. Improve parser, transform, numeric, and collection fixtures outside the
   benchmark suite.

## Non-goals

- A general SMT solver or theorem prover in the compiler.
- Undefined behavior assumptions. Witchy Int arithmetic wraps, and traps remain
  observable.
- Eliminating checks based on profiling likelihood.
- Reordering host effects or treating `pure fn` as proof that a call cannot
  trap.
- Benchmark-specific knowledge about Collatz values, sieve indices, or
  permutation bounds.
- Reintroducing the rejected RFC-0146 Track 4 pass under a new default.

## Evaluation properties

Each WIR expression receives a compositional summary:

```text
EvaluationProperties {
    host_effect: None | Present,
    suspension: None | Present,
    memory: None | Read(roots) | Write(roots) | Unknown,
    may_trap: Bool,
    allocation: None | Present | Unknown,
    total: Bool,
}
```

`total` means evaluation is guaranteed to complete without effect, trap,
suspension, allocation, or observable memory access. A checked load is not
total. A `pure fn` call can still allocate or trap unless its body summary
proves otherwise. An unknown or indirect call receives conservative values.

Summaries are produced in the stage that owns the relevant semantics and are
carried into WIR. The optimizer does not infer effects from function names.

These summaries govern legality:

- Removing an expression requires `total` and proof its value is unused.
- Reordering requires independence of memory roots plus preservation of trap
  order.
- Replacing an expression with a cheaper one must still evaluate every
  effectful or trapping operand exactly once in the same order.
- Hoisting a read requires a stable root and proof no path writes or escapes it.

## Abstract facts

The initial lattice is intentionally small:

```text
Unknown
Constant(i64)
Interval { lo, hi, no_wrap }
NonNegative { upper?, no_wrap }
Positive { upper?, no_wrap }
Congruence { modulus, residue, interval? }
```

Facts attach to typed WIR locals and exact places, not source names. `no_wrap`
is a proof obligation, never a default. Transfer functions cover constants,
copies, checked comparisons, add/subtract by constants, multiplication by a
positive constant when bounded, shifts, masks, and canonical induction updates.
Unsupported operations produce `Unknown`.

Branch conditions refine facts only within the dominated arm. Joins retain
only facts true on every incoming path. Loop headers use a bounded fixed-point
iteration with widening; failure to converge loses precision rather than
accepting a speculative fact.

## Canonical induction

A loop induction plan requires:

- one exact local initialized before the loop;
- one monotonic update on every back edge;
- a nonzero constant or proven-sign step;
- a dominating loop condition relating the local to a stable bound;
- no opaque write to the induction local or bound root;
- proof that the update cannot wrap before loop exit.

The plan records initial domain, step, comparison, maximum trip count when
known, and all affine derived expressions. Nested loops have independent plans
and may consume stable facts from their parents.

## Sequence integration

The landed `SequenceAccessPlan` remains the sole owner of sequence header,
length, stride, cursor, and bounds-check coalescing. RFC-0153 contributes facts:

```text
IndexDomain { min, max_exclusive, affine_base, affine_step, no_wrap }
RootStability { epoch, writes, escapes }
```

An indexed operation is direct only when the plan proves nonnegative index,
index less than current length, address arithmetic without overflow, exact
element layout, and stable root. Otherwise the current checked path remains.

The first implementation supports:

1. forward and reverse unit-stride traversal;
2. positive constant strided traversal;
3. paired cursors moving inward from both ends;
4. nested row-major loops with stable row width;
5. loop-invariant length/header hoisting;
6. packed Bool and byte payloads through existing layout descriptors.

## Arithmetic transformations

Every transformation is separately counted and measured:

1. Positive proven `x % 2^k` becomes `x & (2^k - 1)`.
2. Positive proven `x / 2^k` becomes a right shift.
3. A masked zero-test may feed a branch directly without materializing a
   remainder.
4. A quotient and remainder from the same exact operands may share evaluation
   when both operands are total and trap behavior is unchanged.
5. Address multiplication by constant stride may become a cursor increment
   inside a canonical loop.
6. Redundant comparisons implied by the dominated interval may be removed only
   when all original operands are total. Otherwise their evaluation is retained
   and only the boolean consumer is simplified.

Signed non-power-of-two division is deferred. It requires a separate amendment
with exhaustive semantics around zero, negative operands, minimum Int, and
truncation.

## Compiler controls and diagnostics

The optimized and deoptimized pipelines must be selectable for tests and
measurements. A deterministic stats payload records:

- facts created, joined, widened, and dropped;
- loops accepted and each rejection reason;
- bounds checks removed or retained;
- headers hoisted;
- cursorized accesses;
- arithmetic rewrites;
- effect, trap, overflow, alias, and escape declines.

Production Wasm contains no stats globals or placeholder instructions when
stats are disabled.

## Generalization matrix

| Family | Required cases |
|---|---|
| byte parser | forward cursor, delimiter scan, malformed-input trap/control |
| binary transform | reverse paired cursor, strided rewrite, overlapping roots |
| matrix kernel | nested row-major traversal, empty rows, dynamic width |
| histogram | checked data-derived index and proven sanitized index |
| serializer | bounded output cursor with exact and conservative capacities |
| numeric loop | positive, negative, wrapping-near-boundary, early break |

Each family includes a positive promoted case and a near-miss case that must
retain checks. Interpreter results and independent expected outputs adjudicate
semantics; the interpreter does not implement the optimization.

Before implementation, Phase 0 selects at least three existing repository
loops outside `benchmarks/` from standard-library, project, or example code.
Those unchanged sources remain protected evidence through promotion.

## Acceptance criteria

1. Adversarial fixtures prove effects, allocations, reads, writes, and traps
   are neither removed nor reordered across all proposed rewrites.
2. Exhaustive small-domain and frozen boundary vectors cover wrapping add/sub,
   negative values, minimum/maximum Int, empty ranges, zero steps, and address
   overflow.
3. Generated WAT and deterministic counters prove each promoted mechanism and
   each required negative fallback.
4. At least four non-benchmark workload rows improve by 10 percent or more,
   including at least three whole-operation timing improvements and two
   pre-existing repository sources. Deterministic instruction reduction alone
   is mechanism evidence, not a real-program speed claim. No protected row
   regresses by more than 5 percent.
5. `collatz`, `fannkuch`, `nsieve`, and `list_index` each reach at most 1.00x of
   matched Go, or the RFC remains incomplete. A benchmark whose profile shows
   this RFC owns less than 10 percent of time is transferred to a named later
   owner rather than forcing an unrelated rewrite.
6. `loop_sum`, `mandelbrot`, `list_sum`, `binary_trees`, `dict_count`, and
   `word_count` remain within 5 percent of their protected medians.
7. Two warmups and twelve paired interleaved timing samples support promotion;
   three deterministic counter payloads agree exactly.
8. Whole-corpus emitted Wasm grows by less than 6 percent and compiler time by
   less than 8 percent.
9. The full deoptimization matrix preserves results and traps.
10. Focused shards and the serialized merge-queue gate pass, with durable raw
    artifacts and merged commits recorded in the acceptance ledger.

## Delivery slices

1. Phase 0: profile current master and freeze the generalization and protected
   matrices.
2. Phase 1: land `EvaluationProperties` and tests without an optimization
   consumer.
3. Phase 2: land range facts and branch refinement with stats only.
4. Phase 3: land canonical induction and `SequenceAccessPlan` fact inputs.
5. Phase 4: promote cursor and bounds transformations one family at a time.
6. Phase 5: promote arithmetic transformations separately.
7. Phase 6: run interaction, compile-time, code-size, and full benchmark gates.

No slice combines a new proof producer and several consumers without first
pinning the producer's negative cases.

## Relationship to RFC-0146

RFC-0146 Track 2 and Track 6 remain implemented and are prerequisites. Its
Track 4 performance implementation was rejected and its unsafe experimental
pass was removed. RFC-0153 replaces only that rejected range-analysis design
and extends the landed sequence-plan interface through an explicit fact
contract.

## Alternatives

### Restore the prior range reducer with more guards

The prior failure was architectural: it had no complete evaluation-property or
wrap-aware dataflow contract. Adding local exclusions would create an expanding
list of missed effects. This RFC makes legality a first-class input.

### Rely on Binaryen or Cranelift

Backend passes cannot recover Witchy ownership roots, exact sequence lengths,
or source-level trap and capability facts after erasure. They remain important
controls and may make a Witchy rewrite redundant, in which case that rewrite
does not land.

### Add unchecked indexing

An unsafe or unchecked API would move proof burden to users and weaken the
language contract. The compiler already has the facts required to remove checks
without changing source semantics.

## Drawbacks

- Evaluation summaries and abstract facts increase compiler complexity and
  memory use.
- Conservative invalidation may initially produce fewer wins than expected.
- Fixed-point analysis requires deterministic budgets and careful compile-time
  controls.
- More direct pointer loops increase the importance of adversarial overflow and
  root-stability testing.
- Some residual gaps may be backend costs that this RFC correctly declines to
  address.

## Prior art

- Sparse conditional constant propagation, abstract interpretation, scalar
  evolution, and induction-variable simplification.
- Rust and Swift preserve trapping and ownership semantics while applying
  bounds-check elimination.
- Go's bounds-check elimination output is a useful paired control, not a source
  of Witchy semantic facts.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior lives in spec/ and code, not here.
-->
