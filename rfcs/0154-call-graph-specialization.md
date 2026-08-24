---
rfc: 0154
title: "Call-Graph Specialization: Closure Devirtualization, Envelope Elision, and Budgeted Recursive Expansion"
status: proposed
created: 2026-08-24
superseded-by:
related:
  - "0062 (closure escape elision)"
  - "0090 (proper tail calls)"
  - "0111 (cross-boundary specialized layouts)"
  - "0127 (ownership and opt mode)"
  - "0138 (pure and once callable contracts)"
  - "0146 (rejected leaf inlining and deferred recursive inlining)"
tracking: "Proposes call-site specialization after RFC-0146's narrow inlining experiments missed their workload gates; implementation evidence will live in an RFC-0154 acceptance ledger."
---

# RFC-0154: Call-Graph Specialization

## Summary

Witchy's monomorphizer knows concrete types, callable identities, capture
layouts, ownership envelopes, `pure` and `once` contracts, and recursive call
components. Much of that information is not yet converted into specialized
WIR call edges.

RFC-0146 tested two narrow approaches. Removing one direct closure call changed
`closure_calls` by only 0.33 percent, showing that call opcode removal alone was
not the dominant cost. One-level recursive expansion improved `fib` by about
5.9 percent; a depth-two control was faster but was rejected under its local
code-size budget. Neither experiment became production code.

This RFC replaces those experiments with whole-call-site specialization:

1. devirtualize exact callable values;
2. scalar-replace non-escaping capture environments;
3. eliminate argument/result ownership adapters and envelopes when caller and
   callee layouts agree;
4. clone small functions for useful constant, range, layout, and callable
   facts;
5. apply bounded expansion to eligible recursive strongly connected components
   under a module-level code-size budget.

The unit of optimization is a typed call edge and its complete cost, not an
AST function name or a benchmark.

## Motivation

The 2026-08-24 fast snapshot reported `fib` at 1.62x Go and `closure_calls` at
1.51x. `collatz` at 1.43x may also contain a call component, though RFC-0153
owns its range and loop costs.

Real programs rely more heavily on calls than microkernels do: iterator
adapters, visitor dispatch, validators, callbacks, policy predicates, parser
combinators, and recursive tree transforms. Witchy's stronger callable and
ownership types should allow more precise specialization than a backend sees
after closure and value envelopes have been erased.

Simply inlining every small body is not sufficient. It can retain capture
allocation, capacity shuffling, result reconstruction, and indirect adapters
while increasing code size. The compiler must model and remove the whole edge.

## Goals

1. Turn an exact non-escaping callable into a direct typed edge with scalar
   captures where profitable.
2. Remove redundant caller/callee representation adapters and result envelopes.
3. Specialize reusable functions on facts that materially simplify their body.
4. Expand eligible recursion enough to reduce hot-edge frequency without
   changing asymptotic stack behavior.
5. Use deterministic, module-level cost budgets and reason counters.
6. Improve higher-order and recursive real-program fixtures outside the
   benchmark suite.

## Non-goals

- Source-level memoization, algorithm replacement, or automatic parallelism.
- Unbounded inlining or recursive fixed-point expansion.
- Cloning capability, host, suspension, dynamic, or unsupported reference
  boundaries.
- Assuming `pure` means total or non-allocating.
- Profile-guided optimization as a release prerequisite. Profiles inform cost
  calibration, but build output remains deterministic for a given program.
- A benchmark-specific Fibonacci rewrite.

## `CallSiteSummary`

After monomorphization and before WIR call optimization, every call edge may
carry:

```text
CallSiteSummary {
    exact_target,
    argument_layouts,
    result_layout,
    ownership_envelope,
    capture_layouts,
    callable_contract: ordinary | pure | once,
    effects_and_traps,
    suspension,
    escape_state,
    recursion_component,
    known_constants,
    known_ranges,
    weighted_frequency_hint,
}
```

All type and callable identity facts come from the type checker and
monomorphizer tables. The pass does not reconstruct a shadow type system from
names. Unknown fields conservatively disable transformations that require them.

`weighted_frequency_hint` is structural and deterministic: loop nesting,
single-use edges, and recursion depth classification. It is not a host profile
embedded into the build.

## Transformations

### Exact callable devirtualization

A callable value with one exact target becomes a direct call. A non-escaping
environment is decomposed into exact capture operands. A `once` callable may
move its captures directly when the call dominates all other uses. Ordinary
and `pure` callables retain their distinct contracts.

Devirtualization can land without inlining if it removes environment or
indirect-dispatch cost. Its acceptance evidence measures the complete call
edge, including capture allocation.

### Ownership-envelope elision

When caller and callee agree on exact layouts and ownership lanes, forwarding
adapters are removed. Capacity tokens, roots, and multi-value results travel in
their existing lanes. If a nominal aggregate result is consumed only by local
projection or match, its fields remain scalar until an escape requires
materialization.

No lane may be dropped merely because the immediate caller ignores it. Roots
and write-back tokens required for lifetime or ownership remain live.

### Fact-specialized cloning

A function may be cloned for a call site when constants, ranges, exact callable
arguments, or layout facts make at least one branch, adapter, allocation, or
dynamic dispatch removable. Clones are keyed by canonical facts and shared by
equivalent call sites. Specialization without a predicted removable cost is
declined.

The clone passes through the normal optimizer and retains original evaluation,
trap, and ownership semantics. Code-size cost is charged before emission and
reconciled after optimization.

### Budgeted direct inlining

After devirtualization and envelope simplification, a remaining exact edge may
be inlined when its net cost is favorable. The cost model includes removed call
and adapter nodes, added body nodes, loop nesting, allocation changes, and
instruction-cache pressure. Single-use wrappers have a lower threshold than
repeated library bodies.

Inlining is forbidden across suspension, host authority, dynamic invocation,
unsupported references, or unknown ownership envelopes.

### Recursive SCC expansion

Eligible recursive strongly connected components must be scalar or exact-
layout, contain no suspension or host effect, and preserve proper tail calls.
The initial implementation supports self-recursion; mutual recursion remains a
named fallback until separately proven.

Expansion clones a bounded number of recursive edges and leaves recursive calls
at the boundary. It never changes asymptotic stack depth. The budget is charged
against the complete emitted module, not only the small function's local WAT
line count. A hot tiny SCC may grow substantially when total module growth
remains small.

Depth 0, 1, 2, and optionally 3 are evaluated as compiler controls. The
shipping depth is chosen by deterministic cost rules calibrated on the full
generalization corpus, not by recognizing Fibonacci.

## Pass ordering

The intended order is:

1. monomorphization and exact layout construction;
2. call-site summary construction;
3. exact callable devirtualization;
4. capture and ownership-envelope scalar replacement;
5. fact-specialized cloning;
6. budgeted direct and recursive expansion;
7. RFC-0153 range/loop optimization on specialized bodies;
8. proper-tail-call formation;
9. local pruning and ordinary WIR peepholes.

Interaction tests pin this order. Recursive expansion must not hide a tail edge,
and later fact optimization must not erase required effects or roots.

## Cost and growth budgets

The optimizer uses nested budgets:

- per call site weighted-node budget;
- per function clone count;
- per recursive component expansion budget;
- per module emitted-byte budget;
- compile-time work budget.

Default corpus limits are 8 percent total emitted Wasm growth and 5 percent
compile-time growth. A single tiny recursive function may exceed 50 percent
local growth if the module remains within budget and matched measurements prove
the gain. This corrects RFC-0146's overly local rejection of an otherwise useful
depth-two control while retaining a hard whole-program limit.

Deterministic counters report exact edges, devirtualized edges, scalarized
captures, adapters removed, clones shared, inline accept/reject reasons,
recursive depth, emitted bytes charged, and budget exhaustion.

## Generalization matrix

| Family | Required cases |
|---|---|
| higher-order traversal | capturing and non-capturing callbacks over lists and iterators |
| parser combinator | exact closures, result matching, failure propagation, traps |
| policy predicate | pure callable values with captured immutable configuration |
| record visitor | nominal input/result records and local field projection |
| recursive tree | non-tail fold, tail-recursive walk, allocation in leaves |
| numeric recursion | one and two recursive edges, positive and negative controls |
| escape control | returned callable, stored callable, opaque dynamic call |

Every positive case has a near-miss that must remain indirect, materialized, or
uninlined. Real-program fixtures use ordinary stdlib APIs and independently
checked output.

Phase 0 selects at least three pre-existing repository call paths outside
`benchmarks/`, including one higher-order path and one recursive or mutually
referential call graph. Their unchanged sources remain in the protected matrix.

## Measurement contract

Phase 0 records whole-operation timing, allocation calls/bytes, dynamic call
counts, capture-environment allocations, adapter transitions, emitted Wasm
bytes, compile time, and generated WAT. A symbolized native profile is preferred
when available. When Wasmtime JIT symbols are unavailable, deterministic call
counts plus matched depth/control builds and whole-workload timings provide the
entry evidence; absence of symbols is recorded rather than guessed around.

Each transformation is enabled independently first. Combined promotion occurs
only after interaction measurements show the gain is not double-counted.

## Acceptance criteria

1. Differential and independent expected-output tests preserve call order,
   traps, failure propagation, ownership write-back, capture lifetime, and
   proper tail behavior.
2. Generated WAT and exact counters prove capture allocation, indirect call,
   adapter, or recursive-edge reductions for every promoted family.
3. At least four non-benchmark family rows improve by 10 percent or more,
   including at least three whole-operation latency improvements and two
   pre-existing repository sources. Allocation and instruction reductions are
   supporting evidence, not substitutes for real-program timing. No protected
   family regresses by more than 5 percent.
4. `closure_calls` and `fib` each reach at most 1.00x of matched Go, or the RFC
   remains incomplete.
5. If `collatz` consumes a call-graph transformation, its gain is measured
   independently from RFC-0153 and assigned once.
6. `expr_eval`, `binary_trees`, `record_build`, `word_count`, and `dict_count`
   remain within 5 percent of protected medians.
7. Total emitted Wasm growth across the complete corpus stays below 8 percent,
   compile time below 5 percent, and maximum stack depth does not regress for
   tail-recursive programs.
8. Two warmups and twelve paired interleaved samples support timing promotion;
   three deterministic counter payloads agree exactly.
9. The deoptimized build preserves results and traps and remains available as
   the paired control.
10. Focused shards and the serialized merge-queue gate pass, with merged
    commits and durable evidence recorded in the acceptance ledger.

## Delivery slices

1. Phase 0: freeze call-edge counters, allocation measurements, control depths,
   code size, compile time, and current-master timings.
2. Phase 1: land `CallSiteSummary` and deterministic diagnostics without
   transformations.
3. Phase 2: land exact devirtualization and capture scalar replacement.
4. Phase 3: land ownership-envelope elision and local result scalarization.
5. Phase 4: land fact-specialized clones and direct inlining independently.
6. Phase 5: land recursive SCC eligibility, then depth-one and depth-two
   candidates as separate measured branches.
7. Phase 6: integrate with RFC-0153 facts and run the full protected matrix.

Phases 1 through 3 have no dependency on RFC-0153 and may land independently.
Fact-specialized cloning consumes RFC-0153 metadata only when that interface is
available; otherwise it supports constants, exact layouts, and callable
identity and declines range-driven clones.

## Relationship to RFC-0146

RFC-0146 Track 3 remains rejected because its direct-call removal produced no
meaningful whole-workload improvement. Track 5 remains deferred because native
call/return attribution was unavailable and its narrow controls did not meet
the original gate. RFC-0154 does not declare those experiments successful. It
changes the proposed unit of optimization from one call opcode to the complete
typed call edge and changes code-size accounting from local function growth to
the emitted module.

## Alternatives

### Increase the ordinary inliner threshold

This grows code while leaving capture environments and ownership adapters in
place. The prior experiment already showed that deleting a direct call alone
was not enough.

### Enable depth-two recursion globally

One fixed depth ignores body size, module size, recursion shape, and protected
workloads. SCC eligibility and module budgets make the decision general and
bounded.

### Depend on backend inlining

Cranelift does not retain Witchy's callable identity, ownership envelopes, or
source-level capture facts after erasure. Backend output remains a control, but
front-end specialization owns information the backend cannot reconstruct.

## Drawbacks

- Call-site summaries and clones increase compiler memory and compile time.
- Specialized code can increase instruction-cache pressure.
- Recursive expansion creates a larger semantic and code-size test matrix.
- Whole-edge optimization crosses lowerer and WIR boundaries and needs a frozen
  interface before parallel implementation.
- Some simple recursive call overhead may remain a Wasmtime ceiling after all
  Witchy-specific envelopes are removed.

## Prior art

- Whole-program monomorphization, partial evaluation, devirtualization, and
  scalar replacement of closure environments.
- LLVM function specialization and inlining cost models.
- Bounded recursive inlining used by native compilers under code-size budgets.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior lives in spec/ and code, not here.
-->
