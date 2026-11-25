# Typed Copy Buffer storage

The active heap contract becomes `witchy.typed-buffer.v2`, with actual import
namespace/profile `witchy.buffer.v2`. It replaces the empty-v1 implementation;
the earlier empty allocator document describes that preserved historical slice.
Scalar and two-page frame profiles retain their existing behavior. This source
implementation awaits focused runtime qualification and independent review.

The storage API accepts concrete, non-unit, nonzero-sized Copy elements. Its
initialized operations require at most 30 flattened element carriers so that
`Result<T, U64>` remains within the existing 32-carrier ABI. Larger supported Copy
Buffers remain empty-only and can still transfer through allocation/release.

| Operation | Input permission | Result | Transition |
| --- | --- | --- | --- |
| `memory.init(&mut b, index, value)` | unique Buffer | `Result<(), U64>` | Empty to Full |
| `memory.read(&b, index)` | shared Buffer | `Result<T, U64>` | Full unchanged |
| `memory.take(&mut b, index)` | unique Buffer | `Result<T, U64>` | Full to Empty |
| `memory.swap(&mut b, index, value)` | unique Buffer | `Result<T, U64>` | Full unchanged, returns old value |
| `memory.try_release(move b)` | owned Buffer | `Result<(), Buffer<T>>` | free only when all cells Empty |

`memory.release` now gives an explicit migration error. `try_release` returns
the exact same owner on Err, without modifying any bytes. Its linear Result must
be matched or transferred. A caller must drain Full cells with take before
successful release. Copy read does not drain. Main still requests distinct
Console/Allocator capabilities and physical run has no arguments/results. The
actual host allocator grant remains separate from source authority and output.

## Guard and ownership order

Receiver and index execute exactly once. Identity and full-width index/state
guards finish before any index narrowing or init/swap value evaluation. Normal
Err 1 means bounds; Err 2 means wrong Empty/Full state. Either skips the payload
and all typed leaf loads/stores. Invalid identity, generation, nominal type or
corrupt metadata traps instead of returning a normal Err.

A Copy payload expression can consume another linear owner through a call.
The checker snapshots ownership after receiver/index and joins skipped/evaluated
payload flows. The skipped path keeps its owner duty; a maybe-live owner cannot
silently expire. Precompute such a value when its consumption must be
unconditional. Outer call loans and the receiver's unique loan remain active
through every argument. Borrowed Buffer tokens may be loaded physically under
permission but never copied/moved as owned source values. Header replacement,
stored/returned references and mutable Console/Allocator references remain closed.

## Initialization map and trusted transitions

The three private memory32 pages retain the existing stack, 96KiB heap, 64
metadata slots, generation tokens and quantum occupancy. New reserved state is:

| Region | Bytes |
| --- | --- |
| Initialization bitmap | `[8192, 20480)` |
| 64 u64 Full counts | `[20480, 20992)` |
| Remaining static reserve | `[20992, 65536)` |

An element's Full bit is indexed by its byte start relative to heap low 98304.
Quantum32 allocations never share bitmap bytes. Allocation's read-only plan
retains all previous overflow/quota/metadata/error-precedence checks. Successful
commit clears the complete allocated bitmap extent and resets the selected count
before publishing live; it never initializes or zeroes T or its padding. Normal
allocation Err preserves every byte, including bitmap/counts.

For live entries, Full count equals Full bits at valid element starts and cannot
exceed capacity; other bits in the extent stay clear. Validate adds count bounds.
Slot preparation checks the selected bit/count consistency before its normal
state error. Init writes complete captured Copy leaves, sets one bit and increments
count. Take captures all old leaves before clearing one bit/decrementing count.
Swap captures incoming and old leaves before stores, leaving bit/count unchanged.
Read changes nothing. The common Cell layout traversal and scalar memory codec
preserve nested/trailing padding; flattened registers never define byte offsets.

No fallible call, payload evaluation or normal Err follows the first commit write.
Full-width checked addresses bound every fixed leaf access. A trap discards the
instance; partial abnormal commit has no recovery/rollback promise. These are
reviewed trusted helpers and typed transitions, not new Lean axioms or guarantees
for arbitrary unadmitted Wasm.

## Helper and receipt binding

One Indices assignment drives emission and receipts: I actual imports, N selected
source functions, six old arithmetic helpers, then type_info, validate, try_alloc,
capacity, try_release and slot_prepare at `I+N+6+ordinal`. The first four helper
signatures stay unchanged. try_release returns `(i32 tag, i64 token)`; slot_prepare
takes four i64 metadata arguments and returns eight i64 metadata carriers:
error, entry, element address, bitmap byte address, mask, byte, count address, count.
Only error selects success: metadata entry address zero is valid. Normal slot Err
zeros the other physical carriers; it never supplies initialized T.

Payload cells stay inline in typed source functions. Void blocks and captured
locals assemble Results without additional multi-value block signatures or
payload-helper parameter overflow. The existing encoder, descriptor type_info,
Cell traversal and memory codec are shared. Bitmap masks use a bounded loop with
existing unsigned div/rem/mul; bit changes use checked-state addition/subtraction.

The allocator receipt keeps the previous map/grant/index fields, versions the
contract/profile, adds exact bitmap/count bounds and carrier limit, replaces
release with try_release/slot_prepare and records element carrier count plus
initialized-operation eligibility per descriptor. Source/artifact/current module,
host and actual proof binding still require fresh checks. No admission reuse,
Vec algorithm, generic function, owner element, ZST/drop, growth, element reference,
unsafe operation, String invariant or performance completion is added here.
