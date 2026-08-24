---
rfc: 0151
title: "Owned Byte and Text Construction: Mutable Bytes, Linear Build Plans, and Bounded Copying"
status: proposed
created: 2026-08-24
superseded-by:
related:
  - "0016 (reclamation floor)"
  - "0033 (place-based uniqueness)"
  - "0087 (uniform write-back and fused mutators)"
  - "0122 (borrow relations)"
  - "0127 (ownership and opt mode)"
  - "0140 (SIMD byte operations)"
  - "0144 (hybrid list and string primitives)"
  - "0146 (post-hybrid performance program)"
tracking: "Proposes a general owned byte/text construction contract and compiler build plan; implementation evidence will live in an RFC-0151 acceptance ledger."
---

# RFC-0151: Owned Byte and Text Construction

## Summary

Witchy has compact immutable `Bytes`, zero-copy borrowed string slices, SIMD
bulk copy, and capacity-aware string concatenation. It does not yet have a
complete, public way to construct and transform bytes in place, and its
compiler still lowers many loop-carried one-piece text appends as repeated
helper calls.

This RFC makes owned construction a general sequence capability:

1. `Bytes` receives value-semantic `var` mutators for allocation, indexed
   replacement, swapping, appending one byte, and appending a byte slice.
2. The compiler derives one typed `LinearBuildPlan` from ordinary ownership,
   layout, range, and escape facts. The plan applies to `Bytes`, `String`, and
   other contiguous owned buffers whose intrinsic metadata declares the same
   operation class.
3. A build plan hoists stable length/capacity state, performs at most one
   capacity check per fused append group, uses direct typed loads/stores, and
   materializes only at observable escape boundaries.

No benchmark name, source filename, biological alphabet, output record, or
specific standard-library method is an optimization input.

## Motivation

The 2026-08-24 fast benchmark snapshot reported:

| Workload | Witchy | Go | Ratio |
|---|---:|---:|---:|
| `fasta` | 432.9 ms | 285.7 ms | 1.51x |
| `reverse-complement` | 93.8 ms | 11.8 ms | 7.97x |

These rows expose general representation gaps.

The Go reverse-complement program stores bytes in a contiguous mutable byte
slice and indexes a 256-entry table. The Witchy program converts `Bytes` into
`List(Int)`, retains eight bytes per element, and transforms each element
through a branch ladder. Witchy's representation is safe, but it is not the
appropriate representation for byte processing.

The Go FASTA program reuses a fixed-capacity byte line. Witchy's ordinary
loop-carried string construction is already linear rather than quadratic, but
it still performs a helper transition for each small appended piece and cannot
generally turn a counted construction loop into one reserved write cursor.

Real programs have the same shapes: protocol framing, parsers, encoders,
serialization, checksums, compression front ends, template output, log
assembly, and file transforms. A named reverse-complement intrinsic would not
help those programs. A typed owned-buffer construction mechanism would.

The motivating numbers are exploratory, not promotion evidence. Phase 0 must
remeasure current master with the full paired-sample contract before any
implementation is promoted.

## Goals

1. Make contiguous mutable byte processing expressible without converting
   through `List(Int)`.
2. Preserve Witchy's value semantics: mutation is local to a `var` place, and
   aliases retain their prior value.
3. Make repeated text and byte construction amortized O(n), with exact counters
   for allocations, capacity growth, and bytes moved.
4. Use one typed build-plan mechanism across eligible contiguous buffers.
5. Improve byte- and text-heavy real-program fixtures, not only the motivating
   benchmarks.

## Non-goals

- A new `Byte` scalar type. Public byte values remain `Int` values checked to
  be in `0..=255`.
- Mutable borrowed aliases, shared writable memory, or raw-pointer APIs.
- Changing UTF-8 validity, String character indexing, `Bytes` equality, or
  cross-VM flat-buffer layout.
- A `reverse_complement`, FASTA, DNA, or fixed-alphabet intrinsic.
- New `*_cap` helpers or new per-method `self_*` ownership recognizers.
- Treating host output buffering as missing. The default runtime already
  captures and drains Console output in batches; live server output retains
  its explicit flush behavior.

## Public semantics

### Owned `Bytes` operations

`std/bytes.witchy` adds operations equivalent to the following surface:

```witchy
pub fn with_capacity(capacity: Int) -> Bytes
pub fn repeat(value: Int, count: Int) -> Bytes
pub fn push(var bytes: Bytes, value: Int)
pub fn append(var bytes: Bytes, let suffix: Bytes)
pub fn reserve(var bytes: Bytes, additional: Int)
pub fn set_at(var bytes: Bytes, index: Int, value: Int)
pub fn swap(var bytes: Bytes, first: Int, second: Int)
```

The same operations are available as methods. `capacity < 0`, a byte outside
`0..=255`, a negative index, an out-of-range index, and allocation-size
overflow abort consistently on both backends. `reserve` leaves contents
unchanged and guarantees room for at least `additional` more bytes when it
returns; a negative additional count aborts. These are programmer-contract
violations, matching `list.at` and `list.set_at`; fallible bulk conversion from
untrusted integer data remains `bytes.from_list`.

`append` observes the suffix value before receiver write-back. Appending a
buffer to itself therefore duplicates its original payload exactly once, even
if growth relocates the receiver.

The operations do not expose capacity as semantic state. Equal byte contents
remain equal regardless of allocation history. Reflection and serialization
observe only length and payload.

In normal mode, a shared value is copied before mutation. In `mode opt`, the
existing ownership rules decide whether mutation is legal and in place. The
public functions do not create a second mutability system.

### Text construction

No new `StringBuilder` object is required. Ordinary code remains idiomatic:

```witchy
var text = ""
while more():
    text = text + next_piece()
```

The compiler may retain `text` as an owned build root with length, capacity,
and write cursor while it remains local and unique. Calls, captures, borrows,
stores, reflection, host effects, and returns are explicit observation or
escape boundaries. At such a boundary the value is already a valid ordinary
`String`; there is no user-visible unfinished state.

Every appended String is valid UTF-8, so concatenation cannot create invalid
text. `Bytes` construction has no UTF-8 requirement.

## Compiler contract

### Intrinsic operation classes

The intrinsic catalog attaches operation metadata to eligible operations:

```text
ContiguousRead(width)
ContiguousWrite(width)
AppendScalar(width, validation)
AppendSlice(width)
Reserve
Finish
```

Lowering consumes the resolved intrinsic and the type checker's exact type. It
does not recognize source spellings or grow `analysis::InPlaceOp` by one case
per new method.

### `LinearBuildPlan`

The lowerer constructs a plan containing:

```text
LinearBuildPlan {
    root_place,
    value_kind: String | Bytes | PackedSequence(layout_id),
    payload_width,
    length_local,
    capacity_local,
    cursor_local,
    ownership_token,
    validated_range_facts,
    escape_epoch,
}
```

The plan is available only when the root place is exact, the payload layout is
known, and the current ownership state permits mutation. Unknown calls,
captures, returned borrows, root reassignment, and operations that may retain
the root invalidate it conservatively.

### General transformations

The first implementation contains independently measurable transformations:

1. **Grouped append reservation.** Consecutive append operations whose total
   byte length is available reserve once and copy through one cursor.
2. **Counted construction reservation.** A proven upper bound on produced
   length reserves once before the loop. Unknown or wrapping bounds retain
   geometric growth.
3. **Direct byte access.** Proven byte reads, writes, and swaps use
   `i32.load8_u` and `i32.store8` against the flat payload.
4. **Bulk append.** Appending a slice uses `raw_buffer_copy` with overlap-safe
   semantics where aliasing is possible.
5. **Loop-carried cursor reuse.** A stable owned root keeps length, capacity,
   and payload base in locals instead of reloading its header per element.
6. **Range-check elimination.** Byte-value validation is removed only when a
   typed range proof establishes `0 <= value <= 255`.

Each transformation has a deterministic applied/declined reason counter. The
unoptimized path remains available as a compiler control.

### Growth and overflow

Growth is geometric and bounded by linear-memory limits. The implementation
must check `new_length`, header addition, alignment, and capacity multiplication
before allocation. A failed proof uses the existing checked allocator. Integer
wrapping must never turn an oversized request into a small allocation.

The plan does not require allocation to occur exactly once. It requires bytes
moved and allocation count to remain amortized linear, and it records both.

## Generalization evidence

Promotion requires workload families in addition to FASTA and
reverse-complement:

| Family | Required fixture |
|---|---|
| binary protocol | construct length-prefixed frames from mixed scalar and slice fields |
| parser/transform | scan and rewrite escaped bytes with data-dependent output length |
| serialization | encode a list of records into a contiguous byte buffer |
| text generation | build variable-width UTF-8 records from multiple pieces |
| alias safety | mutate one owned copy while a prior alias retains its bytes |
| boundary behavior | pass, return, capture, reflect, and write completed values |

Each family runs at three geometrically increasing sizes. Results include
checksum, elapsed kernel time, allocation calls and requested bytes, bytes
copied, capacity growths, peak heap delta, and generated-code shape.

Phase 0 selects at least three existing repository code paths outside
`benchmarks/` before implementation begins. At least two of those unchanged
consumers must remain in the promotion matrix, so newly authored fixtures
cannot be the only evidence that real programs benefit.

## Acceptance criteria

This RFC is implemented only when all of the following are proven:

1. Interpreter and Wasm agree on every new stable `Bytes` operation, including
   byte validation, bounds failures, alias preservation, and empty buffers.
2. Generated WAT for eligible byte loops uses direct byte loads/stores and has
   no `List(Int)` conversion or universal-slot element traffic.
3. A known-length build performs exactly one payload allocation in total,
   excluding the canonical empty singleton, and moves no more than 1.10 times
   the final payload bytes, excluding required output copies at host or
   ownership boundaries.
4. An unknown-length build has O(n) total bytes moved and logarithmic capacity
   growth under geometric input sizes.
5. At least three non-benchmark workload families improve by 20 percent or
   more in whole-operation median time or allocation bytes, including at least
   two pre-existing repository consumers and at least two timing improvements;
   no family regresses by more than 5 percent.
6. `reverse-complement` improves by at least 4x and reaches at most 1.00x of the
   matched Go kernel; `fasta` improves by at least 20 percent and reaches at
   most 1.00x of matched Go.
7. `word_count`, `knucleotide`, `regex-redux`, `dict_count`, and `list_sum`
   remain within 5 percent of their protected medians.
8. Two warmups and twelve paired interleaved samples support promotion. Three
   deterministic counter runs agree exactly.
9. Emitted Wasm size for the complete benchmark and generalization corpus grows
   by less than 8 percent, and compile time grows by less than 5 percent.
10. The focused shards and serialized merge-queue gate pass, and the acceptance
    ledger names merged commits plus durable raw artifacts.

## Delivery slices

1. Phase 0: freeze current-master timings, allocation counters, byte-movement
   counters, correctness hashes, and source-pair equivalence notes.
2. Phase 1: land the stable `Bytes` mutator semantics and dual-backend tests.
3. Phase 2: add intrinsic operation classes and the conservative build-plan
   descriptor without enabling transformations.
4. Phase 3: land direct byte access and grouped append reservation separately,
   measuring after each.
5. Phase 4: land counted-loop reservation and cursor reuse.
6. Phase 5: update idiomatic benchmark programs to use the same representation
   class as their matched baselines, then run the full promotion matrix.

Changing a benchmark source is accepted only when input, output checksum,
measured region, and semantic algorithm remain fixed and the new source uses a
public API available to ordinary programs. Both the unchanged-source control
and idiomatic-source result are retained.

## Relationship to RFC-0144 and RFC-0146

RFC-0144 is frozen and implemented. Its flat `Bytes` representation, borrowed
String views, SIMD copies, packed Bool lists, and call-position format arena are
prerequisites to this RFC, not reopened claims. RFC-0151 adds stable owned
`Bytes` construction and a reusable loop-carried build plan that RFC-0144's
closeout does not claim.

RFC-0146's borrowed-string/hash residual remains separate. This RFC does not
change dictionary probing or borrowed-key equality. Its text work concerns
construction before an observation boundary.

## Alternatives

### Add a `Byte` scalar type first

A nominal byte type would make range validity static, but it expands literals,
arithmetic, traits, reflection, FFI, and migration. The existing `Bytes`
contract can provide dense processing now. A future Byte RFC may refine this
surface without invalidating the build plan.

### Expose raw buffers

Raw pointer, length, and capacity values would make fast code expressible but
would bypass ownership, bounds, and cross-VM safety. Compiler-owned layout
descriptors provide the same code generation without exposing unsafe state.

### Add specialized transformation functions

Functions such as `reverse_map_bytes` can be useful library conveniences, but
they do not solve general construction and risk making the benchmark the API
design. They may later be written in Witchy over the primitive mutators.

## Drawbacks

- `Bytes` changes from an API described as immutable to a value type with
  explicit local `var` mutation, requiring documentation and migration care.
- Range validation on dynamically supplied Int bytes has a real cost.
- Build plans add compiler state and conservative invalidation paths.
- Keeping an owned buffer live can increase transient capacity relative to an
  exact-sized allocation.
- Promotion requires meaningful non-benchmark fixtures and allocation
  instrumentation, so implementation is larger than a headline benchmark fix.

## Prior art

- Rust `Vec<u8>` and `String` distinguish owned construction from borrowed
  views while preserving UTF-8 at the String boundary.
- Swift and Kotlin expose copy-on-write value collections with local mutation.
- LLVM loop idiom recognition and scalar evolution show how counted fills and
  pointer induction can be derived from general control-flow facts.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior lives in spec/ and code, not here.
-->
