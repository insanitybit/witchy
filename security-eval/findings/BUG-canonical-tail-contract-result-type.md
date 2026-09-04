# Canonical contract derivation mis-typed tail-only recursive results

## Status

Fixed before RFC-0165 production cutover.

## Impact

Body-derived callable contracts inferred a tail callee's result types by
collecting values from ordinary `Return` terminators. A function implemented
entirely by a proper self-tail call has no ordinary return, so an `Int` result
was inferred as `Unit` and rejected with an unrelated
`CallableContractMismatch`.

This was fail-closed rather than an authority grant, but it made the canonical
pipeline incompatible with the RFC-0090 proper-tail-call guarantee.

## Fix

Tail-call result derivation now obtains result types from the checked callee
signature. Recursive summary iteration still derives memory provenance from
the body and never treats the declaration as memory authority; the signature
supplies only the already-checked value types.

The source-lowering regression now reports the exact checker error on failure.
