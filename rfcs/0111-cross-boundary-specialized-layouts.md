---
rfc: 0111
title: "Cross-boundary specialized layouts and destination passing"
status: implemented
created: 2026-08-01
updated: 2026-08-03
tracking: "All eleven acceptance criteria PROVEN in rfcs/0110-0112-acceptance-ledger.md. Criteria 1-8,11 (canonical LayoutId descriptors, cross-boundary packed values, physical generic specialization, callable-layout identity, host-import metadata, destination forwarding, header elision, closed-sum layouts, specs). Criterion 9: reviewed pinned-ARM report committed at bench/rust-class/reports/arm64-reference.json (geomean 0.92x, all cases within caps), timing gate activated in scripts/check.sh. Criterion 10: example_tests::rfc0111_layout::cross_lever_specialized_layout_slice_is_green_on_every_lever_and_backend (parity + always-on checked heap + full de-opt sweep)."
predecessors:
  - "[0027](0027-packed-layouts-sroa.md) (confined packed record lists and SROA)"
  - "[0029](0029-performance-tier-contract.md) (`mode opt` performance contract)"
  - "[0030](0030-perf-correctness-infra.md) (de-opt and deterministic firing evidence)"
  - "[0110](0110-opt-ownership-access-abi.md) (uniform ownership/access envelopes)"
related:
  - "[0005](0005-unforgeable-capabilities.md) (capability representation boundary)"
  - "[0016](0016-reference-counted-memory.md) (RC, reuse, and header-elision ladder)"
  - "[0034](0034-closing-the-compute-gap.md) (remaining compute/codegen levers)"
  - "[0089](0089-functional-in-place.md) (zero-operation unique state kernels)"
---

# RFC-0111: Cross-boundary specialized layouts and destination passing

> This RFC adds no source syntax. It completes the ABI and code-generation
> meaning of the existing `packed`, `unique`, `own`, `var`, and `mode opt`
> contracts.

## Summary

Make a statically known aggregate layout survive function, module, generic,
trait, closure, host, and isolated-worker boundaries without silently reshaping
to the universal boxed representation. Add destination passing so a callee can
construct a unique result directly into caller-owned storage.

RFC-0027 ships two local optimizations:

- escape-driven SROA for frame-confined records and tuples; and
- one flat buffer for a confined `List` of fixed-scalar records.

The declared `packed` qualifier is already a checked contract: supported local
uses are flat, and unsupported crossings reject rather than box. The remaining
work is the boundary itself. This RFC defines a canonical typed layout descriptor,
specializes callables by layout, transports RFC-0110 ownership state, teaches
host adapters the descriptor, and makes result placement explicit in the
compiled ABI while leaving source value semantics unchanged.

The goal is measurable: on a pinned scalar benchmark corpus with SIMD disabled,
`mode opt` monomorphic kernels and unique collection pipelines compete directly
with equivalent Rust implementations. The RFC does not declare victory from
allocation counters alone.

## Motivation

### Confinement is the current representation wall

A `List(Point)` can be one cache-dense buffer while it remains in one function,
but passing it to a helper, returning it, storing it behind a callable, or sending
it to a host adapter currently requires a loud rejection for declared `packed`
or a boxed fallback for inferred unboxing. That makes abstraction and data layout
fight each other.

Rust-class performance requires both:

- a programmer can factor a hot loop into helpers and generics; and
- the compiler retains the exact element layout across those helpers.

### Borrow checking is not a substitute for layout

Uniqueness proves an update is unobservable. It does not by itself turn an array
of pointers into an inline array, remove universal-slot conversions, flatten a
closed result, or let a callee build into its caller's destination. Those require
typed representation decisions and an ABI that carries them.

### A Rust comparison is absent

The checked benchmark suite compares Witchy with Go. RFC-0029 names Rust-class
struct/numeric performance as an opt-mode target, but the repository has no
paired Rust leg and no scalar-only acceptance threshold. This RFC adds that
missing evidence contract.

## Design principles

1. **One logical type, specialized physical layout.** Layout is unobservable:
   equality, reflection, rendering, errors, and capability behavior are identical.
2. **Declared `packed` never silently boxes.** The compiler either carries the
   packed representation through a supported boundary or reports the boundary it
   cannot represent.
3. **Inferred specialization may reshape in normal mode.** A best-effort normal
   value may box at a cold boundary; statistics expose the reshape. `mode opt`
   rejects a reshape on a required hot path.
4. **Ownership and layout are orthogonal facts.** RFC-0110 answers who may reuse
   storage; this RFC answers what that storage contains.
5. **No raw native pointers cross the sandbox.** Every layout remains valid Wasm
   data/reference structure and every host read uses a checked descriptor.

## Canonical layout descriptors

Type checking and monomorphization produce a canonical `LayoutId` for every
closed physical type. A descriptor records:

- size and alignment;
- scalar field kinds and offsets;
- aggregate nesting;
- list element stride and capacity layout;
- owning-object, view, externref, and GC-reference positions;
- RC/header requirements;
- copy, equality, render, dup/drop, and serialization shapes; and
- a stable descriptor hash used in caches and boundary validation.

Qualifiers affect ownership obligations but do not create a different logical
type identity. `packed` does affect the physical-layout contract and therefore
the `LayoutId`.

The descriptor is the only source for WIR loads/stores, host adapters, generated
copy/drop helpers, and callable physical signatures. A helper may not reconstruct
layout from a type name or operation name.

## Initial specialized layouts

### Packed records and tuples

The first implemented class contains only fields with fixed closed layouts:

- `Int`, `Float`, `Bool`, and `Duration`;
- nested packed records/tuples; and
- closed fieldless tags where their width is fixed.

The physical representation uses each scalar's language width and natural Wasm
alignment rather than an unconditional 8-byte field slot. Padding is explicit in
the descriptor and deterministic across hosts.

Strings, ordinary lists/dicts, capabilities, closures, open type variables,
existentials, and borrowed views are not inline fields in the first class. The
compiler rejects a declared-packed type containing one and names the field.

### Packed lists

`List(P)` for packed `P` is one owning buffer:

```text
[object header when required][length][capacity][P0][P1]...[Pcapacity-1]
```

There is one ownership/reclamation state for the whole buffer, not one allocation
per element. Indexing is base plus checked index times descriptor stride plus
field offset. RFC-0034 bounds elision may remove the logical check only when its
range proof fires.

### Closed sums

After records/tuples/lists are green, fixed-layout closed sums may specialize to
a tag plus a descriptor-sized payload band. Reference-bearing or open sums retain
their existing typed GC/boxed representation. This stage is separately measured;
it is not required to land the record/list foundation, but it is required before
the RFC may be marked implemented.

## Cross-boundary ABI

### Direct functions and modules

A parameter/result whose closed type has `LayoutId L` uses `L` on both sides.
The linker rejects two declarations that agree logically but disagree physically.
Because `mode opt` imports are transitive, linked opt modules form one layout
domain and may share descriptors directly.

### Generic monomorphization

A generic callable is specialized by concrete logical types, conventions,
qualifiers, and layout IDs. Calls from boxed and packed sites receive distinct
compiled instances where their physical ABI differs. The specialization cache
key includes every layout descriptor hash and the active optimization schema.

Open generic compilation never guesses a packed layout. It either waits for a
closed instantiation or uses the uniform representation allowed by its source
contract.

### Function values, closures, traits, and existentials

RFC-0110's exact access signature is extended with layout IDs. A function value
or witness slot may be used only with an exact physical signature. Adapters may:

- reorder or flatten fields without allocation when representations are
  byte-compatible; or
- materialize an explicit owned reshape in normal mode.

They may not erase a declared-packed contract. `mode opt` rejects any adapter
that allocates, reshapes, or loses a destination on a required path.

Owned existential boxing remains outside the packed guarantee unless a later RFC
defines a closed witness/layout set. This RFC does not promise devirtualization
for arbitrary `dyn Trait`.

### Host functions

A host import/export that consumes structured guest memory declares the accepted
layout IDs in generated ABI metadata. The linker chooses one of three outcomes:

1. the host supports the exact descriptor and reads/writes it directly;
2. a generated checked marshal adapter converts at the boundary, permitted in
   normal mode and counted; or
3. compilation rejects the declared-packed opt boundary.

Capability-bearing fields remain in typed reference storage and are never copied
into forgeable linear-memory slots. RFC-0005's representation classifier remains
authoritative.

### Isolated workers and artifacts

Worker VMs and persisted artifacts do not share raw pointers or engine-specific
GC references. Packed values cross through descriptor-driven serialization. The
artifact embeds the descriptor schema/version; a consumer rejects an unknown or
mismatched schema before instantiation.

## Destination passing

When a call returns `unique T`, the physical ABI may accept a hidden destination
described by `LayoutId(T)` and RFC-0110 ownership state. The caller supplies:

- reusable storage from a dead unique value of compatible layout and capacity;
- a region/stack destination proven to outlive the result; or
- no destination, in which case the callee allocates normally.

The callee initializes the destination exactly once on every successful return
path. It may not read uninitialized fields. A `var` result/write-back and an
ordinary return use the same simultaneous result envelope, so destination
selection cannot change write-back order.

Source code needs no destination parameter. Existing source shapes state intent:

```text
fn build(own seed: unique Buffer, let input: Bytes) -> unique Buffer:
    ...
```

In normal mode, lack of a compatible destination allocates. In `mode opt`, a
function or call site covered by a no-allocation contract rejects with the value
that kept the candidate live or the boundary that lost its layout.

## Header and count elision

A value proven `unique` throughout its complete ownership graph needs no dynamic
sharing count. For supported packed buffers, the compiler may select a
header-free layout when:

- every constructor, call, return, and drop is in the same closed layout domain;
- no normal-mode or dynamically typed boundary can share it;
- no borrowed owner-root operation requires the header; and
- the exact lifetime/drop point is statically known.

Otherwise the ordinary RC header remains. Header presence is a descriptor fact,
not inferred independently by each allocator. An opt contract may require
header-free storage only after deterministic counters and size assertions prove
the path.

## Compiler pipeline

The implementation introduces a typed layout phase between monomorphization and
WIR encoding. It produces:

1. closed layout descriptors;
2. callable specialization keys;
3. representation conversions, each explicit in checked IR;
4. destination opportunities and rejection reasons; and
5. copy/drop/equality/serialization operations derived from the descriptor.

These facts belong on the shared typed CFG/SSA representation. Binaryen and
Cranelift optimize the emitted Wasm after Witchy has made ownership, layout, and
destination decisions; they are not expected to recover erased high-level facts.

## Rust-class benchmark contract

Add paired Witchy/Rust programs for:

- scalar integer and float loops;
- packed record traversal and update;
- list construction, map, filter, and reduction;
- closed-sum dispatch;
- helper/generic abstraction over the same packed data;
- destination-built string/byte/record results; and
- allocation-heavy recursive values after the closed-sum stage.

The harness uses two separate measurements:

- **kernel:** warm in-program computation, excluding process/JIT startup; and
- **wall:** complete invocation, reported but not used to hide kernel gaps.

The Rust reference uses the same algorithm, integer overflow behavior, bounds
contract, result validation, and one thread. SIMD and autovectorization are
disabled, and the harness verifies the produced reference code contains no vector
instructions in the measured kernel. Both programs emit the same independent
result before timing.

On the pinned reference machine, completion requires:

- geometric-mean Witchy/Rust kernel ratio at or below **1.25x**;
- no core scalar/packed benchmark above **1.50x** without a checked exception
  naming the remaining mechanism;
- zero representation reshapes and zero boundary re-own copies in every accepted
  opt hot path;
- allocation counts within the algorithm's declared budget; and
- bounded live memory in the long-running variants.

The thresholds are regression gates for this RFC, not a claim that every Witchy
program is faster than every Rust program.

## Diagnostics and tooling

`witchy check` and the LSP report:

- the logical type and selected layout;
- the exact boundary causing a reshape or box;
- the specialization that would be required;
- why a destination cannot be reused;
- whether the problem is a declared-packed violation or a best-effort miss; and
- the source statement that kept a candidate shared/live.

`witchy stats` reports layout IDs plus counts for packed allocations, boxed
elements, reshaped bytes, destination hits/misses, RC headers emitted/elided, and
specialized callable instances.

## Acceptance criteria

1. One canonical typed descriptor drives codegen, host adapters, copy/drop,
   equality, rendering, logical reflection traversal, and serialization for every
   specialized type. Public reflection does not expose physical offsets, padding,
   headers, or destination state.
2. Declared packed records/tuples and `List(P)` cross direct function and user
   module boundaries with no boxing or reshape.
3. Generic functions specialize by layout and preserve packed data through at
   least construction, indexed traversal, mutation, and return.
4. Function values, closures, and trait calls either preserve the exact layout
   and RFC-0110 ownership envelope or reject; no declared-packed call silently
   adapts to boxed storage.
5. Host boundaries accept a declared descriptor, use an explicit counted marshal,
   or reject. Capability references never enter linear-memory scalar fields.
6. Unique results can be destination-built into compatible caller storage, with
   deterministic tests proving zero intermediate allocation and correct behavior
   on every return/write-back path.
7. Header-free unique storage is selected only from whole-graph proof and remains
   byte/value equivalent to the RC-backed form under the differential sweep.
8. Fixed-layout closed sums complete the descriptor, ABI, equality, drop, and
   benchmark matrix.
9. The Rust benchmark harness meets the stated scalar-only thresholds on the
   pinned reference machine and checks result equivalence before timing.
10. Cross-lever, checked-heap, redzone, UAF, interpreter/Wasm parity, runnable
    book, and artifact compatibility suites are green.
11. `spec/architecture.md`, `spec/language.md`, and `spec/performance.md` replace
    the confined-layout limitation with the exact shipped boundary matrix.

The RFC remains `proposed` until all eleven criteria are proven in a checked
acceptance ledger. Local packed success alone is already RFC-0027 and does not
complete this RFC.

## Staging

1. **Layout descriptor and IR plumbing.** No behavior change; existing boxed and
   confined-packed paths must reproduce byte-for-byte.
2. **Direct/module ABI.** Packed records and lists cross direct boundaries.
3. **Generic and first-class ABI.** Specializations, closures, and traits consume
   RFC-0110's frozen interface.
4. **Destination passing and header elision.** Land separately with counters and
   de-opt comparisons.
5. **Closed sums and host/worker adapters.** Complete the boundary matrix.
6. **Rust-class evidence.** Freeze reference programs and numbers only after all
   mechanisms above are active.

## Alternatives

- **Keep packed data local.** Rejected: it makes abstraction a permanent
  representation cliff and cannot meet the stated performance goal.
- **Give every type a packed ABI.** Rejected: dynamic/open/reference-bearing
  shapes need their existing safe representations, and code size would explode.
- **Silently box at unsupported boundaries.** Allowed only for inferred normal-mode
  specialization. It violates declared `packed` and `mode opt` contracts.
- **Expose output buffers in every source API.** Rejected. Destination passing is
  an ABI optimization derived from existing ownership contracts; source-level
  `var` remains available when write-back is semantically desired.
- **Restore an unrestricted native backend first.** Rejected. Better lowering and
  a scalar benchmark must establish the remaining engine gap while the Wasm
  sandbox stays non-negotiable. A later AOT engine may consume the same checked
  Wasm/layout contract.

## Drawbacks

- Layout-specialized callables increase compilation time, cache cardinality, and
  artifact size. The implementation needs deterministic specialization limits
  and must report when it selects the uniform representation instead.
- Descriptor-version compatibility becomes part of the artifact and host ABI;
  changing a descriptor schema can require rebuilding consumers.
- Destination passing and header-free storage add physical signatures that are
  harder to inspect than the universal representation, increasing the burden on
  WIR dumps, counters, and differential tests.
- Scalar-only Rust gates intentionally leave SIMD performance unanswered. They
  establish the non-vectorized floor, not the eventual peak.

## Prior art

- [Fully in-Place Functional
  Programming](../external-refs/fip-fully-in-place-2023/notes.md) gives the
  zero-allocation basis for reusing unique inputs and destinations.
- [Perceus](../external-refs/perceus-2021/notes.md) informs reuse and count
  elision for acyclic immutable values.
- [Region-Based Memory
  Management](../external-refs/region-based-memory-1997/notes.md) informs the
  caller-scoped destination case and its lifetime restriction.
- RFC-0017's survey of unboxed monomorphized layouts records the repository's
  earlier code-generation analysis; RFC-0027 then implemented the confined
  subset that this proposal carries through boundaries.

## Non-goals

- No SIMD; it is disabled for the acceptance comparison.
- No unsafe pointer arithmetic or user-visible addresses.
- No stable external C ABI for arbitrary Witchy values.
- No promise to devirtualize open existentials.
- No new source syntax.
- No performance claim without the paired Rust evidence.
