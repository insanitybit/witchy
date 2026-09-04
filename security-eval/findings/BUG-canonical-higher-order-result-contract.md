# BUG: canonical higher-order result contracts fail closed

## Summary

Standalone higher-order functions in the canonical SafeWIR pipeline reject valid
source because a checked callable parameter has no local concrete target body.
The lowering represents the callable result as `ResultContract::BodyDerived`,
but that marker cannot provide an authenticated result contract when the target
body is supplied modularly through a callable value. This is a compiler
coverage/contract bug: the compiler rejects a valid program rather than emitting
Wasm through an unauthenticated contract.

## Reproducer

The existing reproducer is:

`examples/list_ops/src/list_ops.witchy`

It uses a higher-order `map` function whose callable parameter is invoked to
produce a `String` result that is pushed into a list. Running the production
canonical SafeWIR check pipeline on that source currently fails with:

```text
InitialValidation(UnsupportedIndirectCall { function: FunctionId(1) })
```

The failure occurs during initial validation, before Wasm encoding or
admission. The standalone higher-order function itself is valid; its callable
parameter is checked, but its concrete target is not a local function body in
the module being validated.

## Root cause

The indirect-call path requires a callable signature and then attempts to
resolve memory-bearing result information from the target body. For this
reproducer, signature `0` has no local function body: it describes the checked
callable parameter, not a concrete locally defined callee. Consequently,
`ResultContract::BodyDerived` has no body from which to derive the result
contract, and validation rejects the call as `UnsupportedIndirectCall`.

This is a missing modular contract boundary, not evidence that the result is
scalar or that it is shared. A callable result can be a fresh object, an alias,
static data, or another explicitly checked result form, and the caller must
receive the corresponding authenticated contract.

## Security constraint

The fix must preserve fail-closed behavior. In particular, it must not:

- infer `Shared` or refcount authority from the callable name, signature shape,
  carrier type, or absence of a local body;
- grant liveness, ownership, aliasing, or reclamation authority merely because
  a result is represented as a reference;
- restore a name-keyed contract table or any equivalent guessed authority;
- bypass canonical SafeWIR validation or fall back to the raw encoder.

An unresolved callable result contract must remain a compiler error.

## Required fix direction

Introduce a first-class checked value-result contract for modular callable
values. The contract must state only the authority actually established for the
returned value, including its layout and read/liveness guarantees; it must not
silently confer uniqueness, sharing, refcount, or reclamation authority.

For local bodies, the checker must derive the result contract and verify body
conformance against the callable declaration. For standalone or indirect
callable values, the contract must instead come from an authenticated checked
module/linker or callable-table entry. The linker/table must authenticate the
entry's identity, signature, result form, layout, extent, ownership, aliasing,
and effects before the caller can use it.

Only after that checked value-result contract is available may canonical
SafeWIR lower and validate the indirect call. The production path must remain:

```text
checked source -> canonical SafeWIR -> validation -> mechanical Wasm encoding
```

No source workaround or permissive contract inference is part of the fix.

## Status

This report documents one genuine compiler bug. No compiler source was changed
for this report, and no broad tests were run.
