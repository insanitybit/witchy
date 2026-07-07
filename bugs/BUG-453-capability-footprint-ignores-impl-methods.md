# BUG-453: Capability footprint ignores impl methods

Severity: HIGH
Status: FIXED (this commit)
Verified: 2026-07-07
Component: capability footprint, package manager, `compiler.footprint`, impl methods, public API gates

## Summary

Capability footprint analysis now scans impl methods as callable API surface.
Public self-less/static methods such as `Client.open(net, ...)` and public
receiver methods such as `builder.send(net)` contribute to `entries` and the
module `total`, so package and registry widening gates see the authority they
add. Private impl helpers with capability-typed signatures remain visible in
`per_function` for audit output, but do not widen the public `total`.

## Fix

`capabilities::analyze` now factors the existing function-signature scan and
applies it to both top-level functions and `Item::Impl` methods. Impl method
entries use stable source-facing names:

- inherent methods: `Type.method`
- trait impl methods: `Trait for Type.method`

Regression coverage lives in:

- `capabilities_tests::impl_methods_contribute_to_capability_footprints`
- `capabilities_tests::adding_public_impl_method_authority_is_a_widening`
