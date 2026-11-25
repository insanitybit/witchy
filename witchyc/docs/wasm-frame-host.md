# Private scalar call frames

The `witchy.frame.v1` host profile extends the existing strict Wasm host for the
first call-scoped scalar-borrow slice. It does not implement owning storage,
allocation, generic Vec/String, unsafe proofs, reusable admission or object
safety for arbitrary Wasm. Zero-frame modules keep `witchy.scalar.v1` and its
zero-memory limits. The checker receipt schema remains `witchy.scalar.check.v1`
with an additive frame-profile extension.

The host derives the profile from actual module sections before instantiation.
A frame module has exactly one private, unshared memory32 with minimum and
maximum both two pages (131072 bytes), and one private mutable i32 global
initialized to 66560. The downward stack interval is `[66560 - stack, 66560)` with eight-byte alignment,
`stack` being the build's `--stack-bytes` (16384 by default; 1024 before 2026-11-18). Only `run: () -> ()` may be exported. There is no production
memory export, imported memory/global/table, start function, static data, element
segment, custom metadata, memory growth, memory.size, indirect call, reference,
SIMD, atomic or shared-memory feature.

Closed preflight additionally permits stack-global get/set at index zero, i32
add/sub/and/unsigned lt/gt, and the integer scalar storage codecs used by the
compiler. Memory instruction offsets must be zero and alignments no greater
than their natural exponent. Engine validation checks the same copied bytes
before instantiation. Counts, module size, output, worker heap/stack and execution
deadline retain the scalar host limits; `FRAME_LIMITS` adds the fixed linear
memory and stack map. Lean's proof-tool budget is separate from runtime hosting.
These caps do not establish native process RSS or whole-host availability.

The sole optional import is
`witchy.frame.v1.print_u64(i32 handle, i64 value) -> ()`. It requires an explicit
operator output grant and handle one; building an artifact grants no output
authority. Import namespace must match the actual memory-derived profile.
Output is copied into bounded host-owned capture, and its committed prefix
survives a later trap or timeout. Every invocation uses a new private worker and
instance and is discarded on success, trap or interruption. The host exposes no
live guest-memory view, raw borrowed-function entry or reentry path.

The trusted compiler/checker establishes per-object borrowing, complete frame
size/alignment checks before any stack write or pointer change, and restoration
on every normal return. The sandbox alone guarantees only memory confinement;
arbitrary valid Wasm may still write another object or the excluded stack-adjacent
bytes within its own memory. Abnormal exits discard the instance without guest
unwinding. No owning cleanup or allocation contract is implied by scalar frames.

Frame checker receipts add `runtime_profile: "witchy.frame.v1"` and
`frame_memory` containing pages/maximum_pages two, stack_low 65536, stack_high
66560, stack_alignment eight, allocator false and memory_exported false.
Artifact creation and execution compare that report against actual bytes and
bind actual profile/limits in the instantiation metadata. Each artifact run
still freshly recompiles packaged source with the operator-selected trusted
compiler and optional Lean root, compares exact module/receipt identities, and
validates the independently supplied host grant. Existing proof audit, complete
installation hashes, process-group cleanup and no-overwrite publication remain
in force. Metadata cannot select a profile or confer authority.

`scripts/test-wasm-frame.mjs` (a Node suite, removed 2026-11-16 with Node; docs/decisions.md) contains the focused controls. Its explicitly
UNADMITTED raw-engine diagnostic first validates original bytes, adds a memory
export to a separate copy, initializes sentinel bytes, and returns copied
observations after finite compiler fixtures. Production preflight rejects the
modified copy; production code never imports the diagnostic. These observations
exercise normal repeated/early returns, recursive and oversized frame traps,
and the entire memory outside the reserved stack. Separate hostile fixtures
verify malformed profiles refuse before effects and memory growth/export cannot
be enabled by metadata. The diagnostic is not an artifact-admission route.

The controls are source-written and require focused coordinator execution with
the combined compiler/host candidate before any qualification claim. They do
not constitute a historical full gate or completed consensus milestone.
