# BUG-478: `derive(Eq)` misclassified custom `PartialEq` as structural

Severity: HIGH
Status: FIXED
Fixed: 2026-07-06 (`fix/eq-marker-preserves-custom-partialeq`)
Component: `derive(...)`, RFC-0047 equality, trait coherence, interpreter/compiled parity

## Resolution

`derive(Eq)` now generates the implied structural `PartialEq` implementation
only when the type does not already declare `impl PartialEq for T`. If the user
writes a custom `PartialEq`, `derive(Eq)` is treated as the marker trait only:
`TypeDef.partial_eq_derived` remains false, the generated structural
`PartialEq` is skipped, and the whole-program custom-equality set keeps routing
nested equality through the hand-written implementation.

This preserves the previous `derive(Eq)`-alone behavior fixed by BUG-468 while
making this source shape coherent:

```witchy
type Key derive(Eq):
    id: Int
    cache: Int

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        self.id == other.id
```

`Key` is now `Eq`, direct `Key` equality calls the custom `PartialEq`, and
compound equality such as `List(Key)`, `Option(Key)`, tuples, and derived record
fields use the same custom semantics instead of comparing every field
structurally.

## Validation

Regression:
`derive_eq_marker_preserves_custom_partial_eq_at_depth_on_both_backends`

Focused checks:

- `cargo test derive_eq_marker_preserves_custom_partial_eq_at_depth_on_both_backends -- --nocapture`
- `cargo test derive_eq_alone_implies_partial_eq_on_both_backends -- --nocapture`
- `cargo test -p witchy-syntax`
- command-level repro via `cargo run --quiet -- run`
