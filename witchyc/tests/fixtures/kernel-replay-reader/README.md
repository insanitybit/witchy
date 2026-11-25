These are source fixtures, not qualification evidence. A scheduled host check
must compile the new reader, compile `legacy.lean` to one main `.olean` with
`WITCHY_READER_SENTINEL` unset, run `forge.lean` with its sole owned output
directory, then compile `controls.lean` with these environment variables:

- `WITCHY_READER_INPUT`: that producer's legacy main object.
- `WITCHY_READER_OWNED`: exclusive existing snapshot/fixture directory.
- `WITCHY_READER_SENTINEL`: an absent file inside that owned directory.

Use current SDK, default limits, warnings as errors and the standard axiom audit.
Do not import the producer. `controls.lean` must leave the initializer sentinel
absent and the current environment without `RawReaderOld` declarations. Its
positive theorem contains a private auxiliary closure from raw metadata and
retains additional/reordered current premises. Its negative controls cover old
local data versus a false current target, unsupported opaque/universe data,
cyclic/mvar raw payloads, selector/node/depth misses, truncation and exhaustion.
The final budget test must remain last. Save its current output `.olean`, remove
producer/snapshot paths from the next process's search path, and load only that
new object to check serialization independence.

Lifetime evidence is the current SDK `Lean.CompactedRegion` contract: objects in
its region have no individual reference counts; freeing requires the explicit
unsafe `CompactedRegion.free`. This reader never calls free and roots every
loaded `(ModuleData, CompactedRegion)` in a private process-global arena. It
reserves 8 MiB before each load, at most eight times; rejected loads still consume
reservations. Owned candidates and their Expr/Name/Level children can outlive a
Snapshot handle and remain alive through current frontend serialization. Worker
exit reclaims the arena. This is a 64 MiB reservation/file mapping bound, not a
physical RSS claim or a promise that arbitrary binary files are a safe parser
input. The Lean SDK binary loader remains trusted; the future receipt bridge
must supply exact old compiler-produced, main-only bytes under the current SDK.

Limits: only legacy v2, non-module main objects; no `.private`, `.server`, foreign
imports or initializers. Only monomorphic safe definitions/theorems in an acyclic
bounded owned closure are inlined; SDK references remain current references.
Nonempty or unknown expression metadata is conservative MISS in this slice.
No cache/UnitChecker/compiler integration or performance claim is included.
