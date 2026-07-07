# BUG-434: Comptime-emitted code misses frontend lowerings

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Component: `comptime`, frontend pass order, generated-source normalization

## Summary

`comptime:` expansion now has an explicit post-expansion normalization boundary.
After each block emits source, the generated module fragment is merged into the
enclosing module and the expanded module is run through the same pre-typecheck
frontend lowerings as handwritten source:

- generator lowering;
- async lowering;
- derive expansion;
- named-field record construction lowering.

The expander now consumes `comptime:` blocks sequentially instead of snapshotting
all blocks up front. That lets derive-generated `comptime:` blocks created by
post-expansion normalization run in the same bounded loop. Expansion is capped
at 256 blocks to keep recursive source generation from running forever.

## Regression Coverage

- `emitted_named_field_construction_is_lowered`
- `emitted_derive_blocks_are_expanded`
- `emitted_generators_are_lowered`
- `emitted_async_functions_are_lowered`

The same test group also keeps the related `module_types` and emitted
`from_import` regressions green.

## Validation

- `cargo test -p witchy-interp 'comptime::tests::' -- --nocapture`
