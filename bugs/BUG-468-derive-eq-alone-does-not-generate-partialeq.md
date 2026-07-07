# BUG-468: `derive(Eq)` generates the required `PartialEq` impl

Severity: MED
Status: FIXED
Fixed: 2026-07-06
Component: `derive(...)`, `std/meta`, comparison hierarchy, supertrait checking

## Problem

`Eq` refines `PartialEq`, and `std/meta` described `derive(Eq)` as deriving
both. The derive dispatcher nevertheless emitted only `impl Eq for T`, so a type
declared as `type T derive(Eq): ...` failed supertrait checking with a missing
`PartialEq` impl.

## Fix

The derive dispatcher now emits `meta.derive_partial_eq` before `meta.derive_eq`
when `Eq` is derived and a structural `PartialEq` impl has not already been
generated for that type. The explicit `derive(PartialEq, Eq)` spelling is
deduplicated so it remains valid after duplicate trait impl heads are rejected.

Regression coverage:

- `src/example_tests.rs::example_tests::derive_eq_alone_implies_partial_eq_on_both_backends`
