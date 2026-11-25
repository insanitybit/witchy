# Generic aggregate frame storage and call-only borrows

The frame compiler extends the scalar borrow slice to existing fully initialized
copyable records, concrete generic records and fixed arrays. It uses the same
`witchy.frame.v1` host contract. This page supersedes earlier scalar-only borrow
and field/index-place restrictions; ownership, allocation, general regions,
stored/returned references, unsafe primitives and reusable proof admission remain
unsupported.

```text
type Inner<T>:
    wide: U64
    small: T

type Outer<T>:
    inner: Inner<T>
    tail: T

fn edit(p: &mut Outer<U8>):
    (*p).inner.small = 9
    (*p).tail = 7

fn put(p: &mut [U8; 3], i: U64, value: U8):
    (*p)[i] = value

fn main():
    var record: Outer<U8> = Outer(Inner(100, 2), 3)
    edit(&mut record)
    var bytes: [U8; 3] = [1, 2, 3]
    put(&mut bytes, 1, 9)
```

Explicit dereference spelling is `(*p).field` or `(*p)[i]`. A call can borrow
`&record.inner.small`, `&mut bytes[i]`, or a nested field/index place. Direct
forwarding `callee(p)` and explicit `&*p` / `&mut *p` retain call-only reborrow
semantics. Whole-value `*p = value` is supported; constructors and returns use the
same concrete copyable ABI. By-value parameters remain immutable. Reference
bindings cannot be copied/rebound, placed in generic storage, stored or returned.
No arbitrary pointer/integer conversion or borrowed host entry is added.

## One root, one layout, initialized cells

A typed place carries the originating root index and its complete source type
separately from projected flat slots/physical byte offsets. The resolver handles
local roots, dereferenced private parameters, and nested field/index projections.
Reads, writes and call borrow arguments share this contract. Pure temporary
aggregate projections retain ordinary value operations. Projection visits count
against the existing 4096-operation function budget; write/borrow target traversal
is counted as well. The previous independent
scalar/field/index assignment reconstruction is removed.

The existing concrete layout descriptor is the sole authority for nominal types,
size/alignment, field offsets, flattened cells and array element size/stride. A
bounded traversal derives initialized scalar leaves with flat slot, scalar type
and physical offset. It permits at most 32 leaves and 4096 traversal visits.
Frame planning keeps each addressed root's full type; it never treats the first
flattened parameter register as the whole aggregate's type.

Frames reserve full root storage, including internal and trailing padding, then
align the next root. For `Inner<U8>` above, the U64 is at offset 0 and U8 at 8;
size is 16. `Outer<U8>` retains that inner trailing padding: its tail is at 16
and total size is 24. Array stride of this Outer is 24. Typed reads, writes,
parameter copies and whole-value assignments touch only offsets 0, 8 and 16;
they do not initialize, copy or overwrite padding bytes 9..15 or 17..23.

All RHS scalar leaves are evaluated and captured before the first destination
store, including `*p = Pair((*p).second, (*p).first)`. Addressed local ordinary
reads/assignments use the same physical cells as borrowed accesses. Fully
initialized root construction precedes any reference; no partial initialization,
move/drop tracking or cleanup rule is introduced. Packed U8/U16/U32/U64 and Bool
continue to use the established exact-width scalar codec.

## Indices, snapshots and whole-root loans

Every dynamic index is evaluated once, left to right. Its U64 value must be less
than the descriptor's fixed length before wrapping to i32, multiplying by physical
stride or adding the dynamic address. In particular, U64MAX and 2^32 cannot wrap
into a valid element. Write/borrow target indices are checked before RHS evaluation.
All valid physical/flat arithmetic is bounded by the checked containing layout;
there is no source integer-to-pointer path. Pure local-only programs remain
memory-free and use checked flat-cell selection instead of memory addresses.

Rvalue aggregate indexing preserves existing by-value evaluation order: the
initialized root value is snapshotted before evaluating its index expressions.
An index helper that mutates the original root therefore does not retroactively
change the value being projected. Write/borrow places identify the live root,
evaluate/check their indices and then evaluate the RHS or complete the borrow.

Loan conflicts deliberately use the whole canonical root. Different projections
are conservatively overlapping: `two(&mut r.left, &mut r.right)` and
`use(&mut r.left, r.right)` refuse, as do different array elements. Forwarded
parameter projections retain their original referent root. Claims span all later
argument evaluation and nested calls. Shared claims permit root reads/shares;
unique claims prevent other reads, writes or loans. Completed inner calls release
their own claims before later arguments. Different private unique parameters are
disjoint because every checked caller establishes this; raw borrowed functions
are never exported.

## Unchanged host/proof boundary and pending execution

Memory remains exactly two private unshared pages with private SP global 66560,
stack interval `[65536,66560)`, eight-byte alignment and no allocator/growth/data
or memory/global export. The complete frame guard precedes SP publication and
all parameter/local leaf stores. Normal returns restore SP after result
capture, including aggregate/multi-value results. Abnormal exits poison/discard
the instance. The compiler's only additional frame opcodes are i32 multiplication
and i32 wrapping of a range-checked i64 index; the scalar host profile remains
strict. The `frame_memory` receipt shape is unchanged; concrete
`aggregate_layouts`, actual bytes and compiler identity bind the new lowering.

All selected runtime code and templates remain checked before any fresh scalar
lemma tool resolution. A valid ghost lemma cannot excuse aliasing or reference
storage. No new axiom, runtime proof-obligation family, metadata-only admission,
trusted-store action or native execution fallback is introduced.

Compiler-side source tests cover typed acceptance/refusal, nominal/width/root
identity, layout padding/stride and oversized parameter-frame fixtures. Actual
padding sentinels, repeated/early/multi-value restoration, overflow before RHS,
index side effects, hostile module surfaces and exact artifact binding require
the separately coordinated host checks. A diagnostic exported-memory copy is
unadmitted test instrumentation; production hosting must reject it. This slice
establishes no heap/owning cleanup, generic Vec/String, unsafe storage proof or
complete Witchy language milestone.
