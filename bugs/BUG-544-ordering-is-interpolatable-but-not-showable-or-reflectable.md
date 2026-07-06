# BUG-544: `Ordering` is showable and reflectable

Severity: MED
Status: FIXED
Fixed: 2026-07-06
Component: `Ordering`, `Show`, `Reflect`, JSON reflection

## Problem

`cmp.Ordering` is the central enum returned by `Ord.compare` and used by
sorting/comparison helpers. Structural interpolation could already render it as
`Less`, `Equal`, or `Greater`, but the public protocols did not expose that same
shape:

- `show.say(console, o)` failed without `Show for Ordering`.
- `reflect.debug(o)` and `json.stringify(o)` failed without
  `Reflect for Ordering`.
- `derive(Reflect)` failed for records containing an `Ordering` field.

That made a core stdlib value feel implementation-shaped: a hidden renderer knew
how to display it, while the principled protocols did not.

## Fix

`std/show.witchy` now implements `Show for Ordering`, matching interpolation.
`std/reflect.witchy` now implements `Reflect for Ordering` as a nullary
`MVariant("Ordering", variant, [])`, so JSON reflection encodes it with the
standard tagged variant representation.

Regression:

- `ordering_is_showable_and_reflectable_on_both_backends`
