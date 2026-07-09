# BUG-559: Duration interpolation renders raw milliseconds without `show`

Status: REJECTED
Severity: LOW
Component: RFC-0053 rendering model, `Duration`, interpolation

## Summary

An ignored local bug note proposed changing bare interpolation of a `Duration`
from the structural millisecond representation (`"${90000ms}"` -> `90000`) to
the human `Show` representation (`1m30s`).

That conflicts with the shipped RFC-0053 rendering model. Interpolation is
`show`-gated: modules that do not import/link `show` keep structural
`__render`, while modules that do import `show` route renderable values through
their `Show` impls. `std/show.witchy` already implements `Show for Duration`
using `duration.human`.

## Verification

The existing regression
`rfc0053_duration_interpolation_is_show_gated_on_both_backends` pins both
halves of the rule:

- without `show`, `90000ms` interpolates structurally as `90000`;
- with `import show`, interpolation, `show.render`, and `show.say` all render
  `90000ms` as `1m30s` on both backends.

## Resolution

Rejected as a bug. This is an intentional import-gated rendering boundary, not
a stale Duration omission.
