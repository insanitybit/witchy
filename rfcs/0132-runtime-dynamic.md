---
rfc: 0132
title: "Dynamic as an explicit owned runtime type boundary"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical Dynamic RFC. RFC-0082's authenticated descriptors, owned payloads, checked decode, field/method/trait access, capability accounting, tooling, and backend parity are implemented. This RFC promotes Dynamic from proposed/internal intent to an explicit core destination while retaining experimental product maturity until an installed workflow and limitation review pass."
predecessors:
  - "[0081](0081-existential-trait-values.md) (authenticated trait witness identity and typed dynamic dispatch)"
  - "[0082](0082-runtime-dynamic-values.md) (implemented Dynamic envelope and checked reflection)"
related:
  - "[0131](0131-reflection-and-comptime.md) (Reflect and compile-time structure)"
  - "[0126](0126-capability-effects-contract.md) (explicit capability arguments and footprints)"
---

# RFC-0132: Dynamic as an explicit owned runtime type boundary

## Decision

`Dynamic` is a retained Witchy feature. It is an explicit boundary for programs
that genuinely need runtime-shaped values or behavior: plugin-like registries,
schema-driven tools, heterogeneous message envelopes, inspectors, and adapters.

Witchy remains statically typed by default. Dynamic behavior is never selected
implicitly for an ordinary field access, method call, conversion, or trait
operation. Entering and leaving `Dynamic` is visible and checked.

## Value model

A dynamic value is semantically:

```text
(authenticated RuntimeType, owned payload)
```

The runtime descriptor records exact linked type identity, public shape,
registered methods, trait witnesses, and required capabilities. It is not a
forgeable type-name string. The payload follows ordinary value semantics: a
dynamic conversion produces an owned envelope rather than a hidden alias to
mutable source storage.

`Dynamic` cannot contain an active source reference or borrowed view. Call
`.owned()` or otherwise materialize the value before crossing the boundary.

## Entry and exit

- `dynamic(value)` requires a reflectable, dynamically representable type.
- `type_of` returns its authenticated descriptor.
- `decode(T)` checks exact compatible identity and returns `Result(T,
  DynamicError)`.
- field inspection exposes only public readable fields.
- method invocation exposes only explicitly registered `@dynamic` methods.
- trait queries use authenticated trait descriptors and witness tables.

Type, arity, visibility, field, method, trait, result, and capability mismatch
are structured `DynamicError` values. They do not become unchecked casts or
backend traps.

## Authority model

Capabilities are not dynamically representable payload data. A dynamically
callable method retains its explicit capability parameters and footprint. Plain
`dynamic.call` cannot discover ambient authority. A capability-requiring call
uses an explicit checked bundle and remains visible to `witchy caps`.

A descriptor cannot expose private or sealed constructors, compiler intrinsics,
unregistered methods, or capability internals.

## Package and linking identity

Runtime identity includes the linked declaration, type arguments, and package
coordinate required to prevent two same-named declarations from becoming
interchangeable accidentally. Serialization of a descriptor is metadata, not
authority and not an automatic promise that the receiving program contains a
compatible type.

Dynamic package boundaries must fail with a checked missing/incompatible type
result. They may not rebind by display name.

## Relationship to other abstraction tools

Use generics for compile-time polymorphism, `dyn Trait` for a known behavioral
interface, `Mirror` for structural inspection, and `Dynamic` only when the
runtime shape or operation set is itself data.

Capability-bounded dynamic code loading is a different feature and remains
deferred under RFC-0085. `Dynamic` transports values and checked method tables;
it does not compile or load arbitrary code.

## Acceptance

1. Descriptor identity is authenticated and stable across direct, imported,
   generic, trait, and package-qualified uses.
2. Dynamic encode/decode, public field reads, registered method calls, trait
   queries, and structured errors agree on interpreter and compiled Wasm.
3. Direct and transitive capabilities, source references, private members,
   sealed constructors, and unregistered operations are rejected.
4. Dynamic calls preserve ordinary ownership, mutation, trap, and cleanup
   behavior.
5. Footprint inspection includes every reachable registered capability-bearing
   method and cannot be narrowed by a runtime string.
6. Formatter, docs, LSP, and generated documentation make dynamic boundaries
   visible.
7. A clean installed example demonstrates a use that is materially clearer
   with `Dynamic` than with a closed tagged union or `dyn Trait`.
8. `PRODUCT-STATUS.md` classifies implemented `Dynamic` as experimental until
   these promotion rows are independently exercised; it no longer calls the
   mechanism merely proposed.
