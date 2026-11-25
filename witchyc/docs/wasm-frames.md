# Call-scoped scalar borrows and fixed frames

> **Historical.** This page records a milestone of 2026-11 and its limits then; most have
> since been lifted. For what holds now see docs/wasm-references.md, docs/wasm-sums.md and
> docs/wasm-unions.md, docs/wasm-generic-functions.md and docs/wasm-generic-records.md,
> docs/wasm-memory-sizes.md, docs/wasm-hashmap.md and library/wasm-vec.wy.

The direct compiler accepts temporary shared and unique scalar references only
in direct call arguments. Borrowed parameters carry `&U8`, `&U16`, `&U32`,
`&U64`, `&Bool` or their `&mut` forms. The referent must be a scalar local or
by-value parameter, not an aggregate field or indexed element.

```text
fn read(p: &U64) -> U64:
    return *p

fn increment(p: &mut U64):
    *p += 1

fn forward(p: &mut U64) -> U64:
    increment(p)
    return read(p)

fn main(console: Console<Write>):
    var x = 41
    console.print_int(forward(&mut x))
```

This prints 42 when executed by the matching frame host. Ordinary by-value
parameters are immutable: `&x` can read one, but `&mut x` refuses. A unique
borrowed parameter permits writes through `*p`; its pointer binding cannot be
rebound, copied to a local or returned. Forwarding `p` is a call-lifetime
reborrow, optionally weakened from unique to shared permission. Explicit
`&mut *p` and `&*p` reborrows use the same referent identity. `&p`/`&mut p`
would borrow a reference and are refused. Only temporary calls can produce
these pointer carriers; source cannot construct one from integers.

## Checking calls

Every function is checked, including unused functions and unreachable statements.
The checker records each argument's root and shared/unique permission while
checking all later arguments. A unique loan prevents any other borrow or direct
read/write of its root until the call returns. Shared loans permit reads and
other shared loans, but block unique loans and writes. Nested argument calls
observe the outer loans. `two(&mut x, &mut x)` and
`use_unique(&mut x, read(&x))` therefore refuse. Forwarded parameters retain
their original referent identity, so `two(p, p)` also refuses.

Completed inner calls release their own loans before subsequent arguments.
Values already copied into earlier by-value arguments remain values, not loans.
A callee's different unique parameters are disjoint because every private caller
must establish that fact. Shared parameters may alias; none can be used to write.
There are no externally callable borrowed functions: only a no-argument,
no-result `run` entry is exported. No raw external pointer precondition is assumed.

References cannot be stored in locals, records, arrays, slices or generic record
arguments, nor returned. General loans/regions, owning values, allocator/heap
operations, unsafe primitives, generic functions and runtime contracts remain
unsupported. The existing fresh scalar lemma slice has no new axioms or runtime
VCs; a valid ghost lemma cannot excuse an illegal runtime borrow. Static checking
finishes before resolving any proof tool.

## Actual frame storage

Any selected borrowed parameter selects `witchy.frame.v1`, including an unused
function. A module with no borrows retains its zero-memory scalar encoding and
receipt. The frame module defines exactly one private, unshared memory32 memory,
minimum and maximum two pages (131072 bytes). It defines one private mutable i32
stack pointer initialized to 66560. The stack interval is `[65536, 66560)`,
downward-growing and aligned to eight bytes. Other bytes are excluded from
frames. There is no allocator, static data, memory export or memory growth.

The compiler plans the complete fixed frame from all address-taken scalar
locals/parameters in a function, including branches. The existing layout
descriptor supplies sizes and alignment; frame planning introduces no second
layout model. U8/U16/U32/U64 use exact-width unsigned memory loads/stores with
i64 register carriers. Bool uses one byte with an i32 carrier and normalized
loads/stores. Scalar initialization, assignments and ordinary reads of an
address-taken local all use its frame slot, so mutation through a borrow is
visible to later direct reads. By-value parameters copied into slots are stored
only after the complete prologue guard succeeds. Aggregates, operand-stack
values and compiler temporaries continue to use the engine stack/Wasm locals.

Before any frame write or SP change, the prologue checks that the old SP is in
the interval's inclusive endpoint range and aligned, and that the complete
aligned frame size fits `old_sp - stack_low`. The range check precedes this
subtraction, establishing nonwrapping arithmetic. Only then is the new SP
published and any parameter stored. Constant frame arithmetic is checked during
planning; bounded runtime subtraction/addition cannot wrap. An oversized frame
can compile but traps at the guard before partial allocation or any write.

Every normal early return restores the saved SP after evaluating its result,
including multi-value aggregate results. Unit fallthrough also restores it.
Caller-owned frames remain live across private calls; callee frames are disjoint.
Traps/interruption do not unwind or promise restoration: the host discards the
poisoned instance and preserves already captured output. Engine call-stack
exhaustion is separate from this linear-memory frame bound.

## Binding and remaining acceptance

Frame receipts retain the historical `witchy.scalar.check.v1` schema name with
two additive fields: `runtime_profile: "witchy.frame.v1"` and
`frame_memory: {pages: 2, maximum_pages: 2, stack_low: 65536, stack_high: 66560,
stack_alignment: 8, allocator: false, memory_exported: false}`. Both are omitted
for scalar modules. The actual module sections determine host profile selection;
receipt metadata grants no authority. The optional output import is versioned
`witchy.frame.v1.print_u64`, still requiring a separate operator output grant.
The scalar profile's other numeric execution/output/engine limits still apply.
There is no production memory export or borrowed host-buffer ABI.

The compiler-side source tests cover acceptance/refusal, profile shape and frame
boundary fixtures. Execution, recursive overflow, outside-stack sentinel
preservation, normal-return restoration, output-prefix survival, hostile module
surface and artifact binding controls require the separately coordinated host
checks. A test-only memory-export shim must remain explicitly unadmitted; the
production host refuses such an export. This slice does not establish owning
cleanup, per-object unsafe storage, general memory safety, generic Vec or reusable
proof admission. The checker/emitter remain trusted for object safety, while the
Wasm engine and host enforce sandbox confinement under actual grants.
