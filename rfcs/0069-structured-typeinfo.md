---
rfc: 0069
title: "Structured TypeInfo for compile-time reflection"
status: implemented
created: 2026-07-06
tracking: "RFC-0067 structured comptime facts"
---

# RFC-0069: Structured TypeInfo for compile-time reflection

The reflection builder is implemented in
[`crates/witchy-syntax/src/reflect.rs`](../crates/witchy-syntax/src/reflect.rs),
with cross-backend generator coverage in [`src/example_tests.rs`](../src/example_tests.rs).

## Summary

`std/meta.TypeInfo` exposes declaration shape through `TypeKind` and declared
types through `TypeExpr`. There is no parallel string representation for
generators to accidentally treat as semantic data.

## Motivation

RFC-0067 identifies rendered type strings as the wrong long-term model for
compile-time facts. They are useful when a generator has to emit source, but they
are a brittle place to make semantic decisions: prefix checks cannot distinguish
top-level structure from spelling accidents, nested parsing is ad hoc, and every
derive would otherwise grow its own tiny parser.

The compile-time reflection model should mirror the rest of Witchy's coherence
story: structure is data, rendering is only the boundary where source text is
emitted.

## Design

`std/meta` defines:

```witchy
type TypeKind:
    TypeRecord
    TypeSum
    TypeUninhabited

type TypeExpr:
    TNamed(String, List(TypeExpr))
    TTuple(List(TypeExpr))
    TFn(List(TypeExpr), TypeExpr)
    TQualified(String, TypeExpr)
```

`TypeInfo.kind` is a `TypeKind`; `FieldInfo.type_expr` and
`VariantInfo.field_type_exprs` are the only declared-type representations.
`TypeUninhabited` names the fieldless declaration shape accurately: it has no
constructor and no values, so calling it "unit" would be false.

`meta.type_source(ty)` renders a `TypeExpr` back to source text for the explicit
case where a generator needs to emit a type name. That function is a boundary,
not the semantic representation.

## As Built

- `crates/witchy-syntax/src/reflect.rs` constructs `TypeKind` and `TypeExpr`
  values directly for `module_types` and derive inputs.
- TypeInfo construction normalizes type aliases in field types and infers omitted
  generic parameters from those fields before either `module_types` or built-in
  derives see the shape. Compile-time reflection is therefore the same type fact
  model the checker later uses, not the raw parsed spelling.
- Constructor expressions preserve those normalized generic arguments when they
  feed generated helpers. A value such as `Box(3)` is treated as `Box<Int>` for
  `show.render(Box(3))` and `json.stringify(Box(3))`, matching the direct
  receiver path (`Box(3).reflect()`).
- `std/meta.derive_reflect` dispatches `List`/`Option` handling from
  `TypeExpr`, not `string.starts_with`.
- `std/meta.derive_deserialize` recursively decodes `List` and `Option` from
  `TypeExpr`, not prefix-stripped strings.
- Fieldless declarations such as `type Marker:` are reported as
  `TypeUninhabited`:
  they are uninhabited types with no constructors, not singleton empty records.
  Built-in structural derives reject them at the source-shape gate instead of
  generating vacuous record implementations.
- Custom derives branch on `TypeKind`/`TypeExpr`, the same representation the
  built-in derives consume.

## Compatibility

This is part of the pre-0.1 language contract, so Witchy takes the clean break
before publication instead of carrying duplicate legacy fields into the first
release. `meta.type_source` remains the explicit conversion for generated source.
