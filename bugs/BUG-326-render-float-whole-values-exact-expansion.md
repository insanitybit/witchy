# BUG-326: whole-valued Float rendering used exact fixed-point expansion

Severity: LOW
Status: FIXED
Verified: 2026-07-08 fixed on `worktree-wt-38701-1783510646`
Component: `crates/witchy-syntax/src/fmt.rs`, rendering, RFC-0053

## Problem

`render_float` kept `Float`/`Int` visually distinct by formatting finite
whole-valued floats with `"{x:.1}"`. That path can print Rust's exact fixed-point
decimal expansion for large magnitudes, producing long strings that contradict
the public "shortest-round-trip everywhere" rendering contract.

Examples:

- `1234567890123456789.0` rendered as a long fixed decimal ending in `.0`.
- `1e308` rendered as a 309-digit fixed decimal plus `.0`.

Both backends agreed because they shared the function, but the agreed spelling
was not the intended canonical spelling.

## Fix

`render_float` now uses `ryu` shortest-round-trip formatting for finite floats
and appends `.0` only when the shortest spelling has no decimal point or
exponent marker. That preserves the visible Float marker for `3.0` while allowing
large whole values to render as compact exponent forms.

Regression:

- `whole_floats_keep_shortest_round_trip_digits_with_float_suffix`
- `whole_float_rendering_uses_shortest_round_trip_on_both_backends`
