# BUG-532: `derive(Deserialize)` tuple fields leaked generated fallback code

Severity: LOW
Status: FIXED
Fixed: 2026-07-06
Component: `derive(Deserialize)`, generated-source diagnostics

## Problem

`derive(Deserialize)` is documented for scalar fields, lists, options, generic
parameters, and nested record/user types. Tuple fields are outside that 0.1
contract, but the derive previously accepted them through `std/meta`'s generic
fallback and then failed later with a generated `Tuple2.from_json` method error.

That exposed implementation mechanics instead of the derive contract.

## Fix

`derive::expand` now validates `Deserialize` field shapes before emitting any
comptime-generated source. Tuple and function shapes, including when nested
inside type arguments, are rejected with a source-level `derive(Deserialize)`
diagnostic naming the field and unsupported shape.

Named user types and generic parameters continue to flow through the
`Deserialize` trait path.

Regression:

- `derive_deserialize_rejects_tuple_fields_without_generated_fallback_leak`
