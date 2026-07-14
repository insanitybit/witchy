# BUG-498: `derive(Ord)` accepted records with `Float` fields

- **Severity:** MED
- **Status:** FIXED (commit e760a0e: derive rejects concrete Float for Eq/Ord)
- **Verified:** 2026-07-13 FIXED on master (e760a0e): derive rejects Float for Eq/Ord
- **Component:** `std/meta`, `derive(Ord)`, `derive(PartialOrd)`, `std/cmp`, total-order contract

## Problem

`Float` is deliberately only `PartialEq` + `PartialOrd`: `NaN` prevents
reflexive equality and total ordering, and `std/cmp.witchy` has no `Eq` or `Ord`
impl for `Float`.

The old derived comparison generator emitted field comparisons with `<` and `>`.
Those operators only require `PartialOrd`, so a record containing a `Float` field
could derive `Eq`/`Ord` and then trap when `compare` reached a NaN.

## Resolution

The derive pass now rejects concrete `Float` anywhere inside fields when deriving
`Eq` or `Ord`, because those derives would violate the total equality/order
contracts.

`std/meta.derive_ord` also emits field method calls through `Ord.compare`
(`self.field.compare(other.field)`) instead of `<`/`>`, so generated ordering
exercises the total-order trait. The method form deliberately avoids shadowing by
a same-module free function named `compare` such as `std/semver.compare`.

`std/meta.derive_partial_ord` now emits field calls through
`partial_compare`, propagating `None` for incomparable fields instead of treating
"neither less nor greater" as equality.

## Verification

- `cargo test -p witchy-types derive_ -- --nocapture`
- `cargo test derive_partial_ord_float_field_propagates_none_on_both_backends -- --nocapture`
- `cargo test --test e2e example_todo_workspace_runs_with_a_path_dependency -- --nocapture`
- `NEXTEST_TEST_THREADS=2 ./scripts/check.sh`

