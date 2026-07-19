---
rfc: 0098
title: Structural record conformance and shape composition
status: accepted
created: 2026-07-19
tracking: implementation in progress; syntax/normalized-shape checkpoint
  merged as 9ab02e93 through mq-126f5826842b74196b346920b0be2a2b6b5355c5;
  checked shared projection lowering is rebased immediately after it
related:
  - "0078 (anonymous tagged unions and the structural tier — exact-record rule amended here)"
  - "0033 (place-based uniqueness — caller-place identity remains exact)"
  - "0043 (declared mutation write-back — why var arguments remain invariant)"
  - "0051 (memory-safety invariants — projections are typed constructions, never layout casts)"
  - "0069 (structured TypeInfo — projected values expose only the target shape)"
---

# RFC-0098: Structural record conformance and shape composition

## Summary

Let a richer anonymous record be used where a poorer anonymous record is
expected when every required field exists at the same type. The conversion is
directed, explicit in the checked program, and produces the exact target shape:

```witchy
type X = .{a: Int, b: String}
type Y = .{a: Int, b: String, c: Int}

fn takes_x(console: Console, x: X):
    console.print(x.b)

fn main(console: Console):
    let y: Y = .{a: 1, b: "2", c: 3}
    takes_x(console, y)
```

The proposal also adds type-level record composition, spelled as a structural
record spread:

```witchy
type X = .{a: Int, b: String}
type Y = .{..X, c: Int}
```

This is **closed-shape width conformance**, not row polymorphism and not general
intersection types. Record identity remains exact. A `Y` merely has a directed
coercion to `X` at an expected-type boundary; `X` and `Y` do not become the same
type.

## Relationship to RFC-0078

[RFC-0078](./0078-anonymous-tagged-unions.md) is implemented and therefore
frozen. It deliberately chose exact anonymous-record shapes and said records
would get no width subtyping. This RFC changes that one decision without
rewriting the historical RFC.

If implemented, this RFC supersedes only RFC-0078's **no record width
subtyping** rule. The rest of its structural-tier contract remains unchanged:

- `type X = ...` is a transparent shape alias, not a nominal declaration;
- structural record identity is field-order-insensitive and exact;
- structural types cannot carry capabilities, even transitively;
- user trait impls on structural types remain forbidden; and
- there are no inferred rows or open structural records.

RFC-0078's anonymous-union widening is representation-preserving because a
smaller tag set already has bits valid in the larger set. Record conformance is
different: current anonymous records have shape-specific layouts. This RFC
therefore specifies a real, type-directed projection rather than pretending
that two layouts are interchangeable.

## Motivation

Anonymous records are most useful as lightweight contracts: a function says
which named pieces of data it needs without demanding a nominal wrapper. Exact
shape equality makes that contract unnecessarily brittle. Adding an unrelated
field forces callers to reconstruct a smaller record even though the callee
cannot observe the extra field:

```text
type DisplayRow = .{id: Int, label: String}
type StoredRow = .{id: Int, label: String, revision: Int}

fn display(row: DisplayRow) -> String:
    "${row.id}: ${row.label}"

// Rejected today solely because StoredRow also has `revision`.
display(.{id: 7, label: "ready", revision: 3})
```

The manual workaround repeats fields, grows with every contract, and obscures
the useful fact that `display` depends only on `id` and `label`:

```witchy
display(.{id: stored.id, label: stored.label})
```

The same exactness makes related aliases repetitive. There is no way to say
"this structural shape plus one field"; authors must restate the base fields,
which allows the two declarations to drift:

```text
type DisplayRow = .{id: Int, label: String}
type StoredRow = .{id: Int, label: String, revision: Int}
```

Width conformance and type-level shape composition solve the two problems
together. The function contract stays small, richer values satisfy it, and
related aliases share one source of truth.

## Goals

1. Make a structural record with extra fields satisfy an expected structural
   record that requires a subset of those fields.
2. Keep the target value's observable shape exact: reflection, formatting,
   serialization, equality, and lowering see only the target fields.
3. Let one structural alias extend another without introducing nominal
   inheritance or a general intersection-type operator.
4. Preserve Witchy's ownership conventions: read-only use is directed,
   `own` consumes, and `var` write-back remains invariant.
5. Keep capability safety, backend parity, exhaustiveness, inference, and
   compiled layout decisions explicit and mechanically testable.

## Non-goals

- Open records, row variables, `where r has field`, or row inference.
- Depth subtyping of shared fields; shared field types remain exact.
- General intersection types such as `A & B`.
- Width conformance for nominal `type X:` records or variants.
- Structural function subtyping or automatic variance through containers.
- Mutation through a projected record view.
- Adding fields with value spread; value-position record spread keeps its
  existing exact-shape update semantics.
- User-defined conversions, destructors, or behavior on structural aliases.

## Terminology and formal rule

After transparent aliases and type parameters are substituted, let a closed
structural record shape be a finite map from field names to exact field types:

```text
Fields(R) = { name -> type }
```

An actual record `A` **conforms to** an expected record `E`, written `A <:w E`,
when:

1. both `A` and `E` are anonymous structural records;
2. every field in `E` exists in `A`; and
3. each shared field has the same type after alias substitution and ordinary
   type normalization.

Equivalently:

```text
dom(Fields(E)) subset-of dom(Fields(A))
and for every f in E: Fields(A)[f] == Fields(E)[f]
```

The relation is reflexive and transitive, but not symmetric. Field order is
irrelevant because anonymous-record identity already canonicalizes names.
Every structural record therefore conforms to the already-valid empty shape
`.{}`; projecting to it constructs an exact empty record, not the unit value
`()`. Shared fields are invariant: an inner richer record does not satisfy an
inner poorer field unless an independent expected-type boundary explicitly
projects that inner expression.

This relation is **not type equality**. Exact equality remains the rule for
unconstrained inference, generic identity, equality operands, function types,
`var` places, and every site not listed under coercion sites below.

## Design

### 1. Type-level structural record spread

In type position, an anonymous record may begin with one base spread followed
by zero or more explicit fields:

```witchy
type Base = .{a: Int, b: String}
type Extended = .{..Base, c: Int}
type Generic(a) = .{value: a}
type Located(a) = .{..Generic(a), line: Int, column: Int}
```

The grammar is:

```text
anonymous-record-type := ".{" [ ".." type [ "," field-types ]
                                  | field-types ] "}"
field-types           := field ":" type ("," field ":" type)*
```

Rules:

- The spread must be first and there may be at most one.
- Its type must normalize to an anonymous structural record. A nominal record,
  tuple, union, capability, existential, or unconstrained type variable is an
  error.
- The result is immediately normalized to one ordinary exact structural shape;
  the spread does not survive into type checking or runtime metadata.
- An explicit field already present in the base is allowed only when its
  normalized type is identical. The duplicate then collapses to one field.
- A same-named field with a different type is a compile error. Composition
  never silently overrides or refines a field type.
- The formatter prints the spread first and sorts the explicit suffix fields by
  name. Once aliases are erased, diagnostics and reflection print the resulting
  exact `.{...}` shape.

The base-first spelling is type-position-only. Existing value spread remains an
exact-shape update such as `.{b: "new", ..value}`; this RFC does not let a value
spread introduce `c` into a value whose inferred shape lacks `c`.

### 2. Directed width coercion

When an expression of structural record type `A` appears at a listed boundary
whose expected type is structural record `E`, the checker tries ordinary exact
unification first. If exact unification fails, it applies `A <:w E`. Success
records a **record projection** in the checked program.

The projection:

1. evaluates the source expression exactly once under the existing evaluation
   order;
2. reads each field required by `E` from the resulting `A` value;
3. constructs a value with exact shape `E`; and
4. handles the unselected fields according to the parameter/ownership
   convention in §Ownership conventions.

The compiler must never implement this as an unchecked pointer cast, synthetic
name substitution, or layout-prefix assumption. Anonymous records are keyed by
their full field-name set and may have different field order, GC shape, and
drop/copy obligations.

### 3. Coercion sites

Width conformance is available only where the program already supplies a
concrete expected type:

| Site | Example | Result |
|---|---|---|
| `let`/`var` annotation | `let x: X = richer` | exact `X` value |
| assignment | `x = richer` where `x: X` | exact `X` value |
| default read-only argument | `takes_x(richer)` | projected argument |
| explicit `let` argument | `inspect(richer)` for `let x: X` | non-escaping projected borrow/value |
| `own` argument | `consume(move richer)` for `own x: X` | consumes source, produces exact `X` |
| return / tail expression | function declares `-> X` | exact `X` result |
| typed aggregate slot | expected field/list/tuple slot is `X` | exact `X` element |
| explicit ascription | `richer as X` | exact `X` value |

The existing `as` operator gains this one additional directed case. It succeeds
only when the source structurally conforms to the target; it cannot add fields
or convert a nominal record merely because the names happen to match.

The following remain exact and do not invent an expected shape:

- unannotated `let` inference;
- branch or match-arm joins without an enclosing expected type;
- equality, ordering, hashing, and dictionary-key comparison;
- generic type-argument inference before a concrete parameter is known;
- function-type identity and higher-order function assignment; and
- `var` arguments.

For example, this stays an error rather than inferring a greatest common row:

```text
let row = if detailed:
    .{a: 1, b: "two", c: 3}
else:
    .{a: 1, b: "two"}
// error: branch record shapes differ; add `: X` or `as X`
```

An explicit expected type makes the decision local and visible:

```witchy
let row: X = if detailed:
    .{a: 1, b: "two", c: 3}
else:
    .{a: 1, b: "two"}
```

### 4. Contextual richer literals

A record literal is inferred from all fields it spells before conformance is
checked. An expected `X` does not suppress evaluation or typing of extra fields:

```witchy
let x: X = .{a: make_a(), b: make_b(), c: make_c()}
```

All three expressions are evaluated once in the ordinary source order. The
result is then projected to exact `X`, so `c` is not observable through `x`.
An optimizer may eliminate pure dead construction only under the same rules as
any other semantics-preserving dead-code optimization.

This is deliberately not TypeScript-style "excess property checking." A richer
literal and a richer named binding obey the same conformance rule.

### 5. Ownership conventions

The projection respects the callee's declared convention.

At non-consuming annotations, assignments, and calls, an existing source
binding retains its richer value under ordinary value semantics. A temporary or
owned source ends its lifecycle normally after the selected fields have been
copied or moved; projection does not add a second destruction event.

#### Default immutable value

The caller retains its original richer value. The callee receives an exact
poorer value under ordinary value semantics:

```witchy
let y: Y = .{a: 1, b: "two", c: 3}
takes_x(console, y)
console.print("${y.c}") // still valid
```

#### Explicit `let` borrow

The no-escape rule remains authoritative. The implementation may borrow the
required fields or materialize a temporary exact record, but the observable
callee value is `X`, reflection sees only `X`, and no projected borrow may
escape the call.

#### `own`

An `own X` parameter accepts a conforming `Y`. Passing a binding consumes that
binding exactly as any other `own` call does. The projection may move shared
fields into `X`; omitted fields are dropped under the ordinary value lifecycle.
Using the original `Y` after the call is rejected as use-after-move.

Ownership qualifiers are checked before width conformance. Projection cannot
manufacture `unique`, discard a borrow lifetime, or weaken `frozen`; existing
qualifier rules apply to the source and target, then the underlying record
shapes are compared.

#### `var` is invariant

A `var X` parameter requires a caller place whose type is exactly `X`. A `Y`
place is rejected even though `Y <:w X`:

```text
fn replace(var x: X):
    x = .{a: 0, b: "replaced"}

var y: Y = .{a: 1, b: "two", c: 3}
replace(y)
// error: `var` argument requires exact `X`; found `Y` with extra field `c`
```

The callee can write back any valid `X`, which contains no `c`. Automatically
merging that result into the original `Y` would turn projection into a hidden
lens, complicate nested-place rebuilding, alias rejection, traps, and
transactional write-back. This RFC does not define that behavior.

Code that wants subset mutation makes the conversion and reconstruction
explicit, or changes the function to return an `X` value. A future RFC may
design structural lenses across `var`; it must preserve the existing rule that
write-back commits only after a structured successful return and never leaks a
partial update on a trap.

### 6. Generic behavior and no row inference

Generic aliases may compose concrete structural shapes after substitution:

```witchy
type Value(a) = .{value: a}
type Located(a) = .{..Value(a), line: Int}
```

A generic function can receive a projected record when its instantiated
parameter type is already concrete. This RFC does not add field constraints to
type variables:

```text
fn label(row: r) -> String:
    row.b
// still an error: unconstrained `r` is not known to have field `b`
```

Nor does it make `List(Y)` conform to `List(X)` as a type. A typed list literal
may project each element because each element has expected type `X`; an existing
`List(Y)` remains a distinct invariant container and must be mapped explicitly.

Function types remain exact. `fn(Y) -> R` is not automatically assignable to
`fn(X) -> R`, nor vice versa. Direct calls perform argument projection only
after the actual callee signature is known.

### 7. Nominal and structural boundaries

Width conformance applies only when both normalized heads are anonymous
structural records. These do not conform merely by sharing fields:

```text
type User:
    a: Int
    b: String
    c: Int

type X = .{a: Int, b: String}

let x: X = User(1, "two", 3) // error: nominal `User` is not structural `X`
```

Nominal records may carry invariants, impls, sealing, and capabilities. Field
projection must never bypass their constructors or erase their identity. An API
that wants a structural view constructs one explicitly or provides a method
returning it.

### 8. Reflection, rendering, serialization, and equality

A successful projection produces exact target shape `E`. There are no hidden
fields and no retained dynamic source-shape tag.

Given:

```witchy
type X = .{a: Int, b: String}
let x: X = .{a: 1, b: "two", c: 3}
```

the observable results are:

```text
show/render: .{a: 1, b: two}
reflection fields: [a, b]
JSON object: {"a":1,"b":"two"}
```

Equality and hashing remain exact-shape operations. Comparing `X` directly to
`Y` is a type error because equality has no directed expected operand. The
author may make the policy explicit:

```witchy
x == (y as X)
```

This prevents operand order from deciding which fields participate.

### 9. Capability and authority safety

RFC-0078's transitive capability firewall remains unchanged: an anonymous
record containing a capability anywhere in its shape is rejected. Type spread
normalizes its base and then applies the same recursive check to the result.

Consequently projection cannot hide authority in an omitted field, and shape
composition cannot launder a nominal capability into the structural tier. The
checker must issue the existing capability-payload diagnostic before recording
a projection.

## Representation and backend parity

Anonymous records currently lower through shape-keyed synthetic record types.
`.{a: Int, b: String}` and `.{a: Int, b: String, c: Int}` therefore have
different concrete identities and may have different interpreter metadata,
linear-memory layouts, or Wasm GC struct types.

This RFC preserves that model:

- **Checked representation:** the type checker records the exact source and
  target shapes for every directed projection. Acceptance without a retained
  coercion fact is a compiler bug.
- **Interpreter:** evaluates the source once and constructs an exact target
  record from the selected fields. It is the value-semantics oracle.
- **Compiled Wasm:** lowers the same checked projection shape-by-shape. It may
  move, copy, or destination-construct fields according to their concrete WIR
  kinds, but may not reinterpret the source layout as the target layout.
- **Reference-bearing fields:** retain their precise typed reference kinds.
  Projection must not round-trip them through the universal integer slot.
- **Resource behavior:** projection may allocate in the baseline lowering.
  Ownership analysis may elide the allocation or move fields when justified,
  but optimization cannot change reflection, aliasing, source evaluation, or
  ownership behavior. A `mode opt` contract that forbids allocation must either
  prove the projection allocation-free or reject that program loudly.

Both backends must agree on success output, reflection/serialization, source
evaluation, use-after-move rejection, and every failure diagnostic. Resource
claims come from the compiled backend's exported counters, as elsewhere.

## Diagnostics

Diagnostics name both shapes and the directed reason conformance failed.

Missing field:

```text
record `.{a: Int}` does not conform to `.{a: Int, b: String}`:
missing required field `b`
```

Wrong field type:

```text
record field `b` has type `Int`, but expected `String`
while projecting `.{a: Int, b: Int, c: Int}` to `.{a: Int, b: String}`
```

Invalid type spread:

```text
type spread requires an anonymous record shape; `User` is nominal
```

Conflicting composition:

```text
field `a` has conflicting types in structural record composition:
base provides `Int`, extension declares `String`
```

Invariant write-back:

```text
`var` argument requires exact record shape `X`; found conforming `Y`
with extra field `c`; project into a separate `X` value or change the API
```

An attempted general intersection receives a targeted syntax hint when
possible:

```text
structural record composition is spelled `.{..X, c: Int}`; `&` is not a type operator
```

## Implementation plan

Implementation should land in independently testable vertical slices. A slice
is not complete until its interpreter behavior, compiled behavior, adversarial
rejections, tooling surface, and relevant docs agree.

### 1. Syntax and normalized shapes

- Add a parsed/deferred structural-record type form capable of retaining one
  base spread until aliases and generic arguments are resolved. The current
  parser eagerly synthesizes a `Type::Named(__anon...)`; it cannot resolve
  `..X` correctly before alias expansion.
- Extend alias-cycle discovery, substitution, qualification, quote/type
  metaprogramming, formatter traversal, and structural type collection for the
  deferred form.
- After alias normalization, merge fields, diagnose conflicts, sort names, and
  synthesize the existing exact anonymous-record head. No composition node
  reaches runtime lowering.
- Add parser and formatter round-trip tests, including generic bases, duplicate
  identical fields, conflicts, invalid bases, cycles, and generated syntax.

### 2. Directed conformance facts

- Add `record_width_conformance(expected, actual)` beside the existing directed
  coercion seams for capability narrowing, anonymous-union widening, and
  existential erasure.
- Preserve exact unification as the first choice and keep `var` arguments on an
  invariant path.
- Record a projection fact keyed by stable checked expression identity, carrying
  fully normalized source/target field maps and field kinds. Do not make
  codegen or the interpreter rediscover conformance from strings.
- Extend `as` checking for explicit structural projection.
- Add negative tests proving there is no row inference, nominal projection,
  container covariance, function variance, or symmetric equality coercion.

### 3. Shared semantic projection

- Make both consumers of the checked program consume the same projection fact,
  or lower it once to a compiler-owned typed projection node before the
  interpreter/codegen split.
- Interpreter construction establishes the oracle for exact shape,
  source-once evaluation, rendering, equality, reflection, and ownership.
- Compiled lowering emits typed field selection and target construction for
  scalars, heap values, closures, and fixed-layout reference aggregates.
- Add source-level assertions preventing an unchecked layout cast or anonymous
  synthetic-name relabel from becoming the implementation.

### 4. Ownership conventions

- Cover default, explicit `let`, and `own` arguments independently.
- Prove that a default/borrow call leaves the richer caller binding usable and
  unchanged.
- Prove that `own` consumes the richer binding and that later use is rejected.
- Reject `var Y -> var X` at roots and nested places before reservation or
  write-back planning; retain existing overlap/alias diagnostics.
- Exercise qualifiers (`frozen`, `unique`, borrowed views) so conformance cannot
  manufacture or discard an ownership contract.

### 5. Tooling and public contract

- LSP diagnostics and hover show the target exact type and the projection site.
- Expansion/type quoting preserves type-spread source until normalization and
  shows the normalized shape afterward at the established tooling boundary.
- `witchy fmt` canonicalizes type spread.
- Update `spec/language.md` only when behavior ships; replace the book's
  exact-record warning with a runnable conformance/composition example.
- Regenerate the checked book manifest and ensure the browser compiler executes
  the example on the compiled backend.
- Add migration guidance distinguishing `.{..X, c: Int}` from value update
  spread and explaining why `var` remains exact.

## Acceptance criteria

The RFC is implemented only when checked-in evidence proves all of the
following:

1. `Y` flows to `X` through annotations, assignments, default arguments,
   explicit `let` arguments, `own` arguments, returns/tails, typed aggregate
   slots, and explicit `as X`.
2. Missing required fields and mismatched shared field types are rejected with
   source-located diagnostics on both CLI and LSP paths.
3. `.{..X, c: Int}` normalizes to the same identity as the directly spelled
   full shape, including through generic aliases and across field orderings.
4. Identical duplicate fields collapse; conflicting duplicates and non-record
   bases fail loudly.
5. Nominal records, capabilities, existentials, tuples, unions, and
   unconstrained type variables cannot be projected merely because some field
   names appear compatible.
6. Unannotated branch joins, generic field inference, existing containers,
   function values, and cross-shape equality remain exact.
7. Reflection, rendering, JSON, equality, and hashing observe only the target
   fields after projection.
8. Source expressions, including omitted field expressions, evaluate exactly
   once in ordinary order.
9. Default/borrow calls preserve the richer caller value; `own` consumes it and
   rejects later use.
10. `var` arguments remain invariant at bindings, fields, indexes, and nested
    places; no partial write-back or hidden merge is possible.
11. Interpreter and compiled Wasm agree on all positive behavior and complete
    failure diagnostics.
12. Compiled reference fields preserve their typed representation; structural
    checks guard against integer-slot laundering or unchecked layout casts.
13. Optimization-counter tests compare shallow and deep/projection-heavy cases
    where a resource claim is made; `mode opt` either proves the required bound
    or rejects the shape.
14. Formatter, expansion, quoting, hover, and diagnostics understand the new
    type form.
15. The language spec, runnable book example, migration guidance, RFC tracking,
    and checked book manifest land with the implementation.

## Compatibility and migration

The change is source-compatible: programs that compile today keep their type
identity and behavior. Some programs rejected for extra structural fields begin
to compile at directed expected-type sites.

No existing expression silently loses fields under unconstrained inference.
Field loss occurs only where the source already names an expected poorer shape
or explicitly writes `as Poorer`. Existing exact-shape equality, hashing,
generic inference, and `var` behavior do not change.

The new type-spread syntax is additive. `&` remains available for a future
decision rather than being committed as a partial general intersection
operator.

When implementation lands, RFC-0078 receives only an allowed dated change-note
pointing here; its historical body is not rewritten. `spec/` and the book then
become the current behavioral authority.

## Alternatives

### Keep exact structural records

This is simple and is RFC-0078's current rule. It preserves one equality-only
type relation but makes dependency-minimal function contracts awkward and
forces repetitive manual projection. The user cost now outweighs the checker
simplicity because anonymous record types are public, denotable language
features rather than internal literal sugar.

### General intersections: `type Y = X & .{c: Int}`

This is compact and familiar from TypeScript. It also appears to promise much
more: intersections of nominal types, traits, unions, functions, capabilities,
and generic constraints; distributivity; bottom/conflict behavior; and a new
precedence level throughout the type grammar.

Witchy needs one closed-record composition operation, not that lattice. The
dedicated `.{..X, c: Int}` spelling states the narrow operation and leaves `&`
unclaimed until the language has a real general-intersection design.

### Preserve the richer runtime value and expose a poorer static view

Many structural object systems retain hidden extra properties. In Witchy this
would make `show`, reflection, JSON, equality, hashing, and runtime type
information depend on whether they follow static or dynamic shape. It would
also require a common object layout or fat view and would make capability
auditing harder to reason about.

Exact projection gives one answer everywhere: a value typed `X` is represented
and observed as `X`.

### Open rows / row-polymorphic functions

A row-polymorphic function could express `fn label(row: .{b: String, ..r})` and
return a value preserving `r`. That is more powerful, especially for structural
updates, but introduces row variables, row unification, inferred constraints,
escape rules, and substantially harder diagnostics. RFC-0078 deliberately drew
the no-row line; this RFC keeps it.

### Explicit projection only

Requiring `y as X` at every call is mechanically simple and remains available
when an author wants emphasis. It defeats the conformance goal for ordinary
read-only contracts, where the parameter already supplies an unambiguous target
shape.

### Make `var` use a hidden structural lens

The callee could receive `X`, then merge its final fields back into `Y` while
preserving `c`. That is attractive but is not a small extension: the language
would need to specify nested-place lens composition, alias reservations,
ownership of replaced fields, structured-return commits, and trap rollback.
Keeping `var` invariant is sound, predictable, and consistent with existential
`var` arguments.

### Nominal inheritance or automatic nominal-to-structural conversion

Nominal types exist to carry identity, invariants, constructors, impls, sealing,
and authority. Automatic field-based erasure would undermine that boundary.
Nominal APIs should opt in by constructing a structural view explicitly.

## Drawbacks

- A type mismatch can now insert real record construction. What looks like a
  flexible call may allocate until lowering learns to elide the projection.
- The type checker must retain coercion facts for both backends; returning `Ok`
  from unification is no longer enough because record widening is not a runtime
  no-op.
- Type-position spread and value-position spread have related but distinct
  rules: type spread composes a new exact shape, while value spread updates an
  existing exact shape.
- `var` remains less flexible than read-only and `own` calls. This asymmetry is
  necessary but must be explained clearly in diagnostics and the book.
- Exact projection discards extra fields at an annotation/return boundary.
  This is intentional and observable through reflection, so careless type
  annotations can lose data.
- The deferred type-spread form touches alias expansion, formatter traversal,
  metaprogramming, and diagnostics even though it disappears before runtime.

## Prior art

- **TypeScript structural object assignability** demonstrates the ergonomics of
  passing objects with extra fields to smaller contracts. Witchy deliberately
  does not copy TypeScript's general intersections, open-ended object model, or
  context-dependent excess-property checks.
- **OCaml object and polymorphic-variant rows** demonstrate the expressive end
  of width subtyping and the diagnostic complexity of inferred rows. Witchy
  keeps shapes closed and requires a concrete expected type.
- **Elm extensible record annotations** show the value of naming a required
  field subset, but they entail row-polymorphic function types. This RFC stops
  at directed closed-shape projection.
- **Roc record and tag-union design** reinforces the usefulness of structural
  local data while also illustrating how quickly row inference becomes a core
  type-system feature.
- **Witchy anonymous-union widening (RFC-0078)** is the local precedent for a
  directed structural relation at expected-type boundaries. This RFC reuses
  that user model but not its no-op runtime representation assumption.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields and dated notes.
  - Current behavior belongs in spec/ and executable evidence, not this proposal.
-->
