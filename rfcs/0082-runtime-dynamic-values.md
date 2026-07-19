---
rfc: 0082
title: Runtime Dynamic values and checked reflection
status: proposed
created: 2026-07-12
superseded-by:
tracking: stage 1 in progress — canonical package/declaration/type identities and deterministic closed descriptor plans live in witchy-types::runtime_type; source Dynamic conversion is not implemented yet
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

## Dependencies and implementation order

RFC-0082 is not an alternative representation for RFC-0081. It consumes the
same resolved declaration identities, package coordinates, and convention-aware
call contracts, then adds a checked runtime boundary on top. The following are
hard prerequisites:

1. **RFC-0005:** aggregate and closure payloads have a representation-safe
   typed slot boundary on both backends. A `Dynamic` payload must never be
   recovered by interpreting an arbitrary scalar slot as a reference.
2. **RFC-0081 slices 1-2:** resolved nominal and trait identities, canonical
   anonymous-union shapes, and deterministic method-slot layouts exist before
   a descriptor is emitted. RFC-0082 does not use type display names as keys.
3. **RFC-0081 owned-value execution:** an owned payload and authenticated
   dispatch envelope work on both backends before `as_trait` or `call` ships.
4. **RFC-0080 attributes:** `@dynamic` is compiler-owned, hygienic declaration
   metadata before dynamic method enumeration or invocation ships. A temporary
   spelling is not a second public surface.
5. **RFC-0083 loan facts:** an active owner loan prevents an in-place dynamic
   payload extraction or mutation path. The baseline remains correct by copying
   when no uniqueness proof is available.

Implementation proceeds in these independently mergeable stages:

1. **Descriptor identity and conversion:** add backend-neutral immutable
   descriptor plans; implement `dynamic`, `type`, `decode`, and `try_decode`
   for reflectable, capability-free values. This stage has no field access or
   string dispatch.
2. **Public shape inspection:** add descriptor enumeration and checked public
   field reads. Private members and capability-bearing transitive payloads must
   be rejected before either backend executes the operation.
3. **Authenticated calls:** consume RFC-0081's resolved call contract for
   `@dynamic` methods, with checked arity, argument descriptors, result
   descriptors, explicit capability arguments, and conservative footprint
   accounting.
4. **Trait bridge and tooling:** add `implements`/`as_trait`, `witchy caps`,
   LSP presentation, generated documentation, and migration guidance only after
   stages 1-3 agree across both backends.

The first stage is split at an explicit backend-neutral boundary. As currently
built, `witchy-types::runtime_type` owns immutable package coordinates, resolved
declaration identities, structural identities, authenticated import-alias
mapping, and deterministic descriptor IDs closed over nested types. Unknown or
conflicting declarations and direct capability types fail while building that
plan. No source-level `Dynamic`, payload envelope, conversion operation, or
backend runtime behavior exists until the remaining stage-1 slices land; the
identity plan alone is not presented as an implemented user feature.

The linker now retains each declaration's compiler key, source-module key,
local name, and kind before flattening, and the descriptor catalog can join
that record to loader-assigned ownership without parsing names. The production
package path does not yet supply the required ownership map: the self-hosted PM
currently reduces a selected dependency to `--dep alias=path`, discarding its
package name, version, and source before Rust loading. Until a richer loader
contract carries those authenticated coordinates (including toolchain-owned
std modules), catalog construction fails on a missing module owner rather than
deriving identity from an alias or filesystem path.

## Representation and failure invariants

- A descriptor identity is immutable and compares by resolved identity, never
  by `type_name`. Nominal identities include package coordinate, module path,
  declaration identity, and instantiated type arguments. Structural records and
  RFC-0078 unions compare by their canonical normalized shape identity.
- A `Dynamic` owns its payload according to the normal value model. Converting a
  mutable source cannot expose a second observable mutable alias; unique
  ownership may move, while shared or borrowed storage follows the established
  copy-on-write and loan rules.
- A descriptor cannot manufacture authority. Capability values, values that
  transitively contain capabilities, private members, sealed constructors, and
  non-opted-in methods are absent or rejected at the boundary.
- Every failure is data: malformed descriptors, descriptor/payload mismatch,
  missing member, visibility denial, arity mismatch, argument mismatch, result
  mismatch, and capability denial return `DynamicError`. Compiler bugs and
  runtime traps do not become successful dynamic values, and no operation may
  silently fall back to a string lookup or ambient authority.
- The interpreter is the semantic reference, but the compiled representation
  must use only descriptor-authorized typed loads, stores, and calls. A scalar
  optimization is valid only when it preserves the same ownership and failure
  behavior as the aggregate/reference path.

## Acceptance criteria

RFC-0082 is implemented only when all of the following are true:

1. The compiler emits one canonical descriptor identity per resolved nominal
   instantiation and canonical RFC-0078 shape; package-distinct same-spelled
   declarations and display-name collisions cannot decode each other.
2. `dynamic` rejects direct and transitive capability payloads at type checking,
   with a source diagnostic naming the retaining field or constructor path.
3. `decode` and `try_decode` preserve exact value semantics for scalars,
   aggregate values, generic values, anonymous records, and anonymous unions;
   mismatches return `DynamicError`/`None`, never a trap or unchecked cast.
4. Descriptor constants, conversion, and mismatch behavior agree on the
   interpreter and compiled-Wasm backend, including reference-bearing payloads.
5. Public field enumeration and `field` expose only declared public readable
   members; private, sealed, missing, and malformed requests fail distinctly.
6. `@dynamic` method tables use RFC-0081's resolved identity and slot contract,
   validate arity/argument/result descriptors, and cannot invoke a private or
   non-opted-in method by spelling its name.
7. Dynamic calls retain explicit capability parameters and `witchy caps`
   includes every reachable opted-in method conservatively. A method name alone
   cannot widen authority.
8. Active RFC-0083 loans, shared payloads, and unique payloads each take a
   memory-safe path. Differential tests cover copy/move decisions and prove no
   use-after-reclaim or observable aliasing.
9. The LSP and generated docs identify dynamic operations as checked runtime
   dispatch, and source diagnostics remain loud when an expected decode type or
   required capability argument is absent.
10. Each stage carries focused type-checking, interpreter, compiled-Wasm, and
    adversarial malformed-descriptor tests; the final stage updates the language
    specification, book, stdlib surface, and package-boundary examples.

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
