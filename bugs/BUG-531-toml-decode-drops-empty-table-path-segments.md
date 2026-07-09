# BUG-531: TOML decode drops empty table path segments

Severity: LOW
Status: FIXED
Verified: 2026-07-09 REGRESSION on master 2939429b
Component: `std/toml`, strict decode contract, dotted table paths, package manifests

## Problem

`toml.decode` is documented as the strict/fallible TOML entry point for
structured parsing, but malformed table headers with empty unquoted path
segments were accepted and normalized:

- `[]` became the root table.
- `[a..b]` became `[a.b]`.
- `[a.]` became `[a]`.
- `[.a]` became `[a]`.

That contradicted the module's release-facing split: lenient helpers are visibly
lenient, while `decode` should return `Err` for malformed structure.

## Resolution

`toml.decode` now validates quote-aware table header paths before accepting a
`[table]` or `[[array-table]]` header. Empty unquoted path segments (`[]`,
`[a..b]`, `[a.]`, `[.a]`, and the corresponding array-table form) return
`TomlDecodeError` instead of being silently normalized away.

Quoted literal-dot segments such as `["a..b"]` remain accepted through the
shared BUG-447 quote-aware splitter.

Regression coverage:

- `example_tests::toml_decode_rejects_empty_table_path_segments`

Focused validation:

- `cargo nextest run --workspace -E 'test(/toml_decode/)'`
- `target/debug/witchy fmt --check std/toml.witchy`
