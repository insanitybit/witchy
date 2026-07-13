---
rfc: 0082
title: Runtime Dynamic values and checked reflection
status: proposed
created: 2026-07-12
superseded-by:
tracking:
---

# RFC-0082: Runtime `Dynamic` values and checked reflection

## Summary

Add an explicit `Dynamic` boundary for programs that need runtime-shaped data
and behavior. A dynamic value carries a runtime type descriptor and supports
checked conversion, field access, method lookup, and invocation. Every operation
returns a structured error; ordinary witchy values and calls remain statically
typed by default.

`Dynamic` is flexibility with a visible boundary, not gradual weakening of every
type in the language.

## Motivation

Configuration-driven systems, inspectors, serializers, plugin hosts, data
explorers, interactive tools, and framework DSLs sometimes cannot know every
shape statically. Today they must encode a bespoke tagged tree such as `Json`,
write a closed union, or generate a static adapter for each type.

Those remain preferable when the domain is known. They are poor substitutes for
genuine runtime inspection and invocation. Ruby and Python are productive partly
because libraries can discover and adapt to values. Witchy should offer that
power without making misspelled methods, authority, and failure implicit.

## Design

### Boundary and conversion

```witchy
let value: Dynamic = user.dynamic()

let name: String = value.field("name")?.decode()?
let rendered: String = value.call("render", [context.dynamic()])?.decode()?

let maybe_user: Option(User) = value.try_decode()
match maybe_user:
    Some(user) -> use_user(user)
    None -> ignore()
```

Converting `T` to `Dynamic` requires `T: Reflect`. Converting back requires an
exact compatible runtime descriptor and returns `Result(T, DynamicError)` or
`Option(T)` for `try_decode`. The result type is inferred from the expected type,
using witchy's existing generic-call inference; a missing expected type is an
ambiguity error rather than an unchecked cast. There is no implicit propagation
of `Dynamic` into a statically typed operation.

### Runtime descriptors

RFC-0069's `TypeInfo` remains the compile-time declaration model. This RFC adds
an immutable runtime descriptor generated from it:

```witchy
type RuntimeType

fn type_of(value: Dynamic) -> RuntimeType
fn runtime_type(T) -> RuntimeType
fn type_name(ty: RuntimeType) -> String
fn fields(ty: RuntimeType) -> List(RuntimeField)
fn methods(ty: RuntimeType) -> List(RuntimeMethod)
```

`runtime_type(T)` is a compile-time-resolved type-position operation that emits
an immutable descriptor constant; `T` is not treated as an ordinary runtime
value.

Descriptors contain public structural and callable metadata only. Private fields,
private methods, sealed constructors, and capability internals are absent. A
descriptor is data, not authority to bypass visibility.

Type identity is package coordinate plus module path plus nominal declaration
identity, not a display string. Structural records and unions use canonical
shape identity from RFC-0078.

### Checked operations

The initial surface is deliberately small:

- `field(name) -> Result(Dynamic, DynamicError)` for public readable fields;
- `call(name, args) -> Result(Dynamic, DynamicError)` for public dynamic methods;
- `implements(trait) -> Bool` and `as_trait(trait) -> Result(Dynamic, ...)`;
- inferred generic `try_decode()`, `decode()`, and `type()`;
- enumeration of public fields and dynamic methods.

`DynamicError` distinguishes missing field, missing method, arity mismatch,
argument mismatch, result mismatch, visibility denial, and capability denial.
There is no universal `method_missing` fallback in this RFC.

### Declaring dynamic methods

Not every public method enters runtime metadata. A module opts methods in:

```witchy
@dynamic
pub fn render(let self: Widget, context: Context) -> String:
    ...
```

The annotation preserves dead-code elimination and makes the dynamic surface
auditable in generated documentation. RFC-0080 attributes provide the structured
mechanism; before RFC-0080, an equivalent declaration marker may be compiler
syntax.

### Representation and value semantics

A dynamic value is semantically `(runtime_type, owned_value)`. It preserves
ordinary value semantics: putting a mutable value into `Dynamic` does not create
shared observable aliasing. The compiler may share frozen or RC-managed payloads
when existing rules permit it.

The WASM backend needs a tagged descriptor plus a representation-safe payload.
Scalar values may stay inline; aggregate payloads are owned references managed by
the existing memory model. This RFC does not reinterpret arbitrary 8-byte slots
based on a user-provided descriptor.

### Authority and capabilities

Capabilities do not implement `Reflect` and cannot convert to `Dynamic` directly.
A nominal value transitively containing a capability is likewise rejected unless
a later RFC defines a capability-aware existential envelope.

A dynamic method's capability requirements remain present in its reflected
signature. `call` succeeds only when arguments explicitly include values
satisfying those parameters. The method name never performs ambient capability
lookup. Footprint analysis includes every dynamically callable method reachable
from a constructed `Dynamic` value; implementations may narrow this conservative
set but may not omit it.

### Optimization and tooling

Static calls remain monomorphized. Dynamic calls use a descriptor method table and
are never silently chosen for ordinary syntax. The LSP can show a dynamic call's
known receiver constraints but must not pretend a string-named method is statically
resolved. `witchy caps` reports the conservative dynamic-call contribution.

## Alternatives

- **Use `Json` everywhere.** Good for data interchange, but erases nominal type
  identity and cannot invoke typed behavior.
- **Untagged unions.** Rejected by RFC-0078 and insufficient for open runtime
  shapes.
- **Make all values dynamically callable.** Ruby-like, but it increases binary
  metadata, weakens dead-code elimination, and makes typos runtime failures even
  when static facts exist.
- **Only `dyn Trait`.** Preferred for typed extension points, but cannot support
  generic inspectors and genuinely data-driven dispatch.

## Drawbacks

- Adds runtime type metadata and retention pressure.
- Conservative footprint accounting can over-report authority.
- Dynamic invocation weakens refactoring guarantees at explicitly marked sites.
- Representation work touches reflection, WIR, codegen, the interpreter, and
  package identity.

## Prior art

Python and Ruby reflection, C# `dynamic`, Swift `Any`, Rust `Any`, .NET metadata,
and WebAssembly component-model dynamic values inform this design.
