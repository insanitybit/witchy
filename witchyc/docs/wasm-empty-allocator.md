# Empty-only Buffer allocator

The direct compiler adds an opaque affine `Allocator`, a contextual empty Buffer
factory, shared capacity lookup and consuming release. This is a trusted storage
primitive, not a compiler-recognized Vec algorithm or initialized-element API.

```text
fn main(output: Console<Write>, arena: Allocator):
    let result: Result<Buffer<U8>, U64> = memory.try_alloc(&arena, 3)
    match move result:
        Ok(buffer):
            output.print_int(memory.capacity(&buffer))
            memory.release(move buffer)
        Err(code):
            output.print_int(code)
```

Main can request zero, one or two distinct Console/Allocator capabilities, in
either order; public `run` remains no-argument/no-result. Private entry IR alone
initializes Allocator by calling `allocator_authorize` for handle 2. Allocator
has no type arguments, source constructor, integer cast, field/equality access
or record storage. It can move through private parameters/results and be shared
by call-only reference; mutable capability references refuse. Actual host
allocator and output grants are separate, independently required operator choices.
A source capability or artifact field cannot authorize either grant.

`memory.try_alloc(&arena, capacity)` needs expected
`Result<Buffer<T>, U64>` context, selecting one concrete supported Copy T.
`memory.capacity(&buffer)` reads capacity under a checked shared loan.
`memory.release(move buffer)` consumes the empty owner without a source Allocator
argument. All heap-profile execution still needs the actual allocator grant.
Namespace resolution respects local and selected declaration shadows of `memory`.
Unsupported initialization/read/take/growth primitives remain refused.

Buffer shared references and standalone token frame cells are now enabled:
eight bytes/alignment eight, using the established i64 codec. A physical token
load under shared permission never becomes an owned source copy. No mutable
Buffer reference, stored/returned reference, Buffer record/array/phantom field or
owner element is added. Successful Buffer/Result values remain MUST-consume;
ignored results, leaks, double moves and moving while borrowed refuse before
proof resolution. The factory replaces the former main-Buffer-free gate; it does
not weaken linear flow. Capacity/Allocator loans span all later argument
expressions, including nested consuming calls.

## Actual profile and identities

`witchy.buffer.v1` defines exactly three private unshared memory32 pages,
196608 bytes, private SP 66560 and stack `[65536,66560)`, aligned eight.
Metadata is `[0,8192)`, static reserve `[8192,65536)`, another reserve
`[66560,98304)`, and heap `[98304,196608)`. The heap has 3072 quanta of 32 bytes.
There are 64 metadata entries of 32 bytes, occupancy bytes at `[2048,5120)`,
a reserved header `[5120,5152)` and otherwise reserved metadata bytes. No data
segments, start function, memory growth/import/export or extra public entry.
Fresh Wasm memory initializes allocator metadata to zero; it never initializes T.

Entries contain live/generation/type/base u32 fields at 0/4/8/12, capacity u64 at
16, requested_bytes u32 at 24 and quanta u32 at 28. Type identity is the concrete
interned Buffer descriptor id plus one, including nominal record identity;
equal layouts are insufficient. Stride/alignment come from one descriptor-driven
`type_info` helper, not caller-supplied layout facts. Unknown IDs trap, including
when an Allocator-only module has an empty type table.

Tokens encode `(generation-1)*64 + (slot+1)`. Zero is invalid. Decoding subtracts
and checks generation at full u64 width before any address wrapping. Metadata
live/generation/exact type, capacity/requested/quanta equations, zero-capacity
representation, base/alignment/full extents and occupancy are validated by
release before it writes. An access validates the token against its entry:
live, generation and exact type (`docs/wasm-runtime-cost.md`). Metadata entry address zero is a VALID validated result for slot zero,
never a null/failure marker. Capacity loads that entry; release must not confuse
it with an invalid token.

Identities belong to an instance domain. Fresh isolated instances may both use
numeric token 1; no cross-instance Buffer ABI exists. Within an instance,
generation tokens do not repeat. Free entries at generation u32MAX are excluded
from allocation without mutation or wrap. Zero capacity still owns a live metadata
slot, with base/quanta zero and no heap dereference.

## Read-only plan and bounded commit

Capacity is compared against `floor(98304/stride)` before multiplication.
Unsupported/oversized requests return Err 1. The planner first scans metadata for
a free entry below terminal generation (Err 3 if none), then first-fits a
contiguous occupancy run (Err 2 on quota/fragmentation). That error precedence is
deterministic. Planning uses locals/reads only; Err preserves every byte.
Terminal-generation exclusion never performs a failure-path retirement write.

After all checks, commit writes the bounded occupancy extent and full metadata,
increments generation and publishes live. It performs no fallible call or Err
return after the first write. Release validates everything first, clears only
its own occupancy bytes, marks live zero and preserves generation. Neither
allocation nor release reads/initializes/zeroes T payload bytes, including padding.
Live extents stay disjoint and occupancy is their union under these trusted
transitions. Arbitrary unadmitted Wasm can corrupt its own state and has sandbox
claims only, not these typed allocator guarantees.

A trap/interruption discards the instance; partial abnormal commit is not a
recoverable allocation failure or rollback promise. Normal typed empty release
is infallible only under the checked identity/state contract. Initialized values,
owner elements and destructor cleanup require separate rules and controls.

## One index/encoding source and binding

One named index structure owns optional print, required authorizer, source base,
arithmetic base and allocator helpers. Print is first when present, authorizer
next. With I actual imports and N source functions, allocator helpers are at
`I+N+6+ordinal`, ordered type_info, validate, try_alloc, capacity, release.
Their signature types follow the source type block and authorizer type.
Byte emission shares the existing integer/index encoder; there is no runtime
interpreter or alternate source reconstruction.

Heap receipts add runtime_profile, three-page frame_memory with allocator true,
and top-level `allocator` OBJECT containing contract/version/map, actual import
and helper indices, and descriptor-derived type IDs/strides/alignment. Buffer
frame_storage is true; capability_types adds affine Allocator only when selected.
Buffer type presence alone does not select heap: unused shared Buffer helpers
can retain the two-page frame profile. Actual Allocator use or a heap primitive
selects heap conservatively, including unused helpers. Nonheap report shapes stay
unchanged. Current source/compiler/module/host hashes and fresh source recheck
bind actual helper bytes; version strings or metadata alone prove nothing.

Compiler source controls describe contexts, entries, exact indices, linear duties,
loan conflicts, shadows and empty-only refusals. Qualification additionally needs
independent compiler/host review and actual planner/failure snapshots, generation,
zero-capacity, nominal/stale/high-token, sentinel/disjointness, grant, source-flow,
artifact and proof controls. Copied diagnostic modules exporting helpers/memory/SP
are explicitly unadmitted; production hosting refuses those exports. This is not
Vec/String, initialized storage, generic function, unsafe VC or proof-admission
reuse completion, and no performance improvement follows from source inspection.
