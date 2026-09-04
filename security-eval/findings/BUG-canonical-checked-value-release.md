# Canonical `CheckedValue` could authorize `Release`

## Status

Fixed on `impl/rfc0165-canonical-v1`.

## Impact

Canonical call results with `ResultContract::CheckedValue` intentionally carry
read-only, non-destructive object authority. The initial checker represented
them as `Ownership::Shared` with no authenticated allocation class, but the
`Release` rule checked only `Shared`. A candidate could therefore apply
refcount release to an opaque result that was not established to be a
reference-counted allocation.

Depending on the returned pointer and backend helper behavior, that could
decrement arbitrary object metadata or reclaim unauthenticated storage.

## Fix

`Release` now requires both `Ownership::Shared` and
`AllocationClass::ReferenceCounted`. `CheckedValue`, static data, and other
objects without authenticated reference-counted storage fail closed.

The focused regression is
`canonical_safewir::check::memory::tests::release_rejects_shared_value_without_refcount_authority`.
