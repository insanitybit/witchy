---
rfc: 0147
title: "Holistic Architecture Performance: Moving Beyond Micro-Benchmarks"
status: proposed
created: 2026-08-22
superseded-by:
tracking:
---

# RFC-0147: Holistic Architecture Performance: Moving Beyond Micro-Benchmarks

## Summary

This RFC reorients Witchy's performance strategy away from kernel-specific micro-optimizations (like those in RFC-0145 and RFC-0146) toward resolving the five fundamental architectural bottlenecks in the language and runtime. We propose a systemic overhaul comprising a true WIR middle-end optimizer, first-class unboxed value types, closure monomorphization, static borrow analysis for reference counting, and lock-free ring-buffer channels.

## Motivation

Our recent optimization cycles successfully closed specific gaps in synthetic micro-benchmarks (`collatz`, `fannkuch`, `fib`), but profiling reveals we are treating symptoms, not causes. Witchy's baseline compilation and execution model imposes a structural tax on general-purpose software:

1.  **Wasm is not an Optimizing IR:** Witchy treats Wasmtime/Cranelift as its primary optimizer. However, Cranelift is a function-at-a-time JIT constrained by Wasm's linear memory sandboxing. It cannot do inter-procedural escape analysis, global inlining, or understand Witchy's high-level semantics.
2.  **Heap-Heavy Value Representation:** Composite types (tuples, records) are frequently allocated in linear memory, turning value-passing into reference-passing and forcing RC overhead.
3.  **Dynamic Closure Overhead:** Idiomatic functional pipelines use higher-order functions, which Witchy compiles to Wasm's `call_indirect`. This stalls hardware branch predictors and breaks compiler inlining.
4.  **Reference Counting Traffic:** Perceus RC is fast but noisy. Safe read-only borrows currently pay the cost of `$rc_dup` and `$rc_drop` atomic/non-atomic updates.
5.  **Channel State Machine Allocation:** Channels rely on heavy state machines, lock acquisitions, and allocations, drastically trailing the zero-allocation performance of Go's `M:N` ring-buffer channels.

If we don't address these core architectural limits, Witchy will forever require manual compiler hints or track-by-track benchmark fixes to compete with natively compiled languages like Go and Rust.

## Design

This RFC proposes five independent but compounding architectural pillars for the compiler and runtime.

### 1. High-Level WIR Middle-End Optimizer
Witchy must own its optimization pipeline *before* emitting WebAssembly. The compiler's WIR (Witchy Intermediate Representation) will be expanded to support:
*   **Inter-procedural Inlining:** Inlining across function boundaries, specifically targeting leaf functions and loop bodies.
*   **Global Escape Analysis:** Proving when allocations never escape a call stack, allowing them to be stack-promoted or scalar-replaced.
*   **Loop Invariant Code Motion (LICM) & CSE:** Moving array bounds checks and base pointer calculations out of hot loops.

### 2. First-Class Unboxed Value Types
Witchy will leverage Wasm's multi-value returns and parameter lists to pass composite values in registers.
*   Records and tuples with $\le 4$ fields will be completely unboxed into parallel scalar registers (`i64`, `f64`) across the call graph.
*   Memory allocations for small, short-lived composite types will be eliminated.

### 3. Closure Monomorphization
Higher-order functions (e.g., `list.map`, `list.filter`) will be monomorphized when the closure is statically known at the call site (the >95% case).
*   The compiler will stamp out a specialized version of the higher-order function with the closure body inlined directly.
*   This transforms opaque `call_indirect` table lookups into zero-cost, inline machine code.

### 4. Static Borrow Analysis
The compiler will introduce a static borrow checker pass during WIR generation.
*   When a function is proven to only read a value without retaining it or mutating it, the compiler will elide the corresponding `$rc_dup` and `$rc_drop` instructions.
*   This removes memory bus contention and branch overhead for ubiquitous read-only data access.

### 5. Lock-Free Ring-Buffer Channels
The async executor and channel implementation will be rewritten.
*   Channels will be backed by power-of-two contiguous ring buffers with atomic head/tail cursors.
*   Wait queues will be intrusive, preventing allocation on `send`, `receive`, or `select` operations.
*   Two-way `select` statements will compile down to unallocated atomic polls.

## Alternatives

*   **Continue with RFC-0146 Micro-Optimizations:** We could keep targeting specific benchmarks via peephole optimizations. *Why rejected:* This has diminishing returns and leaves idiomatic, non-benchmark Witchy code significantly slower than Go.
*   **Abandon Wasmtime for LLVM:** We could compile Witchy directly to native machine code via LLVM. *Why rejected:* Wasm provides crucial capability-based sandboxing (`witchy-caps`) that defines Witchy's security model. We need fast Wasm, not just fast native code (though a `trusted-exe` native backend is highly desirable in the future).
*   **Switch to Tracing GC:** Abandon Perceus RC for a tracing garbage collector. *Why rejected:* Ruins deterministic latency and contradicts the project's goal of predictable performance modes.

## Drawbacks

*   **Compiler Complexity:** Building a proper middle-end optimizer (LICM, CSE, Escape Analysis) significantly increases the complexity and compile times of the `witchy-wir` crate.
*   **Code Bloat:** Monomorphization (Pillar 3) increases the size of the generated Wasm binary, potentially impacting instantiation times or instruction cache performance.
*   **Multi-Value Wasm Compatibility:** Relying heavily on Wasm multi-value returns (Pillar 2) requires strict adherence to Wasm 2.0 standards and ensures all target runtimes fully support it.

## Prior art

*   **Go Compiler (gc):** Whole-program escape analysis, aggressive inlining, and lock-free channel architecture.
*   **Rust (rustc):** Monomorphization of traits/closures and static borrow analysis to eliminate RC overhead (via static lifetimes).
*   **Koka / Perceus:** Advanced reference counting optimizations (reuse analysis) combined with functional in-place updates.
