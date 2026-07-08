# BUG-556: Bytes, Ordering, and tuple equality bypass public protocols

Status: FIXED (this commit)
Severity: MED
Component: `std/cmp`, `Bytes`, `Ordering`, tuple protocols, BUG-538 release gate

## Problem

Direct equality for several core values worked structurally, but generic code
could not name the same capability through `PartialEq` / `Eq`:

- `Bytes` values could compare with `==`, but `fn same(x: a, y: a) where a:
  PartialEq` rejected `Bytes`.
- `Ordering` rendered and reflected as ordinary std data, but did not satisfy
  `PartialEq` / `Eq`.
- tuple values compared structurally, but tuple protocol coverage did not match
  the documented `Show` / `Reflect` arity-8 surface.

That made equality feel like backend magic instead of the public comparison
protocol.

## Fix

`std/cmp.witchy` now implements:

- `PartialEq` / `Eq` for `Bytes`, comparing byte contents;
- `PartialEq` / `Eq` for `Ordering`;
- tuple `PartialEq` / `Eq` through arity 8, matching `Show` and `Reflect`.

Regression coverage:

- `example_tests::core_protocol_matrix_composes_on_both_backends`

