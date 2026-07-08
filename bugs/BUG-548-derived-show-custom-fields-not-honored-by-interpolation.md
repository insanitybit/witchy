# BUG-548: `derive(Show)` with custom-Show fields was ignored by interpolation

Severity: MED
Status: FIXED
Component: `derive(Show)`, interpolation, `Show`, RFC-0053 render semantics

## Problem

`derive(Show)` generates ordinary Witchy code that renders each field or payload
through `show(...)`. That means a derived wrapper around a custom-Show value must
inherit that field's public display form.

The old bug report captured a semantic split where interpolation kept derived
`Show` types on the structural `__render` path. That made `"${Box(Label("x"))}"`
print `Box(Label(x))` while `show.say(console, Box(Label("x")))` printed
`Box(<x>)`.

## Resolution

Current RFC-0053 lowering treats every concrete type with a `Show` impl as
render-eligible when `show.render` is linked, including derived `Show` impls.
This lane removed the stale `show_derived` AST marker and the comments that
preserved the old mental model.

The regression test
`rfc0053_derived_show_fields_use_show_in_interpolation_on_both_backends` now
pins the coherent contract:

- direct custom values interpolate through `Show`
- derived wrappers render custom-Show fields through `Show`
- containers of those wrappers recurse consistently
- interpreter and compiled wasm agree byte-for-byte

## Verification

- `cargo test rfc0053_ -- --nocapture`
- `NEXTEST_TEST_THREADS=2 ./scripts/check.sh`
