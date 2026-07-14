# BUG-559: Duration interpolation renders raw milliseconds without `show`

Status: FIXED on master (the one-rendering-protocol cut, `ed2d5dd1`)
Severity: LOW
Component: RFC-0053 rendering model, `Duration`, interpolation

## Summary

An ignored local bug note proposed changing bare interpolation of a `Duration`
from the structural millisecond representation (`"${90000ms}"` -> `90000`) to
the human `Show` representation (`1m30s`).

The import-gated RFC-0053 behavior was inconsistent: an otherwise-unused import
changed a value's observable display. `show` is now preluded, so interpolation
always routes a relevant type through `Show`. `Duration` therefore uses
`duration.human` in every module.

## Verification

The regression in `tests/rendering_protocol.rs` pins both spellings:

- with or without `import show`, interpolation renders `90000ms` as `1m30s`;
- `show.render` and `show.say` agree on both backends.

## Resolution

Imports expose names but do not select semantics. The former rejection is
superseded by the one-rendering-protocol cut.
