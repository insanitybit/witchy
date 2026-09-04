# Counted header load replaced authenticated extent identity

## Impact

After `UpdateCount`, loading the collection length header replaced the
checker's canonical logical-count definition with the load destination. A
runtime guard over the authenticated count therefore no longer matched a
subsequent element access, causing valid post-mutation indexing to fail closed
with `MissingBoundsGuard`.

## Fix

Header loads preserve the canonical logical-count identity for objects that
already have a counted extent. The focused regression
`packed_scalar_collection_access_mutates_and_preserves_headers` proves that the
guard remains attached to the updated logical length; no guard is synthesized
or weakened.
