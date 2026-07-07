# BUG-535: Container equality does not satisfy `PartialEq` bounds

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: `PartialEq`, container protocols, generic libraries, `std/testing`

## Problem

Direct list equality worked, but `List(Int)` did not satisfy an ordinary
`where a: PartialEq` bound. That made equality feel like compiler magic rather
than a first-class protocol: users could write `[1, 2] == [1, 2]`, but generic
library code could not ask for the same comparison capability.

The same gap blocked protocol-shaped helpers such as a generic
`testing.assert_value_eq(got, want)` over list values.

## Fix

Fixed by `d4a8f75` (`cmp: make list equality satisfy protocol bounds`).
`std/cmp.witchy` now provides blanket `PartialEq` and `Eq` impls for `List(a)`,
and compiled monomorphization carries the concrete list equality shape through
generic protocol calls.

Regression coverage:

- `example_tests::list_equality_satisfies_partial_eq_bounds_on_both_backends`

## Related

BUG-553 covers the follow-up nested container specialization work for
`Option(a)` and `Result(a, e)`. `Dict(k, v)` direct equality is covered, but
compiled equality through a generic `PartialEq` bound remains a separate known
gap and is not claimed fixed here.
