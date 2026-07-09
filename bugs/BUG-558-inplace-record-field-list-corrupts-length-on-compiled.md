# BUG-558: In-place record-field container mutation diverged on compiled backend

Status: FIXED
Severity: HIGH
Component: `witchy-lower` in-place record-field mutation, parity

## Summary

An ignored local bug note reported that mutating a `List` or `Dict` stored inside
a `var` record through a helper function could corrupt the compiled backend's
container length/header while the interpreter remained correct. The minimal
cases accumulated into a record field in a loop, then read the field twice via
interpolation.

This mattered because RFC-0050's stdlib method conversion wants `var self`
container methods; shipping that conversion over a record-field in-place
miscompile would spread the failure mode across common container code.

## Verification

Current master no longer reproduces either recorded divergence. On
`caa7187e`, both the List-field repro and the Dict-field repro agree under:

```sh
CARGO_TARGET_DIR=target-codex-bug558 cargo run --quiet -- parity "$tmpdir/main.witchy"
```

Observed result for each repro:

```text
interpreter and compiled WASM agree (2 line(s) of output)
parity-stats outcome=agree compared=2
```

## Resolution

Closed as stale/fixed by prior compiler work. BUG-558 is no longer a blocker
for the RFC-0050 container method-symmetry cut.
