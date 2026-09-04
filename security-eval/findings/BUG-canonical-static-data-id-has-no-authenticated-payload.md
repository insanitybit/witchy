# BUG: canonical `StaticDataId` has no authenticated payload

## Impact

Canonical SafeWIR can state `Operation::StaticData { data: StaticDataId, ... }`,
but `CanonicalModule` contains no immutable table mapping that identity to the
bytes it denotes. A lowerer therefore cannot preserve UTF-8, embedded NUL, or
arbitrary static bytes, and a backend cannot mechanically encode the operation.
Accepting it by consulting an external or caller-provided table would detach
the emitted bytes from the validated module identity.

## Current behavior

The canonical checker and canonical WasmIR backend both reject `StaticData`
fail-closed. String lowering cannot be added soundly until the payload is part
of the authenticated SafeWIR graph.

## Required fix

Bind each immutable byte payload into canonical module serialization and its
digest, validate identity uniqueness and exact extent, and make WasmIR lowering
consume only that bound payload. Negative tests must reject a changed payload,
changed segment identity, or an extent inconsistent with the exact bytes.
