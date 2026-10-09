---
rfc: 0178
title: "Region reclamation: rewind instead of per-object release"
status: proposed
created: 2026-10-08
revised: 2026-10-09
tracking: >
  Implements the reclamation strategy RFC-0167 §8 leaves to measurement.
  RFC-0175 §1 region handles are this mechanism plus a typed handle.
  Revised 2026-10-09 to make "no release work for region-born values" the
  first acceptance criterion, after the counted-objects draft measured slower.
---

# RFC-0178: Region reclamation

RFC-0167 §8 fixes what a `region:` block means: a lexical allocation scope
whose final value leaves by promotion or copy-out, whose region-born pointers
never reach outer bindings, and which does not change observable values. It
leaves the reclamation strategy to measurement. Today the canonical pipeline
accepts the block, promotes its value, and reclaims everything inside one
object at a time through reference counts. A region costs extra and saves
nothing: `binary_trees` with each tree built inside a region ran 219 ms
against 145 ms without one (2026-10-08, loaded host).

This RFC makes a region reclaim its storage by rewinding once at exit, and
removes every per-object release operation for values born inside it.

## Motivation

A reference-counted program pays to free what dies. When a structure dies
whole, the release walk visits every object a second time: `binary_trees`
builds and checks a tree of 131k nodes, then decrements and frees each node.
Go's collector never walks that tree; goose, whose benchmarks beat Rust,
frees a nested million-element structure with one store because every value
lives on a scope-owned stack. The same shape appears wherever a loop
iteration builds temporaries that all die together: parsers, a request
handler's scratch data, per-line buffers.

A region is the programmer's statement that everything allocated inside dies
at its exit. Rewinding makes that statement pay: allocation becomes a bump,
release inside the region becomes nothing at all, and exit frees the region's
storage in time proportional to its chunks, not its objects.

The first draft of this RFC kept per-object reference counts inside the
region and only skipped the free. That still emits a decrement, a zero test
and a branch at every drop, and the measured result was slower than no region.
The win is in deleting the operations, not in making them cheaper.

## Mechanism

1. **Region allocator.** Entering a region pushes an arena onto the task's
   region stack. While a region is active, every allocation on that task,
   including those made by callees, bumps from the innermost arena's chunks.
   Chunks come from the ordinary allocator and are returned to it whole at
   rewind. A rewound arena keeps its first chunk for the region's next entry,
   which is the loop-body case.
2. **No release path for region-born values.** A value whose provenance is
   region-born has no `DropRef`, no decrement, no zero test and no release
   walk. Where the checker knows the provenance statically (the common case:
   a value allocated by a constructor inside the region and never crossing a
   call that could return parent storage) the lowering emits nothing. Where
   it cannot (a value that may be either region-born or parent-owned, such
   as the result of a callee that returns one of its inputs) the lowering
   emits the existing `DropRef` guarded by one header bit, set at allocation.
   The guard is one load and one branch, and it is the fallback, not the
   design.
3. **Retains only where sharing must be observed.** Copy-on-write needs to
   know whether a value is shared. Inside a region the ownership analysis
   already proves most values unique (`var` locals, constructor results,
   `own` parameters), and a unique value is mutated in place with no count.
   A retain is emitted only at a site where a region-born value becomes
   shared and a later write to either copy is possible. Those sites are
   counted in the acceptance table and the target is to drive them to the
   genuinely shared cases.
4. **Promotion before rewind.** The block's value leaves through the existing
   promotion: a region-born result is deep-copied into the parent's allocator,
   and a parent-owned part passes through unchanged. Then the arena rewinds.
5. **Scalars and parent roots are unaffected.** Scalar assignment to outer
   bindings is allowed. Parent-owned objects read inside the region stay
   parent-owned and keep their ordinary counts.

## Soundness: no region-born pointer outlives the rewind

This is the part the checker, not the lowering, decides. A region-born
pointer reaching storage that outlives the region would dangle after the
rewind. With a dynamic allocator switch, every allocation made during the
region is region-born, including values that look parent-owned at the source
level:

- a copy-on-write copy of a parent value, made when the region mutates it;
- an element pushed into a parent collection through a `var` parameter of a
  callee called inside the region;
- a value sent on a channel or captured by a task created inside the region;
- a value held across a suspension point.

RFC-0167 already forbids region-born pointers in outer bindings, tasks and
suspensions. This RFC requires the SafeWIR checker to enforce it as a memory
rule:

- objects allocated while a region is active carry a region provenance;
- a store, write-back, send or retained capture of that provenance into an
  object or binding of the parent is rejected;
- the region's exit must promote every live value of that provenance it
  returns;
- a value with no release path must have region-born provenance; a value
  whose provenance the checker cannot decide must keep the guarded release.

A write that the source permits, such as a `var` parameter mutated in place
inside the region, must either be proven to store only parent-born or scalar
data, or be lowered to allocate in the parent explicitly (an allocator
override for that write). The checker certifies which.

Lowering that cannot meet the rule falls back to today's behaviour, where
reclamation is per object and correct by construction. The fallback is
reported, so a region that silently buys nothing is visible.

## Region handles (RFC-0175 §1)

An append-only arena under a region brand is this mechanism plus a typed
handle. `Handle(T, r)` is an index into a region-owned list, branded by the
region `r`, usable only while `r` is live. Because the brand proves the
backing storage is live and append-only, a handle dereference needs no bounds
check and no count: it is goose's relative reference and milo's `Arena<T>`
plus `Handle<T>` with the validity check moved to compile time. RFC-0175
owns the handle's type rules; this RFC supplies the storage, the brand's
lifetime and the rewind.

## Interactions

- **Opt mode.** RFC-0167 §8 says opt mode cannot conceal promotion copies.
  The region's exit copy stays explicit there.
- **Async.** A region cannot span a suspension (RFC-0167), so the region
  stack is per task and never crosses a yield.
- **Inline aggregate layout (RFC-0181).** A region-born list of fixed-size
  records is stored inline inside the region. The two mechanisms are
  independent: inline layout removes the per-element allocation, regions
  remove the per-object release. Together they are the goose representation.
- **RFC-0179 (memory64).** Chunk ranges and the header bit are pointer-width
  agnostic.
- **RFC-0177 (contracts and proofs).** "No use after rewind" is a validity
  invariant the verifier checks; the escape forms above are its harnesses.

## Acceptance

Measured with the kernel A/B harness (allocations, retains, releases per
iteration; paired release binaries, alternating, matching output).

| # | Criterion | Evidence |
|---|---|---|
| 1 | A region-born value emits no release operation: `binary_trees` with a per-iteration region shows 0 releases/iter and 0 frees/iter inside the region | Harness counters on admitted Wasm |
| 2 | Retains inside the region occur only at sites where a region-born value becomes shared and a later write is possible; the count per iteration is reported and justified per site | Harness counter plus a per-site listing from the lowering |
| 3 | `binary_trees` with a region is faster than without, and the region-free program is unchanged | Paired A/B, quiet host |
| 4 | Every region-born object is reclaimed at rewind and nothing else is | `canonical_heap_leaks() == Some(0)` across region programs; a parent value survives |
| 5 | Each escape form in the soundness list is rejected or lowered to parent allocation | One checker test per form; a forged module storing a region-born pointer into a parent object is refused |
| 6 | A value with no release path has region-born provenance | A forged module that omits the release of a parent-born value is refused |
| 7 | A region the checker cannot certify compiles with per-object reclamation and says so | Fallback test and diagnostic |
| 8 | Results are identical with and without regions | Expected-output tests on admitted unoptimized Wasm |

## Open questions

- Chunk size policy.
- Whether region inference (RFC-0167 §10) should insert regions around loop
  bodies whose allocations provably die each iteration. That applies the
  win without source changes and is the likely path to parity on kernels
  that are not written with regions.
- Whether the header bit is worth keeping once static provenance covers the
  kernel suite, or whether an undecidable provenance should simply refuse
  the region.
