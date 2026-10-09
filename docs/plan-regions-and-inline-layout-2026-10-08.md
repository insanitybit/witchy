# Plan: regions without refcounts, and inline aggregate layout

Status: proposed 2026-10-08. Owner: whichever agent picks up a slice below;
record the branch in the ledger table. Governing RFCs: RFC-0178 (regions,
draft on `rfc/regions-memory64`, unmerged), a new RFC-0181 (inline layout, to
be written in slice 2), RFC-0051 (the fast-path zoo this work retires).

## Goal

Zero allocations and zero refcount operations on the hot path of every kernel
benchmark. That is the metric. Wall time against Go is the consequence we
report, not the thing we drive on.

Why this target and not "faster passes": the remaining 3x against Go on
binary_trees, word_count, fannkuch and nsieve, and the goose results that beat
Rust, are both explained by representation. goose never allocates per object
and never chases a pointer inside a value. witchy pays, per hot-loop write to
an aggregate, either a uniqueness proof or a copy-and-swap allocation, plus a
retain/release pair per reference. No pass removes that; a layout does.

Decided constraints (user, 2026-10-08):

- Ignore Wasm and Cranelift overhead. Codegen improves without us.
- Returned views stay. Zero-copy slices are a performance feature.
- std, examples and compiler-owned witchy stay explicit about conventions.
- One general mechanism. No new `*_cap` helpers, no new `self_*` recognizers.
  Regions and inline layout must drive the existing ownership analysis, not
  add a per-method path beside it.

## The two mechanisms and why both

**Regions** (RFC-0178): a `region:` block bump-allocates every value born
inside it and rewinds on exit. Covers tree and graph shapes, anything with
dynamic fan-out. The current draft keeps per-object RC inside the region,
and the measured result was binary_trees getting slower. The win appears only
when a region-born object has no header, no inc/dec and no free.

**Inline layout** (RFC-0181): a `List(Point)` whose element is a fixed-size
record is stored contiguously as `[x, y, x, y, ...]`, not as a list of
pointers to heap records. Covers array-of-struct shapes. `xs[i].x` becomes one
lane load (SROA already does this for locals). `xs[i].x = 3` on a unique list
becomes one store, where today it allocates a record copy unless uniqueness
is proven. Regions do not give this: a region-born list of records is still a
list of pointers, just freed cheaply. milo's benchmarks put the inline-versus-
arena gap at 20-30% on pointer-chasing workloads and at zero on graph
traversal, so regions are the bulk of the win on those shapes and inline is
the rest, plus the write-path allocation only inline removes.

Two separate facts make inline layout fit the existing analysis:

- **Storage** is a static per-type decision: element type is fixed size
  (scalars, pointers, nested fixed records), carries no `must` and no view.
  Identity is unobservable under value semantics, so no runtime fact about
  the list is needed.
- **In-place write** needs uniqueness, which the ownership analysis already
  computes for `var` lists. Unique: one store. Shared: one CoW of the list,
  as today, then the store.

Not in scope: strings inline inside records (goose's variable-size layout).
It would make `String` layout-dependent everywhere.

## Measurement first

Every slice below is accepted or rejected by a paired A/B on release
binaries using the kernel table (`scratchpad gp/ab.py` pattern; recreate
under `scripts/` as slice 1 so it is not lost again). Numbers:

- allocations per iteration (new counter, slice 1)
- retain + release ops per iteration (new counter, slice 1)
- wall time, median of alternating pairs, against the Go kernel

Memory64 is free given the R2 range-based elision probe, so it does not
compete with this work for attention.

## Slices

Each slice lands through the merge queue on its own, in a fresh worktree,
small enough that the two agents active on 2026-10-08 (`feat/mem64-emit` in
`canonical_wasmir/emit.rs` and the kernels; `perf/async-typed-frame-transport`
in `canonical_lower.rs` and the list projection/update lowering) rebase onto
a small diff. The two hunks that touch their files are deferred to the end.

| # | Slice | Files | Collides with | Depends on | Exit criterion |
|---|---|---|---|---|---|
| 1 | Alloc and RC counters in the kernel A/B harness | `scripts/kernel-ab.py` (new), a `WITCHY_COUNT_ALLOCS` runtime counter in a new file under `witchy-runtime` | nobody | none | table prints allocs/iter and rc-ops/iter for every kernel against master |
| 2 | RFC-0178 rewrite + RFC-0181 draft | `rfcs/0178-*.md`, `rfcs/0181-inline-aggregate-layout.md`, `rfcs/README.md` | nobody | none | RFC-0178 criterion 1 is "zero RC ops on region-born values"; RFC-0181 states the storage rule and the uniqueness rule above |
| 3 | Region arena kernel | new module under `canonical_wasmir/` (bump pointer, rewind, high-water), own tests in `crates/witchy-wir/tests/unit.rs` | nobody until the call-site hunk | 2 | kernel allocates and rewinds in a unit test; no emit.rs change yet |
| 4 | Region provenance in SafeWIR | `canonical_safewir/check*.rs` facts: region-born values carry no RC obligation; escape of a region-born pointer rejects (RFC-0178 §soundness) | nobody | 2 | fixture: region-born value escaping through return, closure, channel, `var` write-back each reject with a named diagnostic |
| 5 | Region allocation call site | the alloc site in `canonical_wasmir/emit.rs`, a few lines | `feat/mem64-emit` | 3, 4, mem64-emit landed | binary_trees in a `region:` shows 0 allocs/iter and 0 rc-ops/iter in the slice-1 table and is faster than master |
| 6 | Inline element layout, storage and reads | new files under `crates/witchy-wir/src/layout/` for fixed-record elements; read lowering through the existing packed-list path | nobody | 2 | `List(Point)` read loop shows 0 allocs/iter; existing packed scalar lists unchanged |
| 7 | Inline element in-place writes | `canonical_list_update.rs`, `canonical_owner_list_update.rs` | `perf/async-typed-frame-transport` | 6, that branch landed | `xs[i].x = 3` on a unique `var` list is one store: 0 allocs/iter; shared list CoWs once |
| 8 | Retire the zoo | `*_cap` helpers in runtime, `self_*` recognizers in `analysis.rs` | nobody expected by then | 5, 7 | RFC-0051's benchmark set shows no regression with the fast paths deleted; if any kernel regresses, keep only that one and record why |

Slices 1 and 2 start immediately and in parallel. 3, 4 and 6 start as soon
as 2 lands and run in parallel in three worktrees. 5 and 7 wait for the
named branches. 8 is the payoff and the proof that the general mechanism
replaced the special cases.

## Critical path

2 -> 3 -> 5 (gated on `feat/mem64-emit`) and 2 -> 6 -> 7 (gated on
`perf/async-typed-frame-transport`). Both external gates are branches already
in progress on 2026-10-08. If either stalls for more than a few days, the
blocked slice lands its new files and tests anyway and leaves the shared hunk
as a documented follow-up, so the queue stays moving.

## Baseline, 2026-10-09 (master `6329b3ab0`, release build, Apple M-series)

Counts are per whole kernel run from `scripts/kernel-ab.py`. Releases count
every `DropRef` entry, including the no-op drop of a static object, so
releases can exceed allocs plus retains.

| kernel | allocs | retains | releases | note |
|---|---:|---:|---:|---|
| fib, loop_sum, collatz, mandelbrot, closure_calls | 6 | 0 | 14 | pure compute; the Go gap is codegen, not memory |
| nsieve | 9 | 0 | 17 | |
| fannkuch | 18 | 0 | 26 | |
| list_sum | 28 | 0 | 36 | |
| record_build | 26 | 3 | 36 | |
| knucleotide | 200,028 | 628,540 | 657,138 | |
| expr_eval | 2,621,416 | 3,932,100 | 3,932,124 | |
| dict_count | 3,000,018 | 0 | 3,000,025 | one alloc per insert |
| word_count | 3,001,020 | 5,999,000 | 12,999,026 | |
| binary_trees | 13,107,156 | 19,660,600 | 19,660,664 | half the allocs are `Leaf` (BUG-0166) |

**Incremental wins the table exposes**, all lowering changes in
`canonical_lower.rs`, which `perf/async-transport-driver` is editing on
2026-10-09, so they land after it:

1. **Nullary variants as static objects** (BUG-0166, local `bugs/`). `None`,
   `Leaf` and every argument-less constructor allocate a 24-byte object. The
   static-data path strings already use removes the allocation, the retain and
   the release. Halves binary_trees' allocations.
2. **Move `let` locals into constructors.** `let a = Node(..); Node(a, Leaf)`
   retains `a`. A local used once is a move.
3. **Lend pattern-bound children to `let` callees.** `Node(l, r) -> check(l)`
   retains `l` for the call. binary_trees' `check` does four retains per node.

## What done looks like

- binary_trees, word_count, fannkuch, nsieve at 0 allocs/iter and 0 rc-ops/iter
  in their hot loops.
- RFC-0178 and RFC-0181 `implemented` for their stated boundaries.
- The `*_cap` and `self_*` families deleted or reduced to a measured residue
  with a reason per survivor.
- Wall time against Go reported as the consequence, with the paired A/B
  samples retained.

Channels (the 1000x column) are a separate project: 60% of their time is the
host closure-owner bridge, and the fix is a guest-side executor, not layout.
