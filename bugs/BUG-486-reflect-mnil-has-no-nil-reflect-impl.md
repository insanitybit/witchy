# BUG-486: `Nil` reflects to `MNil`

Severity: LOW
Status: FIXED
Fixed: 2026-07-06
Component: `std/reflect`, `std/json`, generated stdlib docs, `Nil` unit value

## Problem

`std/reflect` modeled unit values with `Mirror.MNil`, and `std/json` already
mapped `MNil` to `JsonNull`, but the ordinary `Reflect` protocol had no
`impl Reflect for Nil`.

That made the mirror type claim a unit shape while generic reflective consumers
such as `reflect.debug` and `json.stringify` could not accept a unit value.

## Fix

`std/reflect.witchy` now implements `Reflect for Nil` as `MNil`. The generated
stdlib docs now describe `Reflect` as the protocol and keep `MNil` as the unit
mirror case, so the previous misleading trait-heading comment is gone.

Regression:

- `nil_is_reflectable_on_both_backends`

Note: this regression exercises `Nil` through a Nil-returning helper call. The
separate bare-`Nil` expression backend issue remains owned by BUG-214.
