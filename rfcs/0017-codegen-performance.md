---
rfc: 0017
title: Codegen performance — the constant-factor roadmap and its ceilings
status: superseded
created: 2026-06-27
superseded-by: "0029 (tier contract), 0034 (compute-gap levers), 0031 (SIMD)"
tracking:
---

# RFC-0017: Codegen performance — the constant-factor roadmap and its ceilings

This umbrella is superseded by the focused contracts in
[RFC-0029](0029-performance-tier-contract.md),
[RFC-0034](0034-closing-the-compute-gap.md), and
[RFC-0031](0031-simd-stdlib-hot-loops.md); current performance behavior is
tracked in [`spec/performance.md`](../spec/performance.md).

## Summary

The algorithmic performance problems are solved: the compiled backend has no
remaining O(n²) stdlib cliffs and no growth-driven OOM (dict is O(1), the
list/string/dict builders mutate in place, the collection combinators are O(n) —
see the perf work landed in `bench:`/`perf:` commits). What is left is **purely
constant-factor**: how values are represented, how often we bounds-check, how
calls are dispatched, and the JIT-vs-AOT + process-startup overhead.

This RFC is the **umbrella** for that work. It (1) diagnoses *why* witchy trails
Go on the benchmarks where it does, (2) enumerates every recoverable
optimization with an **honest achievability ceiling** — what each can and cannot
fully buy us — and (3) marks which ones warrant their own implementation RFC.

The thesis, stated up front so it can be argued with: the top three levers
(**unboxed layouts, bounds-check elimination, AOT startup**) plausibly pull the
worst micro-benchmarks from 2–4.4× down toward ~1.2–1.5× of Go. Past that lies
the *irreducible* cost of being a sandboxed, capability-secure, interpreter-parity
language instead of native Go — roughly a 1.2–1.3× floor on raw compute, against
which witchy already **beats** Go where its design shines (allocation + recursion).

## Status of the measurement

[`benchmarks/baseline.md`](../benchmarks/baseline.md) — 13 benchmarksgame-style benchmarks, witchy-wasm (run
end-to-end via `witchy sandbox`, including process start + wasm instantiation)
racing a prebuilt Go binary. Current standing:

| class | benchmarks | vs Go |
|---|---|---|
| **witchy wins** | binary_trees, expr_eval | 0.69×, 0.47× |
| close | collatz, loop_sum, word_count, list_sum, dict_count | 1.21–1.59× |
| call-bound | fib, closure_calls | 1.76×, 3.13× |
| float loop | mandelbrot | 2.18× |
| array-store | fannkuch, nsieve | 2.79×, 4.39× |
| string+dict | knucleotide | 3.04× |

**Methodology caveats** (load-bearing for reading the numbers): single machine,
±10–20% run-to-run noise, **end-to-end timing includes startup** (which inflates
the ratio on the sub-30 ms benchmarks), and these are micro-benchmarks that may
not reflect real workloads. A steady-state (compute-only) variant would isolate
the true codegen gap from startup; building one is an action item below.

## Motivation — the five structural gaps

Every benchmark deficit traces to one or more of these. Each is tagged
**recoverable** (a known optimization closes most of it) or **inherent** (a
floor we accept).

1. **The uniform 8-byte value slot.** *(recoverable)* Every value — `Int`
   (i64), `Float` (f64 bits), `Bool`, and pointers (i32 in the low half) — lives
   in an 8-byte slot, and collections are arrays of slots. So `List(Bool)`
   (nsieve's flags) is **8 bytes/element vs Go's 1**, and `List(Int)` packs
   nothing denser than Go. That is up to 8× the memory traffic and cache
   pressure. It is the dominant cost of **nsieve**, and a large part of
   **fannkuch** and **knucleotide** (200k transient char-slot lists). The single
   biggest structural disadvantage.

2. **Mandatory bounds checks.** *(recoverable)* Every `list.at`/`set_at` traps on
   out-of-range. Go's compiler *proves* indices in-range and elides the check; we
   pay it on every access. In tight array loops (**nsieve, fannkuch, list_sum**)
   this is a compare+branch Go usually doesn't emit.

3. **Call & closure ABI.** *(partly recoverable)* **fib** is recursion-bound and
   Go inlines small calls with near-free direct dispatch; **closure_calls** goes
   through `call_indirect` (table bounds check + type check + indirect branch)
   where Go devirtualizes/inlines. We also thread an own-cap token parameter
   through own-ABI functions even when unused, and pass arguments as boxed i64
   slots.

4. **wasmtime JIT vs Go AOT codegen.** *(mostly inherent)* We emit WASM and
   wasmtime/Cranelift lowers it to machine code. Cranelift is good, but Go's
   backend is more tuned for float and tight-integer loops, and we emit no SIMD.
   **mandelbrot** (2.18×) is largely this. We control the WASM we feed Cranelift,
   not its register allocation or instruction selection.

5. **Fixed startup.** *(recoverable for the JIT part)* End-to-end timing includes
   process start + module instantiation. On the tiny benchmarks that ~10–15 ms is
   a large fraction — **nsieve** is 31 ms total, so Go's 7 ms makes it *look*
   4.4× when much of ours is startup. Go's prebuilt binary starts in ~1 ms.

The encouraging inverse: where the design *helps* — allocation + recursion with a
bump/region allocator instead of a tracing GC — witchy **beats** Go
(**binary_trees** 0.69×, **expr_eval** 0.47×). The losses are exactly where Go's
unboxing + bounds-elision + AOT give constant-factor wins.

## Design — the optimization ladder

Each rung: **mechanism**, **target** benchmarks, the **ceiling** (what it can and
*cannot* fully achieve — the part this RFC exists to record), **dependencies**,
and **spin-out** (whether it deserves its own RFC).

### O1 — Unboxed / monomorphized layouts  *(spin-out: yes — its own RFC)*

- **Mechanism.** When a collection's element type (or a record's field type) is a
  statically-known fixed scalar, store the buffer at native width instead of
  8-byte slots: `List(Float)` → packed `f64[]`, `List(Int)` → packed `i64[]` (no
  win vs Go, but cache-dense), `List(Bool)` → packed bytes (or bits). Element
  access becomes base + index·width; the layout is contiguous, cache-dense, and
  SIMD-eligible.
- **Target.** nsieve (8×→1× bandwidth), mandelbrot, fannkuch, knucleotide. The
  biggest single lever — directly attacks gap (1).
- **Ceiling.** *Fully achievable* for **monomorphic scalar collections** whose
  element representation is statically known. *Not achievable* for polymorphic /
  generic collections whose element type is a type variable at codegen, or
  heterogeneous data, **without monomorphization** — those keep the 8-byte slot.
  So the ceiling is exactly the monomorphic/polymorphic line: specialize where
  the type is pinned, fall back to the uniform slot otherwise. This is also the
  first concrete, benchmark-shaped justification for **`mode opt`** — it is the
  whole-file invariant that *guarantees* the element representation statically, so
  the specialization is sound rather than best-effort (cf. RFC-0016 R6, which
  describes the `[len][cap][x0,y0,x1,y1,…]` packed buffer and notes it is
  mode-gated).
- **Dependencies.** Monomorphization (or `mode opt` as the per-file gate);
  type-directed layout selection; the access/store codegen for each width. The
  interpreter need not match the *layout* (unobservable — no `unsafe`, no
  identity, no memory-introspection in the language), only the *values*, so this
  stays a compiled-backend change like every other optimization here.
- **Status.** Proposed. The largest design on this list; **recommended to spin
  out into its own RFC** ("Unboxed monomorphized layouts") because it touches the
  type system (where representation is decided), the allocator, every element
  accessor, and the `mode opt` contract.

### O2 — Bounds-check elimination  *(spin-out: maybe)*

- **Mechanism.** A range/interval analysis: when an index is provably in
  `[0, len)` — a loop counter bounded by `len`, a constant, a value just checked —
  drop the runtime bounds check on `list.at`/`set_at`/etc.
- **Target.** Every tight array loop (nsieve, fannkuch, list_sum). Removes a
  compare+branch per access — gap (2).
- **Ceiling.** *Achievable* for the common provable shapes (`for i in
  range(len)`, `while i < len`, monotone counters, post-checked indices). *Not
  achievable* for genuinely dynamic indices (user-supplied, computed) — those
  **keep the check**, correctly, because the safety guarantee is non-negotiable.
  Most *hot* accesses are loop-counter-indexed, so this closes the bulk of the
  tax, not all of it. Soundness mirrors the uniqueness pass: a missed proof keeps
  the check (slower, never unsafe).
- **Dependencies.** A range lattice over the lowered AST. Composes with O1.
- **Status.** Proposed. Moderate; could be a section of an "analysis passes" RFC
  or its own.

### O3 — AOT module precompilation + startup  *(spin-out: no — section + tracking)*

- **Mechanism.** wasmtime can serialize Cranelift's compiled artifact
  (`Module::serialize` / precompiled `.cwasm`). Precompile to native at build /
  first run and *load* the artifact thereafter, skipping JIT compilation. There
  is already an on-disk *module* cache; the remaining cost is the JIT-warmup vs
  artifact-load delta plus instantiation. A persistent in-process module (CLI
  daemon, or reusing the compiled module across a `witchy test` run) amortizes
  instantiation too.
- **Target.** The fixed ~10–15 ms inflating every sub-30 ms benchmark's *ratio* —
  gap (5). High ROI for a CLI invoked repeatedly.
- **Ceiling.** *Fully eliminable*: the JIT-compilation cost (serialize once,
  deserialize fast). *Minimized but not zero*: OS process start + wasm
  instantiation (linear-memory init, table/elem setup). So the floor here is
  "process + instantiate," which is small. **Also report a steady-state benchmark
  variant** so the compute-only gap (the real codegen number) is separated from
  startup — currently the two are conflated.
- **Status.** Proposed. Small/operational; track as a section here.

### O4 — Call & closure optimization  *(spin-out: maybe — section)*

- **Mechanisms.** (a) **Devirtualize** closures bound to a known local that is
  never reassigned (`let f = fn…: …; f(x)`) into a direct call to the lifted
  body. (b) **Inline** small non-recursive functions. (c) **Drop the own-cap
  token param** on functions the summary shows don't thread ownership. (d) Avoid
  argument boxing where the callee's parameter width is statically known.
- **Target.** fib (recursion), closure_calls (indirect) — gap (3).
- **Ceiling.** Devirt is *achievable* for closures pinned to a known local;
  *not achievable* for closures passed as parameters (the common
  `iter.map(xs, f)` — `f` is dynamic inside `map`) without per-call-site
  specialization. Inlining is achievable for small functions; recursive inlining
  is bounded. The own-cap drop is achievable (the own-ABI summary already exists).
  Net ceiling: static-dispatch calls get cheap; **genuinely dynamic dispatch
  keeps the `call_indirect` floor**, which on wasmtime is inherently dearer than a
  direct call (table + type check).
- **Status.** Proposed. Moderate.

### O5 — Float / SIMD codegen  *(spin-out: no — section)*

- **Mechanism.** Keep floats unboxed in WASM f64 locals across a loop (no slot
  round-trips); emit WASM SIMD (`v128`) for recognizable vectorizable patterns;
  feed Cranelift cleaner float instruction sequences.
- **Target.** mandelbrot and other float loops — gap (4).
- **Ceiling.** *Partly achievable*: we control the WASM (unboxed locals, SIMD
  where the pattern is clear). *Out of our hands*: Cranelift's register
  allocation and instruction selection — the final machine code is wasmtime's.
  Float loops will likely stay somewhat behind Go's tuned AOT float backend even
  after we emit ideal WASM. Honest expectation: narrow, not erase, the gap.
- **Status.** Proposed. Small; partly gated on wasmtime.

### O6 — General reuse + the RC floor  *(defer entirely to RFC-0016)*

- The RFC-0016 RC floor + FBIP reuse. Does *not* move these micro-benchmarks much
  (they don't OOM at benchmark sizes), but it is the runtime-memory frontier
  (bounded memory for escaping values) and the home of **general consume-position
  reuse** — e.g. the `std/chan`/`std/task` scheduler's
  `(set_at(slots, i, v), …)` is currently O(tasks²) because the in-place fast
  path only fires on self-assign, not on a tuple-embedded consuming return.
  Ceiling and plan: see RFC-0016 (held for supervised work).

## The inherent floor — what we cannot fully achieve

Stated plainly so expectations are calibrated:

- **JIT (wasmtime/Cranelift) vs AOT (Go's backend).** Cranelift is a good JIT,
  but Go's compiler has had far more tuning for float/integer loop codegen, and we
  do not control wasmtime's output. A persistent ~1.1–1.3× on compute-heavy loops
  survives even ideal WASM emission.
- **The safety/generality tax.** Bounds checks we *keep* wherever the index isn't
  provably in range; the uniform 8-byte slot wherever the type isn't monomorphic;
  the indirect-call floor for genuinely dynamic dispatch. These are deliberate —
  they buy memory safety, capability security, and interpreter parity.
- **Realistic post-roadmap estimate.** O1–O3 plausibly bring nsieve/mandelbrot/
  fannkuch from 2–4.4× toward ~1.2–1.5×; O4 helps fib/closure_calls; startup work
  fixes the small-benchmark ratios. The residual floor is ~1.2× for raw compute —
  and witchy will keep **beating** Go where bump/region allocation beats a tracing
  GC. **We cannot expect to match Go's native loop throughput on every
  benchmark**, and that is the accepted cost of the language's guarantees.

## What gets its own RFC

The user's instinct — "maybe an RFC per optimization" — applied with restraint so
we don't ship six thin stubs:

| optimization | own RFC? | rationale |
|---|---|---|
| **O1 unboxed layouts** | **yes** | major: type system + allocator + every accessor + the `mode opt` contract |
| O2 bounds-check elimination | optional | a focused analysis pass; RFC if/when scheduled |
| O3 AOT startup | no | operational; a section here + a tracking issue |
| O4 call/closure opt | optional | several small mechanisms; RFC if grouped |
| O5 float/SIMD | no | small + gated on wasmtime |
| O6 RC floor / general reuse | already RFC-0016 | — |

So: this RFC is the standing index and ceiling-of-record; **O1 spins out next**,
the rest are tracked here until scheduled.

## Drawbacks / non-goals

- **Non-goal: beating Go everywhere.** The floor section says why. The goal is to
  close the *recoverable* constant factors and be honest about the rest.
- **Specialization vs simplicity.** O1/O4 trade the uniform value model's
  simplicity (one slot, one accessor) for representation diversity. Gated behind
  monomorphization/`mode opt` so the general path stays simple and the fast path
  is opt-in and checked.
- **Parity is preserved throughout.** Every rung is a compiled-backend change over
  an unobservable property (layout, redundant checks, dispatch form); the
  interpreter oracle and the differential tests are unaffected, as with all prior
  perf work.

## Prior art

- [RFC-0016](./0016-reference-counted-memory.md) (reference-counted memory) — R6 already sketches unboxed monomorphized
  layouts and the `mode opt` unlocks; this RFC is the codegen-side companion and
  pulls O1 forward as a standalone lever.
- The `let`/`var`/`own`/`move` conventions + the uniqueness pass — the existing
  machinery whose *inputs* (provable uniqueness, ownership) these optimizations
  also consume.
- Bounds-check elimination and unboxing are standard in optimizing compilers for
  typed languages (JVM/HotSpot range-check elimination; Rust/Go monomorphized,
  unboxed value arrays); the novelty here is doing them *behind* a capability-
  secure, interpreter-parity, sandboxed boundary, and gating the unsound-in-
  general ones behind `mode opt`.
