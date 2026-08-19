---
rfc: 0134
title: "Codegen and compute optimization roadmap"
status: proposed
created: 2026-08-19
related:
  - "0017 (codegen performance constant factors)"
  - "0027 (packed layouts and SROA)"
  - "0029 (performance tier contract)"
  - "0033 (place-based uniqueness)"
  - "0034 (closing the compute gap - codegen and runtime levers)"
  - "0062 (closure escape elision)"
  - "0087 (fused mutators and uniform write-back)"
  - "0089 (functional in-place)"
  - "0090 (guaranteed proper tail calls)"
  - "0111 (cross-boundary specialized layouts)"
  - "0123 (expression boundaries and collection discard optimization)"
  - "0127 (ownership and opt mode)"
tracking: "Proposes three implementation tracks for constant-factor compute parity with Go"
---

# RFC-0134: Codegen and compute optimization roadmap

## Summary

This meta-RFC consolidates Witchy's performance optimization landscape across
computational, recursive, collection, and memory workloads. It audits current
benchmarks against native Go, traces existing performance contracts across
preceding RFCs, and organizes all remaining optimization opportunities into
three actionable tiers based on architectural dependencies and precursor effort.

## Current Standing & Benchmark Reality

With the landing of `mode opt` ([RFC-0127](0127-ownership-and-opt-mode.md)),
in-place value mutators ([RFC-0087](0087-fused-mutators.md)), guaranteed tail
calls ([RFC-0090](0090-proper-tail-calls.md)), and collection discard
elimination ([RFC-0123](0123-expression-boundaries.md)), Witchy demonstrates
clear architectural advantages over tracing-GC runtimes on memory-heavy and
tree-recursive workloads, while retaining constant-factor gaps on tight
iterative arithmetic and array kernels:

| Workload Class | Representative Benchmarks | Standing vs Native Go | Primary Driver |
|---|---|---|---|
| **Allocation & Strings** | `strings` | **0.01x (Witchy 180x faster)** | Value-semantic in-place string buffer growth vs GC allocations |
| **Recursive Tree Graphs** | `binary_trees`, `expr_eval` | **0.36x - 0.44x (Witchy 2.3x - 2.8x faster)** | Region/arena and unique value recycling without GC pauses |
| **Dictionary Mutations** | `dict_count` (3M ops) | **0.72x (Witchy 1.38x faster)** | Discarded insert optimization (RFC-0123/RFC-0124) |
| **Dynamic Array Growth** | `packed_records` (2M ops) | **0.51x (Witchy 2.0x faster)** | In-place unboxed struct lists vs Go dynamic slice resizing |
| **Call-Bound Recursion** | `fib` (35) | 1.59x (Go faster) | Unused scratch local variable allocations on Wasm stack frames |
| **Tight Arithmetic Loops** | `loop_sum`, `collatz` | 1.32x - 1.99x (Go faster) | Hardware integer division (`i64.rem_s`) and dead discard bytecodes |
| **Array Mutators & Sieve** | `list_index`, `nsieve`, `fannkuch` | 1.93x - 4.71x (Go faster) | Bounds checking, list pre-allocation, and 8-byte uniform slots |
| **Higher-Order Callbacks** | `word_count`, `closure_calls` | 1.72x - 2.77x (Go faster) | Anonymous closure dynamic dispatch in high-frequency loops |

## Precursor Map: Preceding Optimization RFCs

Witchy's performance architecture builds on several foundational RFCs:

1. **Memory & Mutation Foundation**:
   - [RFC-0027](0027-packed-layouts-sroa.md): Packed struct representations and scalar replacement of aggregates.
   - [RFC-0033](0033-place-based-uniqueness.md) & [RFC-0089](0089-functional-in-place.md): Place-based uniqueness and fully in-place functional mutators.
   - [RFC-0087](0087-fused-mutators.md): Uniform var write-backs and capacity-passing list mutators.
   - [RFC-0123](0123-expression-boundaries.md): Discarded return value elimination for collection modifiers (subsumed RFC-0124).

2. **Control Flow & Execution Foundation**:
   - [RFC-0090](0090-proper-tail-calls.md): Guaranteed proper tail calls (TCO) with constant control stack on both interpreter and Wasm backends.
   - [RFC-0034](0034-closing-the-compute-gap.md): Binaryen integration (L1), conservative bounds check elision (L2), and closure devirtualization (L3).
   - [RFC-0062](0062-closure-escape-elision.md): Closure escape elision and stack-allocated environments.

3. **Ownership & Type Systems**:
   - [RFC-0111](0111-cross-boundary-specialized-layouts.md): Cross-boundary layout specialization and unboxed floats.
   - [RFC-0122](0122-uniform-borrow-relations.md) & [RFC-0127](0127-ownership-and-opt-mode.md): Explicit borrow relations, references, and `mode opt` verification.

## Prioritized Implementation Tracks

To methodically close the remaining compute gaps without triggering large upfront architectural churn, optimizations are partitioned into three dependency tiers:

- **Track 1: Zero-Precursor Immediate Wins (Ready for Implementation)**
  - Local variable stack-frame pruning (dropping unused `WirLocal` allocations from the ~370 scratch pools)
  - Constant power-of-two arithmetic strength-reduction (`n % 2` -> `n & 1`) and dead-discard cleanup (`i32.const 0; drop`)
  - Standard preallocation builtin (`list.with_capacity`)

- **Track 2: Intra-Module Specialization & Inductive Loops (Moderate Effort)**
  - Inlining high-frequency collection closures (e.g. `dict.update(d, k, 0, fn(x): x + 1)`)
  - Inductive loop bounds-check elision for `while i < n` counting loops

- **Track 3: Long-term Layout Specialization & Native Features (Architectural)**
  - Sub-word element width specialization (`List(Bool)` 1-byte vs 8-byte)
  - Target-specific SIMD execution (Wasm SIMD128)

---

### Track 1: Zero-Precursor Immediate Wins (Ready for Implementation)

These optimizations require no changes to the language grammar, type checker, or memory model, and operate entirely within isolated WIR codegen and standard library stages.

#### 1.1 Stack-Frame Local Variable Pruning
* **Diagnosis**: In `crates/witchy-lower/src/codegen/mod.rs`, the compiler unconditionally allocates pools of scratch variables (for GC lists, reuse carriers, try contexts, and coordinate buffers) to every lowered function, creating ~370 declared `WirLocal` entries per function.
* **Mechanism**: In Wasmtime, entering a call frame reserves/zeroes all declared locals. In deep call graphs (such as `fib` with 29.8M calls), initializing 370 locals per frame dominates execution time.
* **Design**: Add an AST reference-collection pass prior to WIR emission that filters `WirFunction::locals` to retain only parameters and local variables actually referenced in that function's body.

#### 1.2 Power-of-Two Arithmetic Strength-Reduction
* **Diagnosis**: Expressions such as `n % 2 == 0` or `x % 1024` emit `i64.rem_s` (signed hardware division, costing 10-25 cycles), whereas native compilers emit a 1-cycle bitwise `and` / `test`.
* **Design**: During binary expression lowering, rewrite positive constant power-of-two modulo operations `x % 2^k` into bitwise masking `x & (2^k - 1)`. For parity comparisons `x % 2 == 0`, lower directly to `(x & 1) == 0`.

#### 1.3 Dead Discard Instruction Cleanup
* **Diagnosis**: Expression statements inside loop bodies currently leave trailing `i32.const 0; drop` pairs in the generated Wasm, increasing loop instruction density by 20-30%.
* **Design**: Suppress synthetic discard markers in statement position when lowering statements that produce no runtime value.

#### 1.4 List Capacity Initialization (`list.with_capacity`)
* **Diagnosis**: Dynamic list growth in Witchy starts with `var xs = []` (capacity 0) and performs multiple geometric reallocations and memory copies. Go benchmarks often preallocate slices via `make([]T, 0, n)`.
* **Design**: Expose `list.with_capacity(cap: Int) -> List(T)` in `std/list.witchy`, routing directly to the existing `$list_alloc_cap` WIR allocator.

---

### Track 2: Intra-Module Specialization & Inductive Loops (Moderate Effort)

These optimizations introduce targeted compiler passes to optimize high-frequency patterns without altering external ABIs.

#### 2.1 Higher-Order Combinator Closure Inlining
* **Diagnosis**: In high-frequency text and map processors (`word_count`), `dict.update(d, w, 0, fn(n): n + 1)` constructs and dispatches an anonymous closure object on every single key lookup.
* **Design**: Provide a WIR lowering specialization for standard collection mutators when supplied with inline closure literals, compiling them directly into in-place update loops.

#### 2.2 Inductive Loop Bounds-Check Elision
* **Diagnosis**: [RFC-0034](0034-closing-the-compute-gap.md) shipped conservative bounds-check elision for `for i in 0..list.length(xs)` loops. Array-heavy kernels (`nsieve`, `fannkuch`) use `while i < n` loops where `n <= list.length(xs)`.
* **Design**: Add a simple induction analyzer for counting `while` loops that elides `list.at` and `list.set_at` bounds checks when `i` starts within `[0, length)` and strictly increments without interior reassignment of the target list.

---

### Track 3: Long-Term Architectural & Layout Specialization (Heavy Precursors)

These represent deep structural projects deferred for future major milestones:

#### 3.1 Sub-Word Container Element Specialization
* **Diagnosis**: [RFC-0017](0017-codegen-performance.md) identified that uniform 8-byte container slots force `List(Bool)` to consume 8 bytes per boolean (8x cache line penalty vs Go's 1-byte `[]bool`).
* **Design**: Requires monomorphized container layout specialization and sub-word memory access intrinsics across the type system, runtime, and interpreter mirror.

#### 3.2 Target-Specific SIMD Vectorization
* **Diagnosis**: Floating-point numeric kernels (`mandelbrot`) benefit significantly from SIMD lane operations supported by modern hardware.
* **Design**: Lower bulk numeric iterations to Wasm `v128` vector instructions through Cranelift SIMD primitives.

## Verification & Acceptance Protocol

Implementations under this roadmap are verified against the benchmark harness in `benchmarks/run.sh` and `scratch/opt-bench/run_suite.py`:

1. **Parity Preservation**: All changes must preserve strict behavioral parity between the interpreter and compiled Wasm backends.
2. **Correctness Invariants**: Full test suites and diagnostic goldens must remain green under both standard and optimized compilation modes.
3. **No Regressions**: No existing benchmark may regress in execution latency or control stack consumption.
