---
rfc: 0084
title: Scoped extensions and explicit interception
status: deferred
created: 2026-07-12
superseded-by:
tracking: "deferred beyond 0.1; revive only after RFC-0082 dynamic dispatch is implemented and this RFC has explicit acceptance criteria for resolution, authority, parity, and tooling"
---

# RFC-0084: Scoped extensions and explicit interception

## Summary

Add lexically imported extension methods and explicit interceptor stacks. These
provide Ruby-shaped adaptation, delegation, instrumentation, and testing power
without globally reopening types or making method meaning depend on package load
order.

Extensions add statically resolved methods in a scope. Interceptors wrap only
calls made through an explicitly interceptable interface or dynamic boundary.

## Motivation

Ruby's open classes, refinements, delegation, and method interception make
frameworks unusually expressive. Witchy's current coherence model intentionally
binds methods to trait impls or the type's owning module, and
[RFC-0077](0077-testability-without-monkeypatching.md) rejects a
test-only monkeypatch dispatch model.

That rejection should not imply that adaptation and interception are undesirable.
They need one production mechanism whose scope and authority are visible. Common
uses include:

- domain-specific convenience methods over third-party types;
- tracing, metrics, retries, caching, and authorization wrappers;
- test doubles around service interfaces;
- compatibility adapters between package versions;
- request-local framework behavior.

## Design

### Lexical extension methods

```witchy
extension PathDisplay for path.Path:
    fn basename(let self) -> String:
        path.basename(self)

fn report(p: path.Path) -> String using PathDisplay:
    p.basename()
```

An extension is a named declaration. It becomes eligible only through an
explicit `using ExtensionName` on a module, function, or block. It does not add
methods globally and is not re-exported accidentally through an ordinary import.

Resolution order is:

1. inherent/type-owner method;
2. in-scope trait method;
3. explicitly enabled extension method.

Two matching extensions are an ambiguity error. An extension cannot replace an
existing inherent or trait method. Operators, constructors, private members, and
capability intrinsic operations cannot be extended.

### Interceptable interfaces

Interception operates on [RFC-0081](0081-existential-trait-values.md) existential trait values or [RFC-0082](0082-runtime-dynamic-values.md) dynamic
methods, never on arbitrary statically bound free functions:

```witchy
interceptor TraceService for dyn Service:
    fn call(next: fn(Request) -> Result(Response, ServiceError),
            request: Request) -> Result(Response, ServiceError):
        metrics.record("service.call")
        next(request)

let service = intercept(base as dyn Service, [TraceService(metrics)])
```

The result is another `dyn Service`. Interceptors form an explicit ordered list.
Each receives a typed `next` function with the same method contract. Skipping,
repeating, or modifying a call is ordinary visible code in the interceptor.

There is no process-global interceptor registry, wildcard method match, or
implicit inheritance hook.

### Authority

An extension method's authority is its ordinary signature. Enabling an extension
does not grant capabilities. An interceptor's construction arguments carry any
authority it needs, and the existential capability envelope includes that
authority. Adding an authority-bearing interceptor therefore changes the
constructing program's footprint visibly.

Capability intrinsic methods themselves cannot be intercepted or shadowed. A
wrapper may expose a narrower user-defined interface around a capability, but the
host operation remains linked and enforced under its original identity.

### Testing

Production and test code use the same interception model. Tests inject a `dyn`
interface and may wrap or replace it with another implementation. RFC-0077's
test-only construction rules remain useful for sealed data, but no special
monkeypatch dispatch path is added.

### Tooling

The LSP reports the enabled extension responsible for a method, offers an import
or `using` action, and includes the interceptor chain type at construction sites.
Go-to-definition never depends on runtime load order for statically resolved
extension methods.

## Alternatives

- **Global open classes.** Maximally flexible, but package load order can silently
  change unrelated code and invalidate capability and method audits.
- **Ruby refinements exactly.** Lexical scope is attractive and informs this
  design, but Witchy keeps declarations explicit and refuses replacement of an
  existing method.
- **Traits only.** Sufficient for principled polymorphism, but orphan/coherence
  rules make local third-party adaptation cumbersome.
- **Higher-order functions only.** Semantically sufficient for interception, but
  repetitive for multi-method services and provides no standard tooling model.

## Drawbacks

- Adds a third static method source after inherent and trait methods.
- Interceptor chains introduce allocation and indirect calls.
- Extensions can fragment idioms if libraries publish competing vocabularies.
- Preventing replacement gives up some of Ruby's most powerful, and most
  dangerous, techniques.

## Prior art

Ruby refinements and prepend, C# extension methods, Kotlin extensions, Swift
extensions, Rust extension traits, middleware stacks, and aspect-oriented
interceptors inform this design.
