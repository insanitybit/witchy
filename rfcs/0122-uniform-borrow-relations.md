---
rfc: 0122
title: "First-class references and explicit lifetimes"
status: proposed
created: 2026-08-13
updated: 2026-08-13
tracking: "Long-term reference model; syntax replacement for RFC-0083 and semantic completion of shared and exclusive borrowing"
predecessors:
  - "[0026](0026-unique-qualifier.md) (`unique` and `local unique` ownership contracts)"
  - "[0083](0083-opt-mode-lifetimes.md) (`let('a) T`, `View(T, 'a)`, and shared owner loans)"
  - "[0087](0087-fused-mutators.md) (`var` as exclusive move-in/write-back access)"
  - "[0110](0110-opt-ownership-access-abi.md) (one typed access envelope for every callable)"
  - "[0112](0112-borrowed-aggregate-types.md) (lifetime-bearing nominal values and projection-aware loans)"
---

# RFC-0122: First-class references and explicit lifetimes

## Summary

Witchy gains explicit shared and exclusive reference types:

```text
&'a T
&'a mut T
```

An ampersand always means a reference. The lifetime states how long the
reference may remain valid. `mut` states that the reference grants exclusive
mutation of its referent.

```text
fn first(text: &'a String) -> &'a String:
    text

fn normalize(text: &'a mut String) -> &'a String:
    *text = text.trim().to_lower()
    text
```

Borrow expressions create references from stable places:

```text
let view = &text
let editable = &mut text

let first_view = first(&text)
let normalized = normalize(&mut text)
```

The type system therefore has four independent concepts:

```text
T                  owned logical value
unique T           uniquely owned logical value
&'a T              shared reference valid for 'a
&'a mut T          exclusive reference valid for 'a
```

`let`, `var`, and `own` remain parameter conventions. They describe how a
parameter value crosses a call boundary. They do not spell references:

```text
let x: T            non-escaping read access to a value argument
var x: T            move-in/write-back value argument
own x: T            consumed value argument
x: &'a T            first-class shared reference argument
x: &'a mut T        first-class exclusive reference argument
```

This RFC replaces all ordinary-borrow spellings based on `let('a)`,
`var('a)`, `View(T, 'a)`, and direct lifetime lifting such as `T('a)`. Nominal
types may still declare lifetime parameters, but their borrowed fields use
reference types:

```text
type Parser('input):
    input: &'input String
    offset: Int
```

The distinction between a nominal lifetime argument and a reference is now
visible. `Parser('input)` is an owned parser containing a reference;
`&'parser Parser('input)` is a reference to that parser.

The migration is intentionally breaking. Legacy forms remain only as targeted
diagnostics and are not aliases.

## Decision principles

### Borrowing is a type-level capability

A declaration should say directly whether it accepts an owned value, a shared
reference, or an exclusive reference:

```text
fn consume(text: String) -> Int
fn inspect(text: &'a String) -> Int
fn edit(text: &'a mut String) -> Nil
```

The previous proposal split this contract between a parameter convention and a
result type:

```text
fn first(let('a) text: String) -> String('a)
```

That distinction reflected compiler mechanics, not a useful source-level type
distinction. The accepted form uses one reference type in both positions:

```text
fn first(text: &'a String) -> &'a String
```

### Shared and exclusive references are first-class

Both reference kinds may be passed, returned, placed in aggregates, projected,
reborrowed, and captured by a non-escaping closure while their lifetimes permit.

Shared references are copyable. Exclusive references are affine: they cannot be
copied, but they may be moved or reborrowed. The owner is inaccessible through
any competing path while an exclusive reference is live.

### Exclusive access is not uniqueness

`unique T` describes owned storage and supports in-place optimization.
`&'a mut T` describes temporary exclusive access to a logical place. A mutable
borrow may require a repair copy before it opens when the runtime allocation is
not unique. The reference remains exclusive even when such a repair is needed.

The source type of the reference does not change:

```text
var text: String = source
let editable = &mut text
```

Borrowing from a `unique T` place can prove that no repair is necessary. That
proof is attached to borrow creation, not encoded as `unique &'a mut T`.

### `var` and `&mut` remain different

`var` is Witchy's value-oriented move-in/write-back convention. The callee owns
a local logical value and commits its final value on structured completion.

`&mut` is an explicit reference to an existing place. Mutation occurs through
that reference, and the reference may outlive the call that received it. It can
therefore express stored exclusive access and lending APIs that `var` cannot.

### Public relations are explicit; concrete duration is inferred

Public signatures name every lifetime relation that reaches a result or stored
field. Borrow expressions do not spell a lifetime. The checker infers concrete
roots, reborrow duration, branches, overwrites, and last uses.

### Syntax does not promise a raw pointer

`&` specifies source semantics, not physical representation. A reference may be
lowered to a pointer, an owner root plus projection, a typed GC reference, or a
copy-in/write-back shadow. Both backends consume the same checked reference
facts and must agree observably.

## Surface syntax

### Reference types

The grammar adds:

```text
reference-type = "&" lifetime ["mut"] type
lifetime       = "'" identifier
```

Examples:

```text
&'a String
&'a mut String
&'items List(Token)
&'outer Parser('input)
List(&'input Token)
Option(&'a mut Buffer)
```

Every reference type written in a declaration has an explicit lifetime. Local
borrow expressions infer an internal lifetime and normally need no annotation.
This RFC does not add lifetime elision or `'static`.

`mut` is contextual after a reference lifetime. Witchy uses `mut` rather than
`var` here because the two words describe different operations: `mut` grants
mutation through a referent, while `var` writes a parameter value back to its
caller slot.

### Borrow expressions

The grammar adds:

```text
borrow-expression = "&" place-expression
                  | "&" "mut" place-expression
deref-expression  = "*" expression
```

The operand must be a stable place: a local binding, parameter place,
dereference, field, tuple element, or checked index/range projection. Borrowing
an unbound temporary is rejected rather than receiving a temporary-lifetime
extension rule:

```text
let bad = &make_string()       // error: bind the owner before borrowing it
```

Borrow expressions do not contain lifetime names:

```text
let view = &text
let editable = &mut text
```

The checker relates each inferred local lifetime to named lifetimes when a
reference crosses a declared boundary.

`&mut` additionally requires a mutable place. It establishes uniquely mutable
runtime representation before exposing the reference. Normal mode may perform
one repair copy while opening the borrow; `mode opt` rejects that borrow when
the required no-copy proof is unavailable. Mutation through an established
exclusive reference never performs a later alias-repair copy.

### Dereference and projection

`*reference` denotes the referent place. Reading through either reference kind
is allowed. Assignment through a shared reference is rejected; assignment
through an exclusive reference is allowed:

```text
fn clear(text: &'a mut String) -> Nil:
    *text = ""
```

Field access, indexing, and method lookup transparently project through a
reference when the operation is unambiguous:

```text
fn rename(account: &'a mut Account) -> Nil:
    account.name = account.name.trim()
```

This is projection sugar over `(*account).name`; it does not copy, materialize,
or widen access. Explicit `*` remains available when the whole referent is
needed.

### Shared references

`&'a T` grants read-only access to `T` for `'a`. It is copyable under Witchy's
value semantics because each copy carries the same owner obligation and grants
no mutation.

Any number of overlapping shared references may coexist. While any shared
reference is live, overlapping mutation, `var` access, mutable borrowing,
consumption, reassignment, move, or drop of the owner is rejected.

```text
var text = "hello"
let left = &text
let right = &text
console.print(first(left))
console.print(first(right))
text = "done"                 // accepted after both final uses
```

Passing a shared reference under the default convention copies its reference
value. `own` may consume one reference handle, but consuming it does not consume
the owner.

### Exclusive references

`&'a mut T` grants read and write access to `T` for `'a`. It excludes every
overlapping shared or exclusive access, including access through the original
owner binding.

```text
var text = "hello"
let editable = &mut text
editable.push("!")
console.print(text)            // error: editable keeps text exclusively borrowed
editable.push("?")             // later use keeps the exclusive loan live
```

Exclusive references are affine. Assignment, aggregate construction, closure
capture, and `own` parameters may move them, but no operation may copy them.
After a move, the old binding is unavailable.

Passing an exclusive reference under the default parameter convention creates
an exclusive reborrow for the call. It does not copy or permanently consume the
outer reference:

```text
fn append_mark(text: &'call mut String) -> Nil:
    text.push("!")

fn twice(text: &'outer mut String) -> Nil:
    append_mark(text)
    append_mark(text)
```

If a called function returns a reference derived from that reborrow, the outer
reference remains unavailable until the returned reference's final use.
`own` on an exclusive-reference parameter consumes the handle instead of
reborrowing it.

### Mutable-to-shared reborrowing

An exclusive reference may be shortened and reborrowed as shared:

```text
fn finish(text: &'a mut String) -> &'a String:
    text.trim_in_place()
    text
```

The return coerces `&'a mut String` to `&'a String`. The exclusive capability is
not recoverable from that shared result. A shorter shared reborrow may end while
the exclusive reference remains usable afterward:

```text
fn inspect_then_edit(text: &'outer mut String) -> Nil:
    let view = &*text
    console_free_inspect(view)
    text.push("!")
```

Exclusive projection may also return a mutable reference:

```text
fn name(account: &'a mut Account) -> &'a mut String:
    &mut account.name
```

### Nominal lifetime parameters

Nominal declarations retain lifetime parameters only for relations stored in
their fields:

```text
type Parser('input):
    input: &'input String
    offset: Int

type PairView(t, 'left, 'right):
    first: &'left t
    second: &'right t
```

`Parser('input)` is an owned parser shell. Borrowing the shell uses the ordinary
reference constructor instead of adding an undeclared trailing relation:

```text
fn inspect(parser: &'parser Parser('input)) -> Int:
    parser.offset

fn shell(parser: &'parser Parser('input)) -> &'parser Parser('input):
    parser
```

Construction must prove every declared lifetime from reference-bearing fields.
An unused declared lifetime is an error. Variance is derived from field uses.

### Containers and nested references

Reference placement states exactly which storage is borrowed:

```text
List(&'input Token)             // owned list containing shared references
&'list List(Token)              // shared reference to list storage
&'list mut List(Token)          // exclusive reference to list storage
&'list List(&'input Token)      // list reference whose elements borrow input
```

Tuples, structural records, nominal aggregates, and supported containers carry
the union of their fields' owner obligations. Copying an aggregate containing
only shared references copies those obligations. An aggregate containing an
exclusive reference is affine.

### Generic references

References apply directly to type variables without relation lifting:

```text
fn identity(value: &'a t) -> &'a t:
    value

fn swap(left: &'a mut t, right: &'b mut t) -> Nil:
    let temporary = (*left).owned()
    *left = (*right).owned()
    *right = temporary
```

`&'a Int` is legal even when its runtime projection can be represented without
an owner root. The API relation remains part of callable identity.

Capabilities and host leases are not generally referenceable in the initial
implementation. A host-backed reference must be introduced by a
capability-specific API that transports both its data relation and its
unforgeable lease. `&'a Dir` does not manufacture or widen authority.

### Function types

Reference kinds and lifetime relations are part of callable identity:

```text
fn(String) -> String
fn(let String) -> Int
fn(&'a String) -> &'a String
fn(&'a mut String) -> &'a String
```

Lifetime names are alpha-normalized inside each callable. Renaming `'a` to
`'input` does not change the type; changing which input bounds a result does.

Direct calls, methods, UFCS calls, closures, trait witnesses, existential
adapters, generated wrappers, and proper-tail dispatch preserve the complete
reference contract. A cast or adapter that erases a reference kind, lifetime,
affinity, parameter convention, or ownership requirement is rejected.

## Lifetime binding and owner relations

### Implicit quantification

Lifetime names in callable types are implicitly universally quantified. Every
result lifetime must be reachable from an input reference with the same name or
from a declared relation in an input aggregate:

```text
fn forward(value: &'a String) -> &'a String:
    value

fn bad(value: String) -> &'a String:
    &value                         // error: 'a has no surviving input owner
```

No source `outlives` clause is introduced in this RFC. Borrow creation,
reborrowing, input relations, result flow, and aggregate construction generate
the required subset constraints.

### Shortening and variance

A reference valid for a longer lifetime may be reborrowed for a shorter one:

```text
fn reborrow(value: &'a String) -> &'a String:
    value
```

At each invocation, `'a` may be instantiated to a duration shorter than the
input reference's remaining validity. No separately named output lifetime is
needed. Shared references are covariant in their lifetime and target according
to the target's derived variance. Exclusive references are covariant in their
lifetime but invariant in their target type, because writing through a coerced
mutable reference could otherwise violate the original place type. Nominal
lifetime parameters derive variance from their field positions; unresolved
positions are invariant until proven safe.

### Independent inputs

Distinct names state an exact dependency:

```text
fn left(left: &'left String, right: &'right String) -> &'left String:
    left
```

The result keeps only `left` loaned.

### Several possible owners

The same lifetime may appear on several shared inputs:

```text
fn choose(left: &'a String, right: &'a String, pick: Bool) -> &'a String:
    if pick: left else: right
```

`'a` is instantiated to a duration valid for every possible result owner. The
result's point-sensitive owner set contains the roots that can reach that
program point. A conservative join may retain both roots but may not forget
either.

Two exclusive inputs may share a lifetime name, but their argument places must
still be proven disjoint:

```text
fn edit_pair(left: &'a mut String, right: &'a mut String) -> Nil
```

Unknown overlap is overlap. Separate lifetime names improve diagnostics but do
not replace place-disjointness checking.

### Owner sets and projections

Each reference value has an `OwnerSet` containing:

- one or more stable owner roots;
- a projection path or checked range;
- shared or exclusive access kind;
- open, reborrow, transfer, and close points; and
- symbolic lifetime positions exposed by its type.

Projection composes paths and ranges with the root. It never treats an interior
address as an owning RC base. Shared projections may overlap. Exclusive
projections must be disjoint from every other live access.

Joins union possible roots. Destructuring transfers each field's owner set.
Copying a shared-reference aggregate creates another obligation. Moving an
exclusive-reference aggregate transfers its obligations and kills the source.

## Interaction with ownership features

### `unique`

`unique T` remains an owned-value contract. It says that the logical value has
the only owning reference and may be reused in place:

```text
fn parse(input: &'a unique Bytes) -> Parser('a)    // rejected: see below
```

Uniqueness qualifiers do not belong inside reference targets. Shared access
cannot promise that it is the only access, and exclusive access already grants
the relevant temporary exclusivity. The supported forms are:

```text
fn parse(input: &'a Bytes) -> Parser('a)
fn normalize(text: &'a mut String) -> &'a String

var bytes: unique Bytes = source
let parser = parse(&bytes)                          // borrow creation retains uniqueness fact
```

`unique &'a T`, `&'a unique T`, `unique &'a mut T`, and corresponding
`local unique` forms are rejected as category errors. The optimizer records
whether the owner was unique when the borrow opened. In normal mode, an
exclusive borrow may repair a non-unique runtime allocation before mutation.
In `mode opt`, a no-copy requirement must be proven at borrow creation and the
diagnostic points to the owner provenance.

### `local unique`

`local unique T` remains owned storage confined to one activation. It may be
borrowed locally, but no resulting reference may escape that activation:

```text
var value: local unique Buffer = make_buffer()
let view = &value
inspect(view)
```

An escaping return, closure, aggregate, or task is rejected even if its source
type names a lifetime. A lifetime cannot extend the owner's confinement.

### `frozen`

`frozen T` is deeply immutable owned storage. It may produce shared references
that preserve the frozen guarantee when useful:

```text
fn view(text: &'a frozen String) -> &'a frozen String:
    text
```

`&mut` of frozen storage is rejected. `frozen &'a T` is rejected because
`frozen` qualifies owned storage, not a reference handle.

### `own`

`own` consumes its argument value:

```text
fn digest(own bytes: Bytes) -> Digest
fn count(own parser: Parser('a)) -> Int
fn consume_view(own text: &'a String) -> Int
fn store_edit(own text: &'a mut String) -> EditGuard('a)
```

Consuming a shared reference kills that handle but not its owner. Consuming an
exclusive reference transfers its exclusive capability. There is no `own('a)`;
lifetimes belong to reference and nominal types.

An owned value consumed by `own` cannot directly produce a reference result,
because no caller-owned root survives to bound it. Return an owned result or an
owned object that packages both storage and projection.

### `let` and `var` on reference values

Parameter conventions remain orthogonal to reference types:

```text
fn inspect_handle(let view: &'a String) -> Int
fn replace_handle(var slot: &'a String, replacement: &'a String) -> Nil
```

`let` borrows the reference handle without extending its lifetime. `var` writes
a final handle back to the caller slot; it does not mutate the referent. A
`var` parameter of exclusive-reference type may replace or return the affine
handle but must preserve exclusivity and lifetime constraints.

These forms are valid but uncommon. Ordinary reference parameters are the
canonical API surface.

## Materialization and recovery

`.owned()` reads through a shared or exclusive reference and produces an
independent owned logical value:

```text
fn copy(view: &'a String) -> String:
    view.owned()
```

Materialization can end that handle's loan at its final use. Borrowed nominal
aggregates use an explicit owned-companion conversion when their owned shape
differs. The compiler never drops references or lifetime arguments and guesses
an owned representation.

There is no conversion from `&'a T` or `&'a mut T` to `unique T`. A materialized
copy may subsequently satisfy a uniqueness proof, but it does not recover
ownership of the original referent.

## Mutable references and observability

An exclusive borrow opens in this order:

1. evaluate the owner place and projection coordinates once;
2. reject any overlapping live access;
3. repair runtime sharing when normal-mode value semantics require it;
4. create an affine reference to the resulting logical place; and
5. keep the owner and every competing path inaccessible until the reference
   closes or moves.

Mutation through `&mut` changes the referenced logical place. A reborrow may
shorten access but cannot widen it. Moving the reference transfers its open
exclusive loan. Converting it to shared access permanently relinquishes
exclusive capability through that handle.

No pointer identity is observable. Equality, reflection, pattern matching, and
formatting see logical values. A trap is terminal on both backends, so partially
performed mutations are not observable after resumption. Structured `return`
and `?` preserve mutations already performed through the reference.

`var` retains its RFC-0087 atomic multi-write-back rules. This RFC does not
redefine `var` in terms of `&mut`, even when optimized lowering shares machinery.

## Flow-sensitive reference analysis

### Origins and loans

A named lifetime is an origin in a callable contract. Each borrow or reborrow
creates a concrete loan:

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

The checker records point-indexed facts:

```text
origin_subset_at(sub, sup, point)
origin_live_at(origin, point)
loan_killed_at(loan, point)
place_accessed_at(root, projection, access, point)
reference_moved_at(reference, point)
reference_reborrowed_at(parent, child, point)
```

A loan is live when it can reach a live origin through the point-sensitive
subset graph and has not been killed by final use, overwrite, materialization,
or transfer. An access is accepted only when no incompatible loan reaches that
exact point.

This adopts Polonius's useful separation of origins from loans without requiring
its historical Datalog implementation. Point-indexed facts are the semantic
interface. The solver may use a graph worklist, localized reachability,
incremental computation, or another measured implementation.

### Conflict rules

For overlapping places:

| Existing access | Shared read | Shared borrow | Exclusive borrow | Owner mutation or move |
|---|---:|---:|---:|---:|
| shared loan | allowed | allowed | rejected | rejected |
| exclusive loan | through that reference | reborrow only | reborrow only | through that reference |

Access through an exclusive reference is part of its loan, not competing owner
access. An exclusive reborrow temporarily suspends use of its parent reference.
Unknown projection overlap is treated as overlap. Static record and tuple
fields and distinct constant indices form the initial disjointness proof set.

### Conditional returns

Origins are propagated at CFG points so impossible sibling-path loans do not
remain live:

```text
fn get_or_insert(table: &'a mut Dict(String, Value), key: String) -> &'a Value:
    match table.get_ref(&key):
        Some(value) -> value
        None ->
            table.insert(key, default_value())
            table.get_ref(&key).unwrap()
```

The mutation on the missing path is legal because the existing-value loan from
the other branch cannot reach it. A whole-function owner union would reject
this sound program.

### Lending iteration

First-class exclusive references express lending directly:

```text
trait LendingIterator(item):
    fn next(self: &'next mut Self) -> Option(&'next item)
```

Each item is a shared reborrow of the iterator state. Calling `next` again
requires the prior item to be dead or materialized. Implementations may also
return `Option(&'next mut item)` when exclusive element access is intended.

Trait witnesses, dynamic dispatch, and function values must preserve the
relation between the mutable receiver reborrow and the yielded item.

### Precision stages

Analysis precision advances without changing syntax:

1. preserve current straight-line last-use and projection owner sets;
2. represent explicit shared references and reborrows;
3. add affine exclusive references, parent suspension, and move tracking;
4. compute path-sensitive origin subsets for conditional returns;
5. compute loop fixpoints with overwrite kills for lending iteration; and
6. add dynamic disjoint-range proofs only when corpus evidence justifies cost.

A cheap pass may select conflict-relevant CFG components for precise solving,
but it may not reject a program. Rejection requires a concrete incompatible
loan at the invalidating point. Reference-free bodies stay on the existing
cheap path.

## Escapes and boundaries

A shared reference may be passed, returned, copied into a
relation-preserving aggregate, projected, destructured, and captured by a
proven non-escaping closure within its owner lifetime.

An exclusive reference may perform the same operations only by affine move or
reborrow. It may not be copied or aliased.

Neither reference kind may be:

- stored in a type that erases its lifetime or access kind;
- converted to `Dynamic` or an owned existential without materialization;
- serialized as an address-bearing representation;
- sent through a channel or isolated worker;
- captured by an escaping closure or task; or
- held live across `await` or `yield` in the initial implementation.

Synchronous calls before or after suspension remain valid. A future scoped
concurrency or coroutine RFC may relax these restrictions only with explicit
owner, cancellation, and cleanup contracts.

Host references require lease-bearing APIs. A lifetime cannot widen a grant or
keep capability authority alive by itself.

## Representation and lowering

Lifetimes have no runtime payload. Checked reference values retain:

- owner-root identities;
- projection descriptors;
- shared or exclusive access kind;
- affine state for exclusive references;
- lifetime positions in callable and nominal types; and
- representation-specific root, repair, write-back, or lease obligations.

Compiled shared references retain each distinct linear-memory root until the
checked final use. Typed GC references remain typed. Lowering never emits an
owning retain or drop on an interior projection.

An optimized exclusive reference may be a direct pointer or checked projection.
The interpreter and forced-copy Wasm path may instead use a logical shadow with
a write-back chain, provided all reads, mutations, reborrows, structured exits,
and drops agree with direct-place semantics. Parent references remain suspended
while a reborrow is live in every representation.

The existing RFC-0110 access envelope gains explicit reference inputs and owner
relations:

```text
(explicit arguments, value ownership inputs, reference access inputs)
    -> (ordinary result, var write-backs, ownership outputs, result references)
```

Direct-storage lowering is legal only when checked-place, uniqueness, overlap,
escape, layout, and cleanup proofs hold. Otherwise normal mode may repair or use
a shadow. `mode opt` rejects a missing no-copy proof when the API or local
optimization contract requires one.

The interpreter is the semantic oracle and compiled Wasm must agree on values,
owner mutations, `var` write-backs, traps, accepted programs, and rejection
boundaries. Cleanup covers fallthrough, explicit return, `?`, branches, loops,
reborrow end, affine move, and generated adapters.

## Diagnostics

Reference diagnostics name:

- the owner and overlapping projection;
- the borrow expression and reference kind;
- the value or reborrow keeping the loan live;
- the conflicting read, mutation, borrow, write-back, move, drop, erasure, or
  suspension;
- the path-sensitive final use when available; and
- a repair such as shortening use, reborrowing, materializing with `.owned()`,
  splitting places, or moving mutation after the loan closes.

Diagnostics render `&'a T` and `&'a mut T`, never internal `View`, origin
numbers, hidden roots, capacity tokens, shadow cells, or solver edges.

Targeted migration diagnostics include:

```text
`let('a) String` is retired; write the reference type `&'a String`

`View(String, 'a)` is retired; write `&'a String`

`String('a)` is not an ordinary borrowed String; write `&'a String`

`var('a) text: String` is retired; write `text: &'a mut String`
and pass a mutable borrow with `&mut text`
```

The `String('a)` diagnostic is emitted only when `String` declares no nominal
lifetime parameter. Declared forms such as `Parser('a)` remain valid.

## Migration

Acceptance triggers one AST-based migration:

| Legacy declaration | Reference-model declaration |
|---|---|
| `let text: let('a) String` | `text: &'a String` |
| `text: let('a) String` | `text: &'a String` |
| `let('a) text: String` | `text: &'a String` |
| `var('a) text: String` | `text: &'a mut String` |
| `View(String, 'a)` | `&'a String` |
| `String('a)` where `String` declares no lifetime | `&'a String` |
| `List(Token('a))` | `List(&'a Token)` |
| `List(Token, 'a)` direct storage relation | `&'a List(Token)` |
| `type Parser('a)` | unchanged |
| field `input: String('a)` | `input: &'a String` |

Call sites also become explicit:

```text
first(text)          -> first(&text)
normalize(text)      -> normalize(&mut text)
forward(view)        -> forward(view)
```

`witchy migrate references <paths...>` uses resolved AST information to
distinguish nominal lifetime arguments from retired direct relation lifting. It
rewrites declarations, nested types, function types, aliases, borrow-producing
calls, quoted types, generated fixtures, and documentation examples. It reports
rather than guesses when owner mutability, overload resolution, macro output, or
legacy convention combinations are ambiguous.

The compiler, formatter, reflection vocabulary, `meta.type_*` builders,
highlighter, language server, stdlib docs, book, spec, examples, differential
fixtures, serialized metadata, and cached modules change in the same cut. The
metadata format version is bumped. No compatibility shim survives the cut.

RFC-0083 and RFC-0112 remain frozen historical records. Once this RFC is
accepted and implemented, their metadata receives a supersession note; their
bodies are not rewritten.

## Implementation plan

### Phase 0: executable semantic model and ledger

- Freeze the complete RFC-0083 and RFC-0112 positive and negative corpus.
- Add explicit shared, exclusive, reborrow, affine-move, aggregate, conditional,
  lending, erasure, and boundary fixtures before changing syntax.
- Build a small reference semantics model over owner roots, projections,
  reference moves, reborrows, and logical write-back shadows.
- Use interpreter behavior as the language oracle and the model as the loan
  checker oracle for new exclusive cases.
- Record an acceptance ledger divided into syntax, type checking, callable
  identity, interpreter, Wasm, tooling, migration, docs, and evidence tracks.
- Record checker time, fact counts, peak memory, root counters, repair copies,
  allocations, and parser/iterator throughput on a pinned corpus.

### Phase 1: syntax and checked types

- Parse `&'a T`, `&'a mut T`, `&place`, `&mut place`, and `*reference`.
- Add explicit shared/exclusive reference nodes to AST, checked types, runtime
  types, reflection, quotations, and callable identities.
- Preserve nominal lifetime arguments while deleting direct lifetime lifting.
- Derive copyability and affinity structurally for reference aggregates.
- Update aliases, formatter, linker, type resolution, derive expansion,
  highlighter, LSP, diagnostics, and metadata encoding.
- Add parser-independent signature tests for every valid and invalid form.

### Phase 2: shared-reference migration

- Lower explicit shared borrows through the existing RFC-0083/RFC-0112 owner
  root and projection path during the transition.
- Implement borrow expressions, stable-place checking, shared reborrowing,
  dereference, projection, copying, and materialization.
- Preserve reference relations through direct calls, methods, UFCS, closures,
  function values, traits, witnesses, generated adapters, and tail dispatch.
- Prove matched legacy and migrated fixtures have identical acceptance, owner
  sets, values, roots, and materialization counters.

### Phase 3: exclusive references

- Add affine state, exclusive place loans, parent-reference suspension,
  exclusive and shared reborrowing, and mutable-to-shared coercion.
- Define the interaction with owner mutability, `unique`, `local unique`,
  `frozen`, `let`, `var`, `own`, move, drop, and structured control flow.
- Implement interpreter and forced-copy Wasm shadow/write-back semantics first.
- Enable direct-place Wasm lowering only from the same checked facts.
- Cover root replacement, capacity-token change, projection mutation, multiple
  disjoint borrows, explicit return, `?`, and terminal traps.

### Phase 4: point-sensitive precision

- Introduce origin/loan subset reachability at CFG points.
- Handle conditional returns and sibling-path invalidations.
- Add loop fixpoints, overwrite kills, and lending iterator reborrows.
- Keep precise solving local to conflict-relevant components.
- Add dynamic disjoint-range proofs only after corpus evidence justifies cost.
- Validate every precision change against the semantic model and frozen negative
  corpus.

### Phase 5: migration and repository cut

- Implement `witchy migrate references` and review every ambiguity report.
- Rewrite the repository in one cut with no accepted legacy syntax.
- Update `spec/language.md`, `spec/performance.md`, ownership and performance
  book chapters, stdlib docs, reflection docs, and runnable examples.
- Update generated manifests, censuses, snapshots, and metadata with the slice
  that invalidates them.
- Land independently green syntax, type, interpreter, Wasm, tooling, tests,
  docs, and evidence slices through the merge queue while an integration branch
  keeps the migrated end-to-end path runnable.
- Mark this RFC implemented only when every acceptance criterion has current
  evidence on `master`.

## Acceptance criteria

1. `&'a T`, `&'a mut T`, `&place`, `&mut place`, and `*reference` parse,
   format, reflect, quote, highlight, and survive every compiler stage.
2. Reference types work uniformly for built-ins, type variables, nominal types,
   fields, tuples, nested containers, parameters, results, aliases, traits, and
   function types without direct `T('a)` lifting.
3. Nominal `Parser('a)` remains distinct from `&'a Parser`; the parser, kind
   checker, formatter, reflection, and migration tool preserve that distinction.
4. Every migrated RFC-0083/RFC-0112 fixture has matched acceptance, diagnostic
   intent, interpreter value, Wasm value, owner sets, root balance, and
   materialization counters.
5. Shared references copy and reborrow safely, permit overlapping reads, and
   reject overlapping mutation, exclusive borrow, `var` access, consumption,
   move, drop, and erasing escape until path-sensitive final use.
6. Exclusive references are affine, allow mutation through the referent, reject
   every competing overlap, suspend parent references during reborrow, and
   transfer all loan obligations on move.
7. Mutable-to-shared conversion relinquishes exclusive capability through the
   converted handle; shortening never lengthens an owner relation.
8. `unique T`, `local unique T`, and `frozen T` retain owned-storage meanings.
   Invalid qualifier/reference combinations receive category-specific
   diagnostics, and `&mut frozen T` is rejected.
9. `let`, `var`, and `own` remain distinct from reference kinds in callable
   identity. Applying them to a reference handle affects the handle, never
   silently changes referent access.
10. No cast, trait witness, closure, adapter, existential edge, or tail call
    erases lifetime, shared/exclusive kind, affinity, parameter convention, or
    ownership requirements.
11. Conditional-return and lending-iterator fixtures compile under
    point-sensitive analysis without weakening negative cases. Rejections name
    the exact reaching loan and conflict point.
12. Borrowed nominal aggregates, nested projections, multiple owner relations,
    shared-reference containers, and affine-reference containers preserve exact
    roots through copy or move, overwrite, destructure, iteration, return, and
    drop.
13. Interpreter shadows, forced-copy Wasm, and optimized direct-place Wasm agree
    on values, owner mutations, `var` write-backs, traps, and accepted programs.
    Checked-heap, poison, no-reuse, and UAF tests detect stale roots, double
    commits, premature release, aliasing, and leaks.
14. Async, generator, task, channel, escaping closure, `Dynamic`, serialization,
    existential, and host-capability boundaries either preserve every reference
    and lease explicitly or reject with a scoped/materialization remedy.
15. The migration command rewrites every unambiguous declaration and call,
    reports every ambiguous case, preserves nominal lifetime arguments, leaves
    no accepted legacy syntax, and validates a clean metadata rebuild.
16. Reference-free bodies show no material checker-time regression. The pinned
    corpus reports checker time, loan and subset-edge counts, peak memory,
    allocations, repair copies, root operations, and execution throughput before
    and after each precision phase.

## Alternatives

### `let('a) text: T` plus `T('a)`

This separates loan creation from first-class borrowed values, but the same API
appears to accept `T` and return a different nominal-looking type. It also
requires an extra direct-relation position on every type constructor and creates
forms such as `Parser('input, 'parser)`. Explicit references state both sides of
the contract with one type constructor.

### `text: T('a)` with implicit borrowing

This is more uniform than named `let`, but it still looks like ordinary generic
application and cannot cleanly distinguish `List(T('a))` from a reference to
list storage without a special trailing-argument rule. It also has no natural
exclusive counterpart other than overloading `unique` or adding another wrapper.

### `View(T, 'a)` and `MutView(T, 'a)`

Wrapper names are unambiguous but make references look like unrelated nominal
types, compose poorly in signatures, and duplicate a concept with established
prefix syntax. `&` makes reference nesting and target position immediately
visible.

### `&'a var T`

This would reuse Witchy's existing vocabulary, but `var` already means
move-in/write-back of a parameter binding. Using it for mutation through a
referent would make `var x: &'a var T` describe two unrelated write channels
with one word. Contextual `mut` keeps those axes separate.

### Shared references only

Shared-only references preserve more of the previous value surface, but leave
mutation-followed-by-borrow, stored exclusive access, and lending iteration on a
separate named-`var` mechanism. The resulting type system remains hybrid.
First-class affine exclusive references complete the model directly.

### Implicit borrow insertion at calls

Allowing `first(text)` where `first` expects `&'a String` would preserve old call
sites, but hide the creation of a loan and make ownership effects depend on
overload resolution. This RFC requires `first(&text)` and `edit(&mut text)`.
Passing an existing reference remains direct, and exclusive-reference arguments
receive the defined automatic reborrow.

### Infer every lifetime relation

Inference should determine concrete roots and duration. Public and multi-owner
APIs must still state which results depend on which inputs so modules, traits,
function values, and reviewers retain the contract.

### Keep the conservative loan engine

It is safe but rejects conditional-return and lending patterns. Point-sensitive
origin/loan propagation improves precision behind the same reference syntax and
keeps the solver implementation replaceable.

## Drawbacks

- `&`, `mut`, and `*` add syntax and make borrowing visible at call sites.
- First-class `&mut` introduces mutation through references and affine values,
  expanding Witchy's previous value-only source semantics.
- Reference-typed APIs require rules for moves, reborrows, aggregate storage,
  closure capture, traits, async boundaries, reflection, and every callable
  adapter.
- The migration changes declarations and calls and is intentionally breaking.
- Explicit borrow expressions are noisier than convention-directed implicit
  borrowing for simple read-only calls.
- Point-sensitive loan solving can regress compile time on large bodies and
  requires cardinality metrics plus a cheap reference-free path.
- Forced-copy parity for long-lived exclusive references requires a disciplined
  shadow/write-back model even when optimized Wasm uses direct places.
- General references to host capabilities remain unavailable until their lease
  semantics are specified.

## Prior art

- Rust reference syntax demonstrates a clear distinction between owned values,
  shared references, and affine mutable references. Rust's [2026 Polonius
  goal](https://rust-lang.github.io/goals/2026/polonius.html) and [current
  nightly status](https://rust-lang.github.io/polonius/current_status.html)
  motivate origins-as-loan-sets, point-sensitive propagation, conditional
  returns, lending iterators, formal modeling, and measured rollout.
- [Implementation Strategies for Mutable Value
  Semantics](../external-refs/mutable-value-semantics-2022/notes.md) supplies the
  read, exclusive, and consume access model behind `let`, `var`, and `own` and
  shows how optimized direct access can preserve value behavior.
- [Counting Immutable
  Beans](../external-refs/counting-immutable-beans-2019/notes.md) demonstrates
  inferred borrowed references as an RC-traffic optimization.
- Swift borrowing, Hylo access lifetimes, Vale regions, Cyclone regions, and
  lending iterators inform affine access, reborrowing, and owner-relative
  validity.

---

> 2026-08-13 design revision: the first draft used direct lifetime lifting such
> as `T('a)` and first-class exclusive values derived from `unique`. The second
> draft moved loan creation to `let('a)` and `var('a)` while retaining `T('a)`
> for results. Further long-term analysis found that both forms exposed compiler
> mechanics, overloaded nominal type application, and left exclusive access on
> a separate mechanism. This revision adopts explicit `&'a T` and `&'a mut T`
> reference types, explicit borrow expressions, and affine mutable references.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->
