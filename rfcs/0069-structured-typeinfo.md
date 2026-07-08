---
rfc: 0069
title: "Structured TypeInfo for compile-time reflection"
status: implemented
created: 2026-07-06
tracking: "RFC-0067 structured comptime facts"
---

# RFC-0069: Structured TypeInfo for compile-time reflection

## Summary

`std/meta.TypeInfo` now exposes declared types as structured data through
`TypeExpr`, alongside the older rendered type-name strings.

The string fields remain as compatibility and source-emission conveniences, but
standard derives no longer decide semantics by parsing strings such as
`"List(Option(Int))"`.

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

`std/meta` adds:

```witchy
type TypeExpr:
    TNamed(String, List(TypeExpr))
    TTuple(List(TypeExpr))
    TFn(List(TypeExpr), TypeExpr)
    TQualified(String, TypeExpr)
```

`FieldInfo` keeps `type_name: String` and adds `type_expr: TypeExpr`.
`VariantInfo` keeps `field_types: List(String)` and adds
`field_type_exprs: List(TypeExpr)`.

`meta.type_source(ty)` renders a `TypeExpr` back to source text for the explicit
case where a generator needs to emit a type name. That function is a boundary,
not the semantic representation.

## As Built

- `crates/witchy-syntax/src/reflect.rs` constructs both the legacy strings and
  the structured `TypeExpr` tree for `module_types` and derive inputs.
- TypeInfo construction normalizes type aliases in field types and infers omitted
  generic parameters from those fields before either `module_types` or built-in
  derives see the shape. Compile-time reflection is therefore the same type fact
  model the checker later uses, not the raw parsed spelling.
- `std/meta.derive_reflect` dispatches `List`/`Option` handling from
  `TypeExpr`, not `string.starts_with`.
- `std/meta.derive_deserialize` recursively decodes `List` and `Option` from
  `TypeExpr`, not prefix-stripped strings.
- Fieldless declarations such as `type Marker:` are reported as `kind: "unit"`:
  they are uninhabited types with no constructors, not singleton empty records.
  Built-in structural derives reject them at the source-shape gate instead of
  generating vacuous record implementations.
- Existing custom derives that read `FieldInfo.type_name` or
  `VariantInfo.field_types` continue to work.

## Deferred

The compatibility string fields are still public for 0.1. Removing them would be
a breaking cleanup for a later release after user-defined derives have had time
to migrate to `TypeExpr`.
