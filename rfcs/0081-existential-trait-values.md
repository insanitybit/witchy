---
rfc: 0081
title: Existential trait values and dynamic dispatch
status: proposed
created: 2026-07-12
superseded-by:
tracking:
---

# RFC-0081: Existential trait values and dynamic dispatch

## Summary

Add `dyn Trait` as an explicit existential value: a value whose concrete type is
hidden but whose statically declared trait operations remain available through
runtime dispatch. This supplies heterogeneous collections and plugin-shaped
APIs without erasing programs into a universal dynamic object model.

The trait declaration determines the value's callable surface and capability
envelope. Dynamic dispatch may hide which implementation runs; it may not hide
which authority that implementation can receive.

## Motivation

RFC-0046 made static trait dispatch consume the real typechecker facts, and
RFC-0050 made functions and methods coherent values. Static monomorphization is
the right default, but it cannot express several ordinary general-purpose
patterns:

- a list of values with different concrete types implementing `Render`;
- application services selected from configuration;
- callbacks returned across package boundaries;
- UI component trees with implementation-private state;
- dynamically loaded implementations constrained to one interface.

Encoding every such case as a closed enum centralizes knowledge of every
implementation. Erasing all values to RFC-0082's `Dynamic` gives up useful static
contracts. Existential trait values are the middle layer.

## Design

### Surface

```witchy
trait Render:
    fn render(let self, context: Context) -> Result(String, RenderError)

fn page(parts: List(dyn Render), context: Context) -> Result(String, RenderError):
    parts.map(fn(part): part.render(context)).collect()

let widget: dyn Render = StatusWidget(status)
```

`dyn Render` means "some concrete type implementing `Render`". Conversion from
`T` to `dyn Render` is implicit only where `dyn Render` is the expected type;
otherwise `x as dyn Render` is explicit. Downcasting is not part of this RFC.

`dyn Trait(A, ...)` carries instantiated trait arguments. Associated types, when
added, must be fixed in the existential type before construction.

### Object-safe trait subset

A trait is existential-safe when each callable method:

- has a receiver;
- does not introduce method-local type parameters;
- does not return bare `Self`;
- mentions `Self` only in the receiver unless another occurrence is boxed behind
  the same existential type;
- has a fully determined capability requirement.

The checker reports which method prevents `dyn Trait` construction. Static use
of the same trait remains legal.

### Representation

The semantic representation is `(payload, witness)`. The compiled backend may
use an owned boxed payload plus an immutable witness-table index. Each witness
entry points to a monomorphic adapter generated from an existing impl. The
interpreter stores the same concrete value and resolved impl identity.

Copying a `dyn Trait` follows normal witchy value semantics. The payload is
deep-copied unless ownership, `frozen`, or RC facts permit sharing. A future
borrowed existential composes with RFC-0083 as `let dyn Trait<'a>`; this RFC
ships owned existential values first.

### Authority envelope

The callable authority of `dyn Trait` is the union of capabilities present in
the trait method signatures after type substitution. An impl may call private
helpers, but those helpers cannot acquire authority absent from method inputs or
the existential payload.

Capabilities cannot be captured invisibly inside an existential payload. A type
that transitively contains authority may be converted only when that authority
is represented in the existential type's declared capability envelope. Until
RFC-0005 supports all capability-carrying aggregates, such construction remains
a loud compile error.

Footprint analysis follows the trait surface plus the construction sites of
reachable witnesses. Package interfaces therefore remain auditable without
enumerating runtime control flow.

### Coherence and sealing

Existing impl coherence applies. Witnesses are created only from linked impls;
there is no runtime method-table mutation. A sealed trait may restrict impls to
its home module, enabling closed but dynamically dispatched interfaces.

### Equality, reflection, and serialization

`dyn Trait` has no automatic `Eq`, ordering, reflection, or serialization.
Those operations are available only when the existential trait includes the
corresponding operation, such as `dyn Render + Eq`. Two existential values never
compare by payload address or witness identity as an accidental fallback.

## Alternatives

- **Closed enums.** Preferable when the implementation set is genuinely closed;
  insufficient for package extension points.
- **Monomorphization only.** Faster and simpler, but cannot represent runtime
  heterogeneity.
- **Universal `Dynamic`.** More flexible but loses the compile-time method and
  error contract. RFC-0082 builds on, rather than replaces, this typed layer.
- **Function records.** Expressive in the current language, but repeat the
  receiver plumbing, erase trait coherence, and provide no standard reflection
  or capability envelope.

## Drawbacks

- Adds an indirect-call and boxed-payload representation.
- Object-safety rules add another trait concept users must learn.
- Capability-bearing existential values complicate RFC-0005 and WIR lowering.
- Cross-package ABI stability needs a witness-layout version or adapter layer.

## Prior art

Rust trait objects, Swift existentials, Haskell existential packages, OCaml
first-class modules, and C++ virtual dispatch inform this design.
