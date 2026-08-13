---
rfc: 0122
title: "Uniform lifetime arguments and exclusive borrowed access"
status: proposed
created: 2026-08-13
tracking: "Proposal; supersedes the RFC-0083/RFC-0112 borrowed-type spellings if accepted"
predecessors:
  - "[0026](0026-unique-qualifier.md) (`unique` and `local unique` ownership contracts)"
  - "[0083](0083-opt-mode-lifetimes.md) (`let('a) T`, `View(T, 'a)`, and shared owner loans)"
  - "[0087](0087-fused-mutators.md) (`var` as exclusive move-in/write-back access)"
  - "[0110](0110-opt-ownership-access-abi.md) (one typed access envelope for every callable)"
  - "[0112](0112-borrowed-aggregate-types.md) (lifetime-bearing nominal values and projection-aware loans)"
---

# RFC-0122: Uniform lifetime arguments and exclusive borrowed access

## Summary

Replace the two spellings for a shared borrowed value,
`let('a) T` and `View(T, 'a)`, with one lifetime-argument spelling on the value's
ordinary type:

```text
fn first(let text: String('a)) -> String('a):
    text
```

`T` remains an owned value. `T('a)` is the same logical value with a validity
relation to owner lifetime `'a`. The rule applies uniformly to built-ins,
generics, nominal aggregates, function types, fields, parameters, and results:

```text
String('a)
List(Int, 'a)
Parser('a)
List(Token('a))
```

Lifetime arguments are compile-time relation arguments, not runtime generic
arguments and not a second family of `StringView`, `ListView`, or user-defined
view types. They are erased from value representation after the checker and
lowering have retained the exact owner-root and projection facts.

The parameter conventions remain separate:

- `let` opens or forwards shared borrowed access;
- `var` reserves exclusive move-in/write-back access to a caller place;
- `own` consumes the argument value or borrow handle; and
- no convention means an ordinary observably immutable value argument.

The RFC also completes the composition with `unique`. `unique T` remains an
owned value proven to have no aliases. `unique T('a)` is an exclusive borrowed
access tied to `'a`. It may be moved, returned, or rebound like any other
lifetime-bearing value, but its owner is inaccessible while it is live and any
mutation is committed through the same exclusive-place/write-back model as
`var`. This adds no shared mutable aliasing and no pointer identity.

This is a source-breaking syntax replacement. If accepted, the implementation
lands as one compiler-and-library cut with a mechanical migration command; the
old spellings do not remain as aliases.

## Motivation

### The current input spelling repeats `let`

RFC-0083 deliberately separated a parameter convention from a type qualifier:

```text
fn first(let text: let('a) String) -> View(String, 'a)
```

The first `let` says how the parameter is passed. The second says that the value
has a named owner relation. That distinction is real in the compiler, but the
surface makes one idea look accidentally duplicated and uses a different shape
for the result.

The asymmetry gets worse in aggregate APIs:

```text
type Parser('a):
    input: View(String, 'a)

fn parser(let input: let('a) String) -> Parser('a)
```

All three occurrences carry the same kind of lifetime relation, yet they use
three visual forms: `let('a) T`, `View(T, 'a)`, and `Parser('a)`.

### A lifetime is a relation carried by a value

Witchy does not expose addresses or reference identity. A borrowed string is
still logically a `String`; a borrowed parser is still a `Parser`. The useful
static fact is that the represented value depends on one or more owner roots.

Writing that fact as a lifetime argument keeps the ordinary type at the head:

```text
String             // owned String
String('a)         // String whose validity depends on 'a
Parser('a)         // Parser whose declared fields depend on 'a
List(Token('a))    // owned list containing tokens that depend on 'a
List(Int, 'a)      // view of a List(Int) tied to 'a
```

The spelling follows the same kinded argument grammar already shipped for
`type Parser('a)`. The parser does not need to know a list of borrowable type
names and libraries do not mint parallel view constructors.

### Ownership and lifetime are independent axes

The current design has useful independent concepts:

- a calling protocol (`let`, `var`, `own`, or default);
- an aliasing/immutability contract (`unique`, `local unique`, `frozen`); and
- an owner relation (`'a`).

They should compose rather than reject combinations by spelling. In particular,
an exclusive borrowed access is meaningful:

```text
unique String('a)
```

It does not own the underlying allocation, but it is the only live access that
can mutate that allocation for `'a`. Moving or consuming that access is valid;
it moves or consumes the handle and its obligation, not the owner allocation.
The owner becomes available again only after the obligation closes.

### Better analysis should not require more surface syntax

Rust's Polonius work is useful here for its model, not its punctuation. Its
central move is to separate symbolic origins from concrete loans and to compute
which loans can reach live origins at each relevant control-flow point. That
accepts sound conditional-return and lending-iterator patterns that a coarser
region-as-CFG-points model rejects.

Witchy should likewise keep `'a` as a compact API relation while improving the
point-indexed loan engine underneath it. Users name which results depend on
which inputs; they do not manually encode branch or lexical end points.

## Design

### 1. One lifetime-argument syntax

A lifetime relation is a kinded type argument beginning with `'`. It appears in
the same parenthesized argument list as ordinary type arguments:

```text
String('a)
List(Int, 'a)
Dict(String, Token('a))
PairView(Int, 'left, 'right)
```

Ordinary type arguments and lifetime arguments remain different kinds. A type
parameter `a` can receive `String`; a lifetime parameter `'a` can receive only a
relation. No runtime `TypeId`, constructor field, monomorphized body, or Wasm
argument is created for a lifetime.

The grammar becomes conceptually:

```text
type-argument = type | lifetime
lifetime      = "'" identifier
named-type    = qualified-name ["(" type-argument {"," type-argument} ")"]
```

Kind checking, rather than parser branches for `View` or `let('a)`, determines
which argument positions are ordinary types and which are relations.

Every first-class non-capability value type may carry one direct-storage
relation in addition to the relation parameters declared by its nominal shape.
This includes scalars; `Int('a)` is legal and usually representation-free, which
keeps generic code uniform. Capabilities remain excluded because an ordinary
lifetime cannot extend host authority or replace a host lease.

Built-in constructors expose the direct relation as a trailing lifetime
argument after ordinary type arguments:

```text
String('a)
Bytes('a)
List(Int, 'a)
Dict(String, Int, 'a)
```

This is distinct from storing relation-bearing elements:

```text
List(String('a))       // owned list; each element depends on 'a
List(String, 'a)       // borrowed view of the list storage itself
```

Both may be useful, and the type system preserves the distinction.

### 2. Nominal lifetime parameters

The existing declaration syntax remains:

```text
type Parser('input):
    input: String('input)
    offset: Int

type PairView(a, 'left, 'right):
    first: a('left)
    second: a('right)
```

A declared lifetime parameter says that instances of the nominal type carry
that dependency through their fields. Construction must prove every declared
relation from the supplied field values. An unused declared relation is an
error.

A nominal value may itself be borrowed. Its direct-storage relation follows its
declared arguments:

```text
Parser('input, 'parser)
```

Here `'input` is the relation declared by `Parser`; `'parser` is a view of the
parser shell itself. Projecting `.input` retains `'input`; reading or replacing
the parser shell is constrained by `'parser`. The kind checker knows the
nominal declaration's relation arity, so this remains deterministic without a
new delimiter.

Most APIs do not need the second relation. Passing `Parser('input)` by `let`
creates a call-scoped shell borrow, and returning a field carries only the
declared `'input` relation:

```text
fn lexeme(let parser: Parser('input)) -> String('input)
```

### 3. Lifetime binding and quantification

Lifetime names in a callable signature are implicitly universally quantified.
An input occurrence binds the name; every result occurrence must be reachable
from an input relation of the same name:

```text
fn first(let text: String('a)) -> String('a)
fn parser(let text: String('a)) -> Parser('a)
```

A result cannot introduce an unbound lifetime:

```text
fn bad(text: String) -> String('a)       // error: 'a has no input owner
```

Multiple inputs may use different relations when the result identifies one:

```text
fn left(let x: String('a), let y: String('b)) -> String('a)
```

Inputs may intentionally share a relation when a result can come from either:

```text
fn choose(let x: String('a), let y: String('a), pick: Bool) -> String('a)
```

At a call, using one symbolic name for multiple arguments computes a common
relation bounded by every contributing owner. It does not assert that the two
arguments have the same allocation. The result remains valid only while all
owners that can reach it remain valid. Flow-sensitive facts may narrow that
owner set when the selected path is known.

Elision remains available for a call-scoped borrow that does not escape:

```text
fn length(let text: String) -> Int
```

Public borrowed results and ambiguous multiple-owner contracts spell their
relations. The compiler does not invent an externally visible lifetime name.

### 4. Meaning of the parameter conventions

The lifetime argument does not replace parameter conventions.

| Signature fragment | Meaning |
|---|---|
| `x: T` | owned, observably immutable value argument |
| `let x: T` | anonymous shared borrow confined to the call |
| `let x: T('a)` | shared borrow whose relation may flow through the typed result |
| `x: T('a)` | an existing relation-bearing value passed with ordinary value semantics |
| `var x: T` | exclusive move-in/write-back of an owned caller place |
| `own x: T` | consume an owned value |
| `var x: T('a)` | rebind/write back an existing relation-bearing value while preserving `'a` |
| `own x: T('a)` | consume a relation-bearing value and transfer its loan obligation |

An owned `T` may satisfy `let x: T('a)` by opening a shared loan at the call.
An existing `T('b)` may instantiate `'a = 'b` and forward the same concrete
loan. Default-convention `T('a)` copies the relation-bearing value, not its
payload; every resulting handle contributes to the same owner obligation.

`own x: T('a)` consumes the borrowed value, not the referent. If the argument is
an existing borrowed binding, that binding is dead after the call. An owned `T`
is not silently consumed as a `T('a)`; owner-to-borrow conversion occurs only at
a borrow- or exclusive-access site whose source semantics preserve the owner.

`var x: T('a)` writes a final borrowed handle back to a caller place already
typed with the same relation. It does not turn a shared view into mutable access
to its owner. Mutation of owner storage requires exclusive access below.

### 5. Shared and exclusive relation-bearing values

`T('a)` is shared read access. Any number may coexist. While one is live, the
owner may be read but cannot be moved, consumed, reassigned, or mutated in a way
that can invalidate the view.

`unique T('a)` is exclusive borrowed access. Exactly one access to the covered
owner place may be live. It conflicts with every overlapping shared or
exclusive access, but disjoint projections may coexist when the place oracle
proves them disjoint.

The four central forms are:

```text
T                    owned value
T('a)                shared borrowed value
unique T             uniquely owned value
unique T('a)         exclusive borrowed value
```

`unique T('a)` is affine. It cannot be implicitly copied or weakened to two
shared values. It may be moved into `own`, threaded through a `var` place, or
returned with the same relation. It may be reborrowed temporarily as shared
`T('b)` where `'b` is bounded by `'a`; the exclusive access is suspended until
all such reborrows close.

An exclusive borrow can be opened only from a mutable, write-back-capable place
whose exact root and projection are known. The checker logically removes that
place from its owner for the duration of the loan. No caller code can observe
the owner while the exclusive access is live.

Mutation through a mutable binding of an exclusive borrowed value updates that
suspended place. On normal loan completion the final value is committed to the
owner. This is the same move-in/write-back mechanism as `var`, extended across a
named relation; it is not an aliasing store observable through another binding.

A trap has the same rule as existing `var`: no source program resumes to observe
a partial commit. Structured returns, including `?`, commit before control
continues. A host boundary that catches failure must represent it as a normal
`Result` if exclusive borrowed state is observable.

Examples:

```text
fn inspect(let text: String('a)) -> Int
fn edit(var text: unique String('a)) -> String('a)
fn forward(own text: unique String('a)) -> unique String('a)
```

The second signature opens or receives exclusive access, writes the final
`String` back, and returns a shared view of that final value. The owner remains
shared-loaned until the result's last use. The third consumes an existing
exclusive handle and returns the obligation; it does not consume the owner.

The convention still controls what happens to the exclusive handle:

| Signature fragment | Exclusive-handle behavior |
|---|---|
| `let x: unique T('a)` | immutable, non-escaping reborrow; the caller retains the exclusive obligation |
| `x: unique T('a)` | immutable call-scoped exclusive access; no unique handle is copied or returned |
| `var x: unique T('a)` | mutable exclusive access; final referent and handle state write back |
| `own x: unique T('a)` | transfer the affine access obligation; the callee may return it, move it again, or close it |

The default and `let` forms may return a shared `T('a)` derived from the input;
that suspends the caller's exclusive write capability until the returned shared
view dies. They may not return `unique T('a)`, because neither convention
transfers the exclusive obligation. `var` and `own` provide the required output
or transfer channel.

An owned mutable place can implicitly open `var x: unique T('a)`, because `var`
already names the source place and commit protocol. An owned `T` does not
implicitly satisfy `own x: unique T('a)`: that would make it unclear whether
`own` consumed the owner or only a freshly created handle. The latter requires
an existing exclusive borrowed value. These rules keep source consumption
visible while still allowing exclusive borrows to be first-class.

### 6. Composition with `unique`, `local unique`, and `frozen`

`unique` continues to describe exclusive access to the storage represented by
the qualified value. A lifetime determines whether that access owns the storage:

| Type | Contract |
|---|---|
| `unique T` | sole owning reference; returnable and reusable in place |
| `local unique T` | sole owning reference within this activation; cannot escape |
| `unique T('a)` | exclusive non-owning access tied to `'a` |
| `local unique T('a)` | exclusive non-owning access additionally confined to this activation |

`local unique T('a)` is not required merely because the value is borrowed. It
is useful when an optimizer contract intentionally forbids returning an access
even though `'a` would permit it.

`frozen T('a)` is a valid shared view of deeply immutable storage. It can be
freely shared within `'a`. `frozen unique T` and `frozen unique T('a)` are
rejected as contradictory access contracts rather than normalized by qualifier
order.

Uniqueness is shallow with respect to separately named borrowed owners. A
`unique Parser('input)` means the parser shell is unique; it does not grant
exclusive access to the string named by `'input`. Exclusive access to that
string must be represented by the field's own type and owner relation.

### 7. Materialization and ownership recovery

`.owned()` removes direct borrowed relations by copying the represented logical
value:

```text
fn copy(let text: String('a)) -> String:
    text.owned()
```

For a shared view, materialization may close the loan at that use. For an
exclusive view, `.owned()` produces an independent owned snapshot but does not
silently discard pending owner updates. The exclusive access must first commit
or be consumed by an operation whose access signature specifies the final owner
state.

Borrowed nominal aggregates use an explicit owned-companion conversion when
their owned shape differs. The compiler never erases lifetime arguments from a
nominal value and guesses an owned representation.

There is no general conversion from `T('a)` to `unique T('a)`. It requires a
proof that every other access to the covered owner projection is dead. Normal
mode may materialize a fresh `unique T`, but that changes the type to owned and
does not mutate the original owner. `mode opt` reports the missing proof.

### 8. Containers, projections, and multiple owners

The RFC preserves RFC-0112's owner-set and projection model. A lifetime-bearing
value carries compile-time facts for:

- one or more concrete owner roots;
- a projection path or checked range;
- shared or exclusive access kind;
- the symbolic relation positions exposed by its type; and
- open, transfer, suspension, commit, and close points.

An owned container may hold relation-bearing elements when its layout descriptor
retains every required root:

```text
List(Token('a))
```

A borrowed view of container storage uses a direct relation argument:

```text
List(Token, 'a)
```

Projection composes owner facts rather than minting a new root. Shared
projections may overlap. Exclusive projections may coexist only when the place
oracle proves disjointness. The first implementation proves static record/tuple
fields and distinct constant indices; dynamic ranges remain conservatively
overlapping until range facts can prove otherwise.

Joining branches unions possible owner roots. If a result can originate from
either of two owners under one symbolic relation, both remain loaned unless
point-sensitive propagation proves one path impossible at the use. This is a
precision issue, never permission to forget a possible owner.

### 9. Function values, traits, and reflection

Lifetime relations and access kinds are part of callable identity:

```text
fn(String('a)) -> String('a)
fn(let String('a)) -> String('a)
fn(var unique String('a)) -> String('a)
```

These are distinct function types. Direct calls, methods, UFCS calls, closures,
trait witnesses, existential adapters, and tail dispatchers transport the same
logical access envelope. An ascription or generated adapter that erases a
relation, shared/exclusive kind, `var` write-back, or `own` consumption is a type
error.

Reflection reports lifetime arguments as compile-time relation parameters. It
never reports owner addresses, projection pointers, or hidden retain roots.
Serialization and `Dynamic` require materialization unless their type explicitly
preserves every relation and access obligation.

### 10. Async, generators, closures, and capabilities

Shared and exclusive relation-bearing values may cross an ordinary call or a
non-escaping closure when the callable type preserves their relations.

The initial implementation does not carry a loan across `await` or `yield`, send
it through a channel, move it into a task, or capture it in an escaping closure.
These boundaries require materialization or a future scoped-concurrency contract.
Exclusive access is never held across suspension in the initial release.

Capabilities do not gain ordinary lifetime arguments. A host-backed byte view
must carry both a data relation and an unforgeable host lease. `Bytes('a)` can
describe the data only after the capability-specific API has authenticated and
transported that lease; it cannot manufacture or extend authority.

## Loan analysis

### Origins, loans, and places

The source lifetime `'a` is an **origin relation** in a callable contract. A
concrete borrow at a call or construction creates a **loan**:

```text
LoanFact {
    id,
    origin,
    kind: Shared | Exclusive,
    owner_root,
    projection,
    introduced_at,
}
```

The analysis also records point-indexed facts:

```text
origin_subset_at(sub, sup, point)
origin_live_at(origin, point)
loan_killed_at(loan, point)
place_invalidated_at(root, projection, access, point)
```

A loan is in scope at a point when it can reach a live origin through the
point-sensitive subset graph and has not been killed by a valid overwrite,
commit, or last use. A place invalidation is legal only when every reaching loan
is compatible with that access.

This follows the useful Polonius separation between origins and loans without
requiring its historical Datalog implementation. The point-indexed facts are the
contract; the worklist, graph, Datalog, or incremental solver is replaceable.

### Conflict table

For overlapping places:

| Existing live access | New shared read | New exclusive/write | Move/drop owner |
|---|---:|---:|---:|
| shared | allowed | rejected | rejected |
| exclusive | rejected, except a bounded reborrow | rejected | rejected |

Disjoint projections do not conflict when proven. Unknown overlap is treated as
overlap. A bounded shared reborrow from an exclusive access suspends the latter's
write capability until the shared reborrow closes.

### Precision stages

The implementation proceeds without changing the source model:

1. Preserve current straight-line last-use behavior and projection owner sets
   under the new syntax.
2. Add shared/exclusive access kinds and exact commit facts for named exclusive
   relations.
3. Compute origin subset reachability at branch points so conditional returns do
   not keep impossible loans alive on sibling paths.
4. Add loop fixpoints with overwrite kills, enabling lending-iterator and
   conditional-reborrow patterns that are sound under the access model.
5. Add disjoint dynamic-range reasoning only after corpus evidence justifies its
   compile-time cost.

A conservative prepass may select a control-flow component for precise solving,
but it may not itself reject a program. Rejection requires an incompatible loan
reaching the exact invalidation point. Bodies with no relation-bearing values
stay on the existing cheap path.

### Diagnostics

Every rejection names:

- the owner and projection;
- where shared or exclusive access opened;
- the lifetime-bearing value keeping it live;
- the conflicting read, write, move, write-back, escape, or suspension;
- the relevant final use or branch when known; and
- a repair: shorten use, preserve the relation, reborrow, call `.owned()`, or
  move the mutation after the loan closes.

Diagnostics use source types such as `String('a)`, never internal `View`, origin
numbers, hidden root locals, or solver edges.

## Representation and lowering

Shared and exclusive lifetime arguments have no value-level representation.
Lowering erases them only after consuming checked facts that identify the logical
payload, owner roots, projections, and access obligations.

For shared borrows, compiled lowering retains each distinct linear-memory owner
root until the checked last use, as RFC-0083/RFC-0112 do today. It never applies
owning RC operations to a projection address.

For exclusive borrows, lowering may use direct caller storage only when the
checked-place and layout proofs permit it. Otherwise the semantic reference
implementation is copy-in, exclusive local mutation, and atomic copy-back on
structured completion. Both implementations have the same observable value and
write-back behavior. `mode opt` rejects the fallback when the signature promises
no-copy exclusive access.

The interpreter implements the same logical access envelope. It may materialize
shared views. For exclusive views it may use copy-in/copy-back, but it must
consume the same loan facts and commit at the same structured points as compiled
Wasm. Differential tests compare final values, write-backs, traps, and rejected
programs; pointer identity remains unobservable.

Root retain/drop and exclusive commit cleanup must cover fallthrough, explicit
`return`, `?`, branch exits, loop exits, and every generated adapter. A trapped VM
is terminal and cannot expose a partially committed exclusive access.

## Migration

Acceptance triggers a single source migration:

| Old spelling | New spelling |
|---|---|
| `let('a) String` | `String('a)` |
| `View(String, 'a)` | `String('a)` |
| `let('a) List(Int)` | `List(Int, 'a)` |
| `View(List(Int), 'a)` | `List(Int, 'a)` |
| `View(a, 'a)` | `a('a)` |
| `type Parser('a)` | unchanged |

`witchy migrate borrowed-relations <paths...>` parses old source and rewrites
types through the AST, including nested generics, function types, quoted types,
generated source fixtures, and documentation examples. It is a migration tool,
not a compatibility parser mode.

The compiler, formatter, reflection vocabulary, `meta.type_*` builders,
highlighter, language server, stdlib docs, book, spec, examples, and differential
fixtures change in the same cut. `View` and type-position `let('a)` then produce
targeted diagnostics showing the new spelling. They are not accepted aliases and
do not survive formatting.

Serialized compiler metadata and cached modules bump their format version.
There is no on-disk compatibility shim. Package sources must declare a compiler
version that understands the new syntax or migrate before rebuilding.

RFC-0083 and RFC-0112 remain historical records of the implemented design. On
acceptance, their status metadata receives the normal supersession note; their
bodies are not rewritten.

## Implementation plan

### Phase 0: freeze the contract and corpus

- Add this RFC's signature matrix as parser-independent type-model tests.
- Capture every shipped RFC-0083/RFC-0112 positive and negative fixture.
- Add conditional-return, lending-iterator, exclusive reborrow, and branch/loop
  false-positive cases before changing syntax.
- Record current checker time, fact counts, root counters, allocations, and
  parser/iterator throughput on the pinned borrow corpus.

### Phase 1: syntax and kind model

- Parse lifetime arguments through the ordinary type-argument loop.
- Remove the parser branches for `let('a) T` and `View(T, 'a)`.
- Represent lifetime arguments as relation-kind arguments rather than a
  `TypeQual::Borrow` surface artifact.
- Add declared-relation arity plus one optional direct-storage relation to type
  constructor metadata.
- Update aliases, formatter, linker, type resolution, quotations, reflection,
  derived code, highlighter, LSP, and diagnostics.
- Ship and test the AST-based migration command.

Phase 1 must preserve current shared-borrow acceptance and generated code exactly
apart from metadata/version changes. Matched counters prove no payload
materialization or root-lifecycle regression.

### Phase 2: typed access envelope

- Extend RFC-0110 access identities with `Shared` versus `Exclusive` relation
  access and direct-storage relation positions.
- Preserve these through direct calls, methods, UFCS, traits, closures, function
  values, aliases, and tail dispatch.
- Define coercions from owned places and reborrows from relation-bearing values.
- Reject every erasing ascription or adapter with source-level diagnostics.

### Phase 3: exclusive borrowed access

- Extend checked-place facts with exclusive open, suspend, reborrow, commit, and
  close events.
- Reuse RFC-0087's overlap and structured-write-back rules.
- Implement the copy-in/copy-back semantic oracle in the interpreter and forced-
  copy Wasm mode.
- Implement direct-storage lowering only from the same checked facts.
- Add `unique T('a)` coverage for `let`, default, `var`, and `own` conventions,
  including valid moves/returns and invalid overlapping access.

### Phase 4: flow-sensitive precision

- Introduce the origin/loan subset graph at CFG points.
- Handle conditional returns and path-specific kills.
- Add loop fixpoints and overwrite-aware reborrows.
- Keep solver selection local to functions with candidate conflicts.
- Compare diagnostics and acceptance against the frozen corpus and formal model.

### Phase 5: documentation and landing

- Update `spec/language.md`, `spec/performance.md`, the book's ownership and
  performance chapters, and generated stdlib documentation.
- Include runnable interpreter/Wasm examples for shared views, borrowed nominal
  values, exclusive write-back, materialization, and diagnostics.
- Run focused path-selected checks on each implementation slice and land slices
  through the merge queue; the queue serializes gates, not implementation.
- Mark this RFC implemented only when every acceptance criterion below has
  current evidence on `master`.

## Verification and acceptance criteria

1. `T('a)` parses, formats, kind-checks, aliases, quotes, reflects, resolves, and
   migrates uniformly for built-ins, type variables, nominal types, nested
   generics, fields, parameters, results, and function types.
2. Every old shared-borrow fixture has an AST-migrated equivalent with identical
   checker acceptance, owner sets, interpreter result, Wasm result, root
   retain/drop balance, and materialization counters.
3. `T`, `T('a)`, `unique T`, and `unique T('a)` remain distinct in type identity
   and every callable path preserves conventions, relations, access kinds,
   write-backs, and ownership state.
4. Shared borrows permit overlapping reads and reject owner mutation, move,
   `var` write-back, consumption, and relation-erasing escape until last use.
5. Exclusive borrows reject every overlapping access, permit proven-disjoint
   projections, support bounded shared reborrows, and restore exclusive access
   only after every reborrow closes.
6. `var unique T('a)` commits the final value on fallthrough, explicit return,
   and `?`; `own unique T('a)` moves the access obligation without consuming the
   owner; both reject use after move and relation erasure.
7. Interpreter copy-in/copy-back, forced-copy Wasm, and direct-storage optimized
   Wasm produce identical final values and write-backs. Checked-heap, no-reuse,
   poison, and UAF tests prove no premature release, stale projection, double
   commit, or leaked root.
8. Conditional-return and lending-iterator fixtures accepted by the specified
   flow-sensitive model compile without weakening any negative test. Branch and
   loop diagnostics identify the exact reaching loan and conflicting point.
9. Borrowed aggregates and `List(B('a))` preserve exact owner sets through copy,
   overwrite, destructure, projection, iteration, return, and drop. Direct
   container views remain distinct from owned containers of borrowed elements.
10. Async, generator, task, channel, escaping-closure, `Dynamic`, serialization,
    existential, and host-capability boundaries either preserve every relation
    and lease explicitly or reject with a materialization/scoping remedy.
11. The migration command rewrites the complete repository with no old accepted
    spelling left, and a clean rebuild validates the bumped metadata format.
12. Borrow-free bodies show no material checker-time regression. The pinned
    borrow corpus publishes checker time, loan count, subset-edge count, peak
    memory, allocations, root operations, and execution throughput before and
    after each precision phase. No broad performance claim is made without
    matched evidence.

## Alternatives

### Keep `let('a) T` and `View(T, 'a)`

This is implemented and explicit. It also preserves the visible distinction
between a parameter-side borrow constructor and a result-side view. Rejected
because both spellings already normalize to one relation-bearing type, the input
form visually duplicates the `let` convention, and nominal aggregates already
use the proposed argument form.

### `let text: 'a T` and `-> 'a T`

This is compact and keeps the parameter convention in its existing position.
Rejected because prefix lifetime qualifiers compose less clearly with `unique`,
`local unique`, `frozen`, qualified names, and nested generic types. The
postfix-argument form also matches existing nominal lifetime declarations.

### `let('a) text: T`

This binds the lifetime next to the parameter convention and removes one `let`.
Rejected because lifetimes also occur in results, fields, aggregate arguments,
and function types where there is no binding convention. It would require a
second spelling elsewhere.

### Keep `View(T, 'a)` everywhere

Uniform and unambiguous:

```text
fn first(let text: View(String, 'a)) -> View(String, 'a)
```

Rejected because it presents a logical `String` as a separate nominal wrapper,
does not match `Parser('a)`, and makes nested type heads harder to read. `View`
is an implementation-neutral concept, but it need not be a source type name.

### Treat lifetime arguments as ordinary generics

Rejected. Lifetimes have a separate kind, do not exist at runtime, do not by
themselves trigger monomorphization, and participate in owner-loan checking.
Uniform syntax does not require uniform runtime semantics across kinds.

### Shared borrows only

This is the current contract and remains a viable smaller implementation cut.
Rejected as the final design because `unique T('a)` is meaningful, composes with
existing access qualifiers, and allows exclusive zero-copy APIs without adding a
second `&mut`-style syntax. The staged plan still lands shared syntax before
exclusive semantics.

### Infer every relation

Rejected for public and multi-owner APIs. Inference should determine concrete
loan duration and owner sets at calls, but signatures must state which output
depends on which input so function values, traits, modules, and reviewers see the
contract.

## Drawbacks

- `T('a)` resembles an ordinary generic instantiation even though `'a` has a
  different kind and no runtime representation.
- Types with declared internal relations can also carry one direct shell
  relation, so `Parser('input, 'parser)` requires explanation.
- `List(T, 'a)` and `List(T('a))` are intentionally different and easy to
  transpose until the distinction becomes familiar.
- Exclusive borrowed access expands the language beyond RFC-0083/RFC-0112's
  shared-only model and requires commit semantics, affine checking, and a
  stronger differential oracle.
- The clean syntax migration is source- and metadata-breaking.
- Flow-sensitive loan solving can regress compile time on very large functions;
  the implementation needs cardinality metrics and a cheap borrow-free path.
- Normal source can receive a lifetime diagnostic after calling an opt API even
  when it did not spell a lifetime locally. Diagnostics must name the API and
  concrete owner rather than expose solver terminology.

## Prior art

- Rust lifetimes demonstrate kinded, erased relations in type signatures.
- Rust's [2026 Polonius goal](https://rust-lang.github.io/rust-project-goals/2026/polonius.html)
  and [nightly alpha announcement](https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nighty/)
  motivate separating origins from loans, point-sensitive subset propagation,
  conditional-return acceptance, lending iterators, formal modeling, and
  measured rollout. Witchy adopts those analytical lessons, not Rust reference
  syntax or its implementation wholesale.
- [Implementation Strategies for Mutable Value
  Semantics](../external-refs/mutable-value-semantics-2022/notes.md) supplies the
  read/exclusive/consume access model behind Witchy's conventions and makes an
  exclusive borrow compatible with value semantics when no alias can observe
  intermediate mutation.
- [Counting Immutable
  Beans](../external-refs/counting-immutable-beans-2019/notes.md) demonstrates
  borrowed-reference inference as an RC-traffic optimization.
- Swift borrowing, Hylo access lifetimes, Vale regions, Cyclone regions, and
  lending iterators provide additional evidence that access mode, ownership,
  and validity relation should remain separate concepts.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->
