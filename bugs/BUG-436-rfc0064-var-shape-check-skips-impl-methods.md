# BUG-436: RFC-0064 `var`-shape checks cover impl methods

Severity: HIGH
Status: FIXED
Fixed: 2026-07-06
Component: RFC-0043/RFC-0064 mutation classification, impl method lowering, type checker pass order

## Problem

RFC-0064 row-3 validation rejected abolished combined write-back+return shapes
on top-level free functions, but impl methods were lowered only after that
pre-lowering check. Static and instance methods could therefore accept explicit
`var` parameters with non-`Nil` returns even though the same shape was rejected
for free functions.

## Fix

The checker still runs the declaration-shape validation before lowering for
source-quality free-function diagnostics, then runs the same validation again
after trait/impl lowering. At that point impl methods are ordinary functions
with concrete receiver annotations, so the same `var` contract is enforced on
every callable surface both backends consume.

Regression coverage:

- `crates/witchy-types/src/typeck_tests.rs::rfc0064_row3_var_shapes_are_rejected_in_impl_methods`
