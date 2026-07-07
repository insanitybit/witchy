# BUG-479: Trait method names are effectively global

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Component: trait dispatch, method namespace hygiene, generic bounds, stdlib trait design
Discovered: 2026-07-06

## Summary

Trait method dispatch now carries trait identity instead of treating method names
as a global namespace. The lowering tables distinguish trait methods by
`(trait, method, receiver)` and keep inherent methods in a separate receiver
method table.

Bounded generic dispatch uses the active bound (and its supertraits) when
rewriting calls, so unrelated traits can both declare natural method names like
`name`, `from`, or `get` without declaration-order coupling. A concrete call on
a type that implements multiple traits with the same method name is rejected as
ambiguous unless an inherent method provides an explicit receiver-method target.

## Regression Coverage

- `same_named_trait_methods_dispatch_by_trait_identity` verifies two unrelated
  traits with `fn name(self)` lower bounded calls to their respective
  trait-owned implementations.
- The same test verifies an unqualified concrete `u.name()` call is rejected as
  ambiguous when both trait impls apply.

## Validation

- `cargo test -p witchy-types same_named_trait_methods_dispatch_by_trait_identity -- --nocapture`
- `cargo test -p witchy-types`
- `cargo test all_std_modules_type_check -- --nocapture`
