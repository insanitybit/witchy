---
rfc: 0081
title: Existential trait values and dynamic dispatch
status: proposed
created: 2026-07-12
superseded-by:
tracking: "slice 1 (type identity and safety) implemented: dyn Trait(args…) and dyn module.Trait(args…) parse/format in every type position; non-ambient trait declarations, impl heads, bounds, supertraits, and existential heads resolve to one module-qualified declaration identity; same-spelled imported traits require qualification; aliases and imports preserve identity; the six existential-safety rules and transitive capability-payload rejection are enforced; and every dyn-mentioning program fails with one feature-stage diagnostic before either backend lowers (no construction/dispatch yet). Reflection metadata uses a placeholder opaque meta.TNamed head until the witness slice adds meta.TDyn. Slices 2-5 (witness substrate, owned values and dispatch, receiver completion, authority/tooling closure) remain unimplemented"
related:
  - "0005 (capability-safe aggregate representation and typed callable ABI)"
  - "0038 (grantable capabilities and transitive authority checks)"
  - "0046 (typed trait dispatch and impl coherence)"
  - "0050 (one callable model and type-owned methods)"
  - "0069 (structured type identity; no existential downcast in this RFC)"
  - "0083 (borrowed views; owned existentials only in this RFC)"
  - "0087 (uniform var write-back through direct and indirect calls)"
  - "0090 (typed indirect-call signatures and proper tail calls)"
---

# RFC-0081: Existential trait values and dynamic dispatch

## Summary

Add `dyn Trait` as an explicit existential value: a value whose concrete type is
hidden while the statically declared operations of one trait remain available
through runtime dispatch.

`dyn Trait` is the typed middle layer between monomorphized generics and
RFC-0082's future `Dynamic`. It supports heterogeneous collections and
linked-package extension points without introducing global method mutation,
load-order semantics, implicit downcasts, or a universal object model.

This RFC ships **owned** existential values. Borrowed existentials and binary
plugin ABI stability are separate designs.

## Surface

```witchy
trait Render:
    fn render(let self, context: Context) -> Result(String, RenderError)

fn page(parts: List(dyn Render), context: Context)
        -> Result(String, RenderError):
    var out = ""
    for part in parts:
        out = out + part.render(context)?
    Ok(out)

let widget: dyn Render = StatusWidget(status)
let explicit = StatusWidget(status) as dyn Render
```

`dyn` is contextual only in type position. `dyn Render(a, ...)` instantiates a
generic trait with concrete type arguments. The trait's transitive supertraits
are part of the callable surface; v1 has no separate `dyn A + B` intersection
syntax.

A concrete `T` erases implicitly only where an expected type is already
`dyn Trait`: an annotation, argument, return/tail position, typed container
element, or other directed-coercion site. Otherwise erasure is explicit with
`as dyn Trait`. Inference never silently replaces a concrete type with an
existential.

`dyn Sub` may coerce to an expected `dyn Super` when `Super` is a transitive
supertrait of `Sub`. Unrelated existential types never coerce.

## Existential-safe traits

The checker decides existential safety when a `dyn Trait` type is used. Static
generic use and ordinary impls remain legal even when the trait is not
existential-safe.

A callable method is existential-safe when all of the following hold:

1. It has a receiver.
2. Every trait type parameter is fixed by `dyn Trait(args...)`.
3. It introduces no method-local type parameter.
4. It does not return bare `Self`.
5. `Self` appears nowhere except the receiver type.
6. Any borrowed result has a lifetime relation that can be expressed without
   exposing the hidden concrete payload. V1 rejects a result borrowed from
   `self`; borrowed existentials require a follow-up composition with RFC-0083.
7. Every capability requirement is visible in the method signature after type
   substitution.

Receiver conventions are not split into a special object subset:

- bare `self` uses the ordinary immutable value-receiver contract;
- explicit `let self` borrows the payload for the call, but cannot return a view
  tied to that hidden receiver in v1;
- `var self` uses RFC-0087 move-in/move-out. The witness adapter returns the
  updated hidden payload and the declared result in one write-back envelope.
- `own self` consumes the existential value and does not reconstruct it unless
  the declared return type explicitly contains another existential value.

A receiver-less associated function is callable through its type as today, not
through a value whose concrete type is hidden. One unsafe method makes the trait
unusable as `dyn Trait`; the diagnostic names every blocking method and rule.

`PartialEq` is intentionally not existential-safe: its second `Self` parameter
violates rule 5. There is no accidental `dyn Render + Eq` escape hatch. A trait
that needs dynamic equality must declare an object-safe operation with explicit
semantics, such as a stable key or domain-specific comparison.

## Type identity and coercion

The checker gains a first-class existential type identity containing:

- the resolved trait declaration identity;
- fully substituted trait arguments;
- the transitive, ordered method-slot surface.

It is not represented as a guessed type name or as `Dynamic`. Aliases normalize
to this identity through the existing structural type-resolution path. Two
modules referring to the same resolved trait instantiation get the same
existential type.

Non-ambient trait declarations receive the same module-qualified identity as
nominal types. Resolution rewrites trait declarations, supertraits, impl heads,
generic bounds, and `dyn` heads before modules merge. A bare trait name from one
import resolves to that declaration; the same spelling from multiple imports is
an ambiguity error, and `dyn module.Trait` selects explicitly. Two modules may
therefore declare unrelated traits with the same source spelling without
collapsing their existential identities. The ambient comparison traits and
prelude `Show` retain their bare language identities.

Construction requires a coherent linked `impl Trait(args...) for T`. The
conversion records that exact impl as a witness. No runtime lookup by string,
reflection name, source order, or load order is permitted.

## Representation and ABI

The semantic representation is an owned pair:

```text
(payload_box, witness_id)
```

The interpreter stores the concrete `Value` plus a resolved witness identity.
The compiled backend uses a Wasm-GC existential record containing a generic
`structref` payload box and an integer witness-table index.

Every concrete payload is boxed in a generated GC struct whose field kinds are
the concrete value's real WIR kinds. Scalars, GC references, and multi-slot
values are never squeezed through an `i64` slot. This reuses RFC-0005's typed
reference discipline and makes each reference-kind crossing visible to WIR
validation.

Each linked `(existential type, concrete impl)` pair generates one immutable
witness table. Slots follow trait declaration order after deterministic
supertrait linearization. Each slot points to a monomorphic adapter with a
canonical signature:

```text
(payload_box, explicit method arguments...) -> method result envelope
```

The adapter casts/unboxes the concrete payload, calls the already-lowered impl
method, and boxes any `var self` write-back. The signature carries all parameter
conventions and multi-result kinds; RFC-0087 write-back and RFC-0090 indirect
tail-call classification therefore use the existing typed envelope instead of a
second dynamic ABI.

Witness IDs and table layouts are deterministic compiler-internal identities for
one closed linked program. They are not a stable binary plugin ABI. Packages may
contribute impls because they are linked before witness construction; loading a
new witness into a running process is out of scope.

Copying an owned existential follows ordinary Witchy value semantics. The
payload is copied unless ownership, frozen sharing, or uniqueness analysis
proves reuse. Constructing an owned existential may allocate its payload box.
That allocation is part of the explicit `dyn` cost model, not a hidden claim of
zero-cost abstraction. Devirtualization is an optional general optimization,
never a semantic requirement or per-trait fast path.

## Dispatch

Method lookup on `dyn Trait` is fully static up to one witness slot: the checker
resolves the trait method and slot, while runtime selects only the concrete
adapter. Missing methods, ambiguous supertrait methods, arity errors, and
convention errors remain check-time diagnostics.

There is no fallback from a failed existential dispatch to reflection, a method
name string, or RFC-0082 `Dynamic`. A malformed witness or slot is an internal
compiler error and must fail loudly in both backends.

The witness table is immutable. Existing impl coherence and trait sealing rules
apply unchanged. A sealed trait may restrict witness construction to impls in
its home module, producing a closed dynamically dispatched interface.

## Authority and footprint

V1 rejects conversion when the concrete payload transitively contains a
capability. There is no capability-envelope syntax in this RFC, so accepting such
a payload would hide authority behind `dyn Trait`. A later RFC may add an
explicit envelope; until then the rule is reject-first on both direct and nested
authority.

Methods may receive capabilities explicitly in their signatures. Dynamic
dispatch does not grant authority: an adapter can call private helpers, but those
helpers cannot mint capabilities absent from the method inputs or ordinary
root-grant flow.

Footprint analysis records the trait surface and the union of reachable witness
adapters at linked construction sites. The analysis is conservative and
closed-world. Adding a new reachable existential construction may widen a build
footprint, and the ordinary build-footprint comparison reports that change.

## Operations deliberately absent

An existential value has no automatic equality, ordering, hashing, reflection,
serialization, type-name inspection, or downcast. Address equality and witness
identity are never observable fallbacks.

Those operations require an existential-safe trait method with explicit domain
semantics. General runtime type tests and checked downcasts belong to RFC-0082,
built on stable type information after this typed layer exists.

V1 also excludes:

- borrowed existential values;
- returned views tied to the hidden receiver;
- trait intersections outside declared supertraits;
- runtime witness registration or monkey-patching;
- dynamically loaded binary plugins;
- a stable cross-compiler witness-table ABI.

## Evaluation order and traps

Existential construction evaluates the concrete value once, then allocates and
stores its payload before producing the witness pair. A failed allocation traps
without producing a partial existential value.

A dynamic call evaluates receiver and arguments in the same left-to-right order
as an ordinary method call. `var` write-backs commit together on structured
return and do not commit on a trap, exactly as RFC-0087 specifies. Dynamic
dispatch adds no transaction or rollback semantics.

## Implementation plan

Implementation proceeds as small vertical slices, but the RFC remains proposed
until the whole public contract is complete.

1. **Type identity and safety.** Add contextual `dyn` parsing, resolved
   existential type identity, object-safety diagnostics, coherent concrete-to-dyn
   coercions, and explicit capability-payload rejection.
2. **Witness substrate.** Add deterministic method-slot linearization and typed
   witness adapter metadata shared by interpreter and WIR lowering. Internal
   structural tests pin scalar, GC-reference, multi-slot, and convention-bearing
   signatures before source programs depend on them.
3. **Owned values and dispatch.** Implement interpreter and Wasm-GC payload boxes,
   witness construction, `let` receiver dispatch, heterogeneous containers, and
   supertrait upcasts with differential tests.
4. **Receiver completion.** Implement `var` write-back and `own` consumption
   through the same indirect multi-result ABI, including explicit return, `?`,
   trap, aliasing, and proper-tail-call cases.
5. **Authority and tooling closure.** Integrate footprint analysis, formatter,
   `witchy check` diagnostics, `witchy expand`/LSP type display, spec, book, and
   migration coverage. Prove both backends agree and run the full release gate.

No slice may expose source syntax that silently falls back to a guessed dynamic
representation. Before the complete runtime exists, parsed `dyn` programs must
fail with a precise feature-stage diagnostic rather than reach one backend only.

## Acceptance criteria

RFC-0081 is implemented only when all of the following are true:

1. `dyn Trait(args...)` and `dyn module.Trait(args...)` parse and format in every
   type position. Identity is stable across aliases and imports; distinct
   same-spelled declarations in different modules do not unify, and ambiguous
   bare references fail with module-qualified alternatives.
2. Concrete-to-dyn coercion occurs only at specified expected-type sites or an
   explicit `as dyn Trait`; inference does not erase concrete types implicitly.
3. Existential-safety diagnostics cover receiver-less methods, unresolved type
   parameters, method generics, every forbidden `Self` position, bare `Self`
   returns, and receiver-borrowed results.
4. A transitive capability in a concrete payload is rejected before lowering,
   including capabilities nested in records, sums, tuples, and containers.
5. Witness ordering is deterministic under module/import order changes and
   includes transitive supertraits without duplicate or ambiguous slots.
6. Interpreter and Wasm use the same witness identity and dispatch result for at
   least two concrete implementations in one `List(dyn Trait)`.
7. Compiled payload boxes preserve scalar, GC-reference, and multi-slot kinds;
   WIR/source guards prove no reference is packed through an `i64` slot.
8. Bare value, explicit `let` borrow, `var`, and `own` receiver methods agree
   across direct calls, interpreter dynamic calls, and compiled indirect calls.
9. `var self` preserves RFC-0087 all-at-once write-back on tail return, explicit
   return, and `?`; traps expose no partial caller write-back.
10. Supertrait method calls and `dyn Sub` to `dyn Super` upcasts work on both
    backends; unrelated upcasts fail at check time.
11. Existential values have no accidental equality, ordering, hashing,
    reflection, serialization, type-name, address, witness, or downcast surface.
12. Footprint analysis includes every reachable witness adapter and widens
    deterministically when a reachable construction adds authority-using code.
13. Normal and `mode opt` programs report the same values and traps. Opt mode
    makes no unsupported no-allocation or devirtualization promise.
14. Formatter round trips, diagnostics name the trait/method/concrete type, and
    the spec/book include executable heterogeneous-list and `var self` examples.
15. Focused adversarial tests, both backend suites, Wasm/browser build, and the
    serialized full release gate are green in the implementing change set.

## Alternatives

- **Closed enums.** Preferable when the implementation set is genuinely closed;
  insufficient for linked-package extension points.
- **Monomorphization only.** The default and fastest path, but unable to store
  runtime heterogeneity.
- **Universal `Dynamic`.** More flexible but loses the compile-time callable and
  authority contract. RFC-0082 builds on, rather than replaces, this layer.
- **Function records.** Expressive but repeat receiver plumbing, erase trait
  coherence, and do not integrate with convention-bearing method ABI or sealing.
- **Capability-bearing payloads with an implicit envelope.** Rejected because the
  type would hide authority. V1 rejects; a future explicit surface may amend it.
- **Stable binary witness ABI now.** Rejected as premature. Witchy packages are
  linked closed-world today; runtime loading requires a separate versioning and
  trust design.

## Drawbacks

- Owned existential construction may allocate and dynamic calls are indirect.
- Object-safety rules add a trait concept and intentionally exclude common
  `Self`-binary protocols such as `PartialEq`.
- Rejecting capability-bearing payloads limits service-object patterns until an
  explicit authority envelope is designed.
- Borrowed existentials are required for the lowest-overhead Rust-like usage and
  remain follow-up work with RFC-0083.

## Prior art

Rust trait objects, Swift existentials, Haskell existential packages, OCaml
first-class modules, and C++ virtual dispatch inform this design. Witchy's
differences are the explicit capability rejection, convention-bearing `var`
write-back ABI, closed-world footprint accounting, and parity requirement across
the interpreter and Wasm-GC backend.
