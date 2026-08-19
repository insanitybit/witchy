---
rfc: 0131
title: "Static reflection and capability-free compile-time metaprogramming"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical reflection/comptime RFC. Reflect, structured TypeInfo, typed quotation, hygiene, provenance, tagged literals, expansion tooling, and backend parity are implemented. Promotion requires a curated installed generator workflow and an explicit compatibility policy for raw emit/source-backed builders."
predecessors:
  - "[0006](0006-compile-time-tagged-literals.md) (tagged literals)"
  - "[0053](0053-one-rendering.md), [0065](0065-sealed-type-constructors.md) (derived protocols and invariant boundaries)"
  - "[0069](0069-structured-typeinfo.md) (structured compile-time type reflection)"
  - "[0080](0080-structured-hygienic-metaprogramming.md) (typed syntax, hygiene, provenance, and tooling)"
related:
  - "[0132](0132-runtime-dynamic.md) (owned runtime type envelopes)"
  - "[0133](0133-standard-library-contract.md) (Reflect, Show, Eq, and Deserialize protocols)"
---

# RFC-0131: Static reflection and capability-free compile-time metaprogramming

## Decision

Witchy retains two explicit kinds of structure inspection:

- runtime static reflection uses the `Reflect` trait and `Mirror` data; and
- compile-time reflection uses structured `meta.TypeInfo` and compiler-owned
  syntax values.

Compile-time code is ordinary Witchy evaluated with zero ambient capabilities.
It may add typed declarations and expressions before normal checking. It cannot
rewrite existing declarations, access the filesystem or network, or bypass the
post-expansion type and capability analysis.

## Runtime static reflection

`derive(Reflect)` opts a nominal type into structural inspection. Scalars,
standard containers, tuples, and structural records have deliberate built-in or
generic implementations. `reflect(value)` returns `Mirror`, an ordinary tagged
union that preserves record names, field order, variant identity, and payloads.

Serialization, debug rendering, hashing, schema inspection, and similar generic
consumers build on this protocol. Encoding may be reflective; typed decoding is
separate and uses `Deserialize` or another explicit constructor because
constructing an invariant-bearing type is not structural inspection.

Static reflection does not imply runtime dynamic dispatch. That boundary belongs
to RFC-0132.

## Compile-time evaluation

A top-level `comptime:` block and a `comptime fn` execute in the deterministic
compile-time evaluator. They receive no root capability parameters. Expansion
has bounded steps, recursion, output size, and item count.

Generated items are appended before linking, type checking, trait resolution,
capability-footprint analysis, and backend lowering. Generated code therefore
obeys the same rules as handwritten code.

## Typed syntax and hygiene

The canonical generation API uses compiler-owned values:

- `ExprSyntax`, `PatternSyntax`, `TypeSyntax`, `StmtSyntax`, `BlockSyntax`,
  `ItemSyntax`, and `ModuleSyntax`;
- category-checked builders and `quote` forms;
- typed holes that substitute AST nodes rather than concatenating source;
- compiler-fresh binding identities; and
- persistent definition, invocation, and hole provenance for diagnostics and
  editor navigation.

Definition-site names remain definition-site names. A generator uses an
explicit `meta.call_site` reference when it intentionally requests consumer
scope. User source cannot forge compiler-fresh identities.

Raw `emit(String)` and source-backed compatibility builders remain a migration
surface. They parse only at their explicit boundary, retain provenance, and
cannot silently become the foundation for new generators. Promotion requires a
documented compatibility or retirement policy for them.

## Tagged literals and derives

Tagged literals are compile-time functions that receive static string fragments
and opaque expression holes and return typed expression syntax. Their expansion
has the same capability, hygiene, import, and provenance rules as other
compile-time generation.

Built-in and user-defined derives are compile-time item generators. They may
inspect structured type information and emit protocol implementations, but they
cannot bypass sealed constructors, visibility, capability accounting, or normal
type checking.

## Tooling contract

`witchy expand` renders stable canonical expanded source for inspection. The
formatter preserves source quotation. Diagnostics identify both generated code
and its invocation/definition ancestry. The language server indexes generated
declarations and offers navigation without exposing compiler-private names as
ordinary source identifiers.

## Acceptance

1. `Reflect` and `Mirror` cover the documented scalar, container, tuple, record,
   union, and generic shapes on both backends.
2. `TypeInfo` exposes normalized structured types, conventions, qualifiers,
   lifetimes, fields, and variants without semantic string parsing.
3. Every quotation category and typed hole retains AST identity and provenance
   through expansion.
4. Generated code is type-checked and footprint-analyzed exactly once through
   the ordinary pipeline.
5. Compile-time capability access, syntax-value runtime escape, capture, and
   forged identities fail closed.
6. Expansion is deterministic and bounded, with cache and diagnostic evidence.
7. A clean installed example implements one useful typed generator and can be
   understood through `witchy expand`.
8. Product documentation distinguishes static reflection, compile-time
   generation, and runtime `Dynamic`.
