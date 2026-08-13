---
rfc: 0122
title: "Named access lifetimes and uniform borrowed values"
status: proposed
created: 2026-08-13
updated: 2026-08-13
tracking: "Long-term design proposal; syntax replacement for RFC-0083 and semantic completion of named shared/exclusive access"
predecessors:
  - "[0026](0026-unique-qualifier.md) (`unique` and `local unique` ownership contracts)"
  - "[0083](0083-opt-mode-lifetimes.md) (`let('a) T`, `View(T, 'a)`, and shared owner loans)"
  - "[0087](0087-fused-mutators.md) (`var` as exclusive move-in/write-back access)"
  - "[0110](0110-opt-ownership-access-abi.md) (one typed access envelope for every callable)"
  - "[0112](0112-borrowed-aggregate-types.md) (lifetime-bearing nominal values and projection-aware loans)"
---

# RFC-0122: Named access lifetimes and uniform borrowed values

## Summary

Witchy has two related but different things to spell:

1. a parameter opens access to a caller-owned place; and
2. a first-class value carries a dependency on that access after the call.

This RFC gives each one a single syntax:

```text
fn first(let('a) text: String) -> String('a):
    text
```

`let('a)` opens named shared access to `text`. Inside the function, `text` may
produce read-only values tied to `'a`. `String('a)` is such a value: logically a
`String`, but represented using storage that remains valid only while the loan
identified by `'a` is live.

The exclusive counterpart extends the existing `var` convention rather than
introducing a mutable-reference type:

```text
fn normalize(var('a) text: unique String) -> String('a):
    text = text.trim().to_lower()
    text
```

`var('a)` opens named exclusive move-in/write-back access. The callee mutates an
ordinary local value, every structured return writes its final value back, and
the returned `String('a)` is a shared view of that final caller-owned value.
`unique` retains its existing meaning: the input must be uniquely owned so the
implementation can update it without a repair copy. It does not become a
reference kind.

The long-term surface is therefore:

```text
let x: T                  anonymous shared access for one call
let('a) x: T              named shared access that may flow into results
var x: T                  anonymous exclusive write-back access for one call
var('a) x: T              named exclusive write-back access that may flow into results
own x: T                  consume an owned or first-class value
x: T                      ordinary value parameter

T                         owned value
T('a)                     read-only value dependent on 'a
unique T                  uniquely owned value
local unique T            activation-confined uniquely owned value
```

There is deliberately no first-class `unique T('a)` mutable reference in this
RFC. Exclusive access belongs to `var('a)`, remains scoped to a call, and
commits through Witchy's existing value/write-back semantics. A later proposal
may add first-class exclusive references only if a real workload cannot be
expressed by named `var` access, but it would be a new source-semantics feature,
not a consequence of this syntax.

This RFC replaces the current `let('a) T` input type and `View(T, 'a)` result
type. It preserves the implemented shared-loan behavior, completes named
exclusive access without observable aliasing, and adopts point-sensitive
origin/loan analysis inspired by Polonius. The migration is intentionally
breaking and mechanical.

## Decision principles

The final form follows six principles.

### Access creation belongs to a parameter convention

`let`, `var`, and `own` already state what a call may do with an argument.
Naming a shared or exclusive access belongs on `let` or `var`, not inside the
argument's ordinary type annotation.

This avoids the visual duplication in the current form:

```text
fn first(let text: let('a) String) -> View(String, 'a)
```

and avoids overloading this form:

```text
fn first(let text: String('a)) -> String('a)
```

That spelling does not reveal whether the call opens a fresh loan from an owned
argument or merely receives an already borrowed value. The chosen syntax does:

```text
fn first(let('a) text: String) -> String('a)       // opens access
fn forward(text: String('a)) -> String('a)         // receives a borrowed value
```

### Borrowed values are first-class read-only values

`T('a)` may appear wherever a type may appear: results, fields, tuples,
containers, function values, trait methods, aliases, and parameter types. It is
copyable under ordinary Witchy value semantics because copying it creates
another read-only handle to the same owner obligation, not shared mutation.

### Exclusive mutation remains value/write-back

Witchy has no observable shared mutable storage. `var` preserves that property:
the callee receives a value, mutates its local binding, and returns a final value
through a hidden write-back channel. `var('a)` names that access so a result can
depend on the committed value; it does not expose an address that can be moved
elsewhere and mutated later.

### Lifetime, convention, and uniqueness remain separate axes

- `'a` relates result validity to an input access;
- `let`/`var`/`own` choose the calling protocol; and
- `unique`/`local unique`/`frozen` constrain an owned value.

Useful combinations remain direct:

```text
fn scan(let('a) bytes: frozen Bytes) -> TokenIter('a)
fn normalize(var('a) text: unique String) -> String('a)
fn consume(own parser: Parser('a)) -> Int
```

### API relations are explicit; concrete duration is inferred

A public signature says which output may depend on which input. The checker
infers concrete owner roots, branches, last uses, overwrites, and loan end
points. Users never annotate lexical end points or solver facts.

### Syntax does not promise runtime representation

Lifetime arguments are erased relation-kind arguments. They do not create
runtime generics, monomorphized copies, pointers, identity, or a dynamic borrow
checker. Lowering retains only the owner roots and projection metadata required
by the checked program.

## Surface syntax

### Named shared access

`let('a)` names an immutable parameter access:

```text
fn first(let('a) text: String) -> String('a):
    text

fn field(let('record) record: Record) -> Bytes('record):
    record.bytes
```

An owned argument or an existing read-only borrowed value may satisfy the
parameter. At the call site, an owned argument opens a loan on its exact place;
an existing borrowed argument reborrows its current owner set for the new
relation.

The parameter binding is immutable. Any number of shared values may derive from
it. The owner may be read, but overlapping mutation, `var` access, `own`
consumption, move, reassignment, or drop is rejected until every dependent value
reaches its path-sensitive last use.

Plain `let` remains the elided form when no result exposes the access:

```text
fn length(let text: String) -> Int:
    text.length()
```

The compiler may internally assign an anonymous origin, but it does not become
part of callable identity or source diagnostics unless needed to explain an
error.

### Named exclusive access

`var('a)` names an exclusive move-in/write-back parameter access:

```text
fn normalize(var('a) text: String) -> String('a):
    text = text.trim().to_lower()
    text
```

Every argument must be a mutable caller place, exactly as for existing `var`.
Evaluation reserves that place until the call's structured completion. The
callee mutates an ordinary local binding. Its final value writes back before the
ordinary result is exposed to the caller.

A result carrying `'a` depends on the committed final value, not the pre-call
value and not a hidden mutable alias. After write-back, the exclusive reservation
becomes one or more shared loans represented by the returned values. The caller
may mutate the place again after their last use.

```text
var text = "  Hello  "
let normalized = normalize(text)
console.print(normalized)
text = "done"                    // accepted: normalized's last use has passed
```

Structured completion follows RFC-0087: body tail, explicit `return`, and `?`
all commit every `var` output before control continues. A trap has no
source-observable partial-write-back guarantee, and a trapped VM is terminal.

Plain `var` remains the elided form when no result exposes a relation to the
written-back value.

### Borrowed value types

`T('a)` is the canonical spelling for a read-only value dependent on relation
`'a`:

```text
String('a)
Bytes('a)
List(Int, 'a)
Parser('a)
PairView(Int, 'left, 'right)
```

The grammar uses ordinary parenthesized type arguments:

```text
type-argument = type | lifetime
lifetime      = "'" identifier
named-type    = qualified-name ["(" type-argument {"," type-argument} ")"]
```

Lifetime arguments have relation kind. Ordinary type arguments have type kind.
The parser preserves both; kind checking validates positions after aliases and
nominal declarations are known.

For built-in storage types, a trailing lifetime is the direct storage relation:

```text
String('a)
List(Int, 'a)
Dict(String, Int, 'a)
```

For a type variable, applying a lifetime is relation lifting:

```text
fn identity(let('a) value: t) -> t('a):
    value
```

`t('a)` means the same logical `t` represented through a read-only dependency on
`'a`. It does not require higher-kinded user syntax and does not permit arbitrary
ordinary arguments on `t`.

Relation lifting is legal for scalar instantiations so generic APIs do not split
by representation. `Int('a)` retains its relation in callable identity and loan
checking, but needs no runtime owner root; materialization is representation-
identity. The optimizer may erase the vacuous root operation, not the API
relation.

Capabilities cannot be relation-lifted by an ordinary lifetime. A host-backed
view needs both a data relation and an unforgeable lease supplied by its
capability-specific API. `Dir('a)` is rejected.

### Lifetime-bearing nominal types

Nominal declarations retain relation parameters:

```text
type Parser('input):
    input: String('input)
    offset: Int

type PairView(t, 'left, 'right):
    first: t('left)
    second: t('right)
```

`Parser('input)` is an owned parser shell whose `input` field depends on
`'input`. Construction must prove every declared relation from its fields. An
unused relation declaration is an error.

A named access may borrow the parser shell independently of the relations
already carried by its fields:

```text
fn inspect(let('parser) parser: Parser('input)) -> Int:
    parser.offset
```

If a result is a view of the parser shell itself, the direct shell relation is a
trailing relation after the declaration's own parameters:

```text
fn shell(let('parser) parser: Parser('input)) -> Parser('input, 'parser):
    parser
```

The kind checker knows that `Parser` declares one relation. Its first lifetime
argument fills `'input`; the optional final relation is the direct shell view.
A type has at most one direct relation in addition to its declared relation
parameters.

Reborrowing a shell replaces that direct relation with a shorter one while
preserving declared field relations:

```text
Parser('input, 'outer)  ->  Parser('input, 'inner)
```

where `'inner` is bounded by `'outer`. Direct relation lifting therefore never
grows an unbounded chain such as `Parser('a, 'b, 'c)`.

A nominal type with no declared relation uses its first lifetime as the optional
direct relation: `Account('a)` is a read-only view of an `Account`. For a type
with ordinary parameters, declared arguments come first and the direct relation
remains last: `Pair(String, Int, 'a)`.

### Reborrowing, shortening, and variance

A shared value can always be reborrowed for a relation no longer than its
current one:

```text
fn shorten(let('short) value: String('long)) -> String('short):
    value
```

The signature introduces `'short` through named access to an already borrowed
value. Type checking records `'short` as a subset of `'long`; it cannot infer the
reverse edge. There is no source `outlives` clause in this RFC. Input access,
result flow, and aggregate construction generate all required subset constraints.

The direct shared relation on `T('a)` is covariant: a value valid for a longer
relation may be shortened. Declared nominal relation parameters derive variance
from their field uses. A relation used only in shared fields/results is
covariant; one used in a callable input or another invariant position is checked
with the corresponding structural variance. The initial implementation may
treat an unresolved nominal position invariant, but it may not accept an
unsound extension.

Mutable bindings do not make a relation contravariant. Replacing a borrowed
field or rebinding a borrowed variable must satisfy the binding's already checked
relation type; mutation cannot relabel an owner.

`'static` is not introduced. Frozen storage is still owned storage and does not
manufacture an unbounded relation. A future static-data or host-lease design must
define its own valid origin.

### Containers and nested relations

The location of a relation remains meaningful:

```text
List(Token('input))        // owned list of tokens borrowing input
List(Token, 'list)         // read-only view of list storage
List(Token('input), 'list) // list view whose elements also borrow input
```

These types have different owner sets and drop/root obligations. The formatter
does not normalize one into another.

Tuples and structural records carry the union of their fields' owner sets. A
nominal aggregate carries the relations declared by its type. Containers may
hold borrowed elements only when their layout descriptor transports and drops
the required roots, as completed for `List(B('a))` by RFC-0112.

### Function types

Named access binders and result relations are part of callable identity:

```text
fn(let('a) String) -> String('a)
fn(var('a) unique String) -> String('a)
fn(String('a)) -> String('a)
```

The first opens shared access from a caller argument. The second opens exclusive
write-back access and requires unique owned storage. The third accepts an
already borrowed value under ordinary value passing. They are not interchangeable
function types.

Lifetime names are alpha-normalized within each callable identity. Renaming
`'a` to `'input` does not change the type; changing which parameter binds the
result does.

Direct calls, methods, UFCS calls, closures, trait witnesses, existential
adapters, generated wrappers, and proper-tail dispatch preserve the same access
envelope. Any cast or adapter that erases a convention, named origin, result
relation, write-back output, or uniqueness requirement is rejected.

## Lifetime binding and owner relations

### Implicit quantification

Lifetime names in a callable are implicitly universally quantified. A named
`let('a)` or `var('a)` parameter introduces an origin. An already borrowed input
may also introduce a relation for forwarding:

```text
fn forward(value: String('a)) -> String('a):
    value
```

Every result lifetime must be reachable from an input origin or an input
borrowed value of the same name. A result cannot invent an owner:

```text
fn bad(value: String) -> String('a)       // error: 'a has no input source
```

### Independent inputs

Distinct names state an exact result dependency:

```text
fn left(let('left) left: String,
        let('right) right: String) -> String('left):
    left
```

The result loans only `left`.

### One result relation with several possible owners

The same origin name may be introduced by several shared inputs:

```text
fn choose(let('a) left: String,
          let('a) right: String,
          pick: Bool) -> String('a):
    if pick: left else: right
```

`'a` represents the union of owners that can reach the result. It does not claim
that `left` and `right` are the same allocation. At a call, every possible owner
remains loaned until point-sensitive analysis proves that a path selects only
one. A conservative implementation may retain both; it may not forget either.

The same-name form is unavailable for two `var` parameters because RFC-0087
requires exclusive write-back places and distinct result dependencies must remain
distinguishable. Use separate origins and return an aggregate carrying both:

```text
fn edit_both(var('left) left: String,
             var('right) right: String) -> PairView(String, 'left, 'right)
```

### Owner sets and projections

Each borrowed value has an `OwnerSet` containing:

- one or more stable owner roots;
- a projection path or checked range;
- the symbolic relation positions exposed by its type; and
- open, transfer, and close points.

Projection composes paths and ranges with the existing root. It never treats an
interior address as an owning RC base. Shared projections may overlap. Any live
projection blocks an overlapping mutation or move of its root.

Joins union possible roots. Destructuring transfers each field's owner set to
the corresponding bindings. Copying a borrowed aggregate creates another
read-only obligation; every copy must reach its final use before the owner is
released.

## Interaction with ownership features

### `unique`

`unique T` remains an owned-value contract: the value is the sole owning
reference and can be reused in place. It strengthens named access without
changing its kind:

```text
fn parse(let('a) input: unique Bytes) -> Parser('a)
fn normalize(var('a) text: unique String) -> String('a)
```

For shared `let('a)`, uniqueness can eliminate an initial repair or retain but
the body still receives read-only access. For `var('a)`, uniqueness certifies
that direct caller storage may be updated without copy-in/copy-back. In normal
mode a missing uniqueness proof may insert one repair copy where the existing
contract allows it; in `mode opt` it is an error with ownership provenance.

`unique T('a)` and `local unique T('a)` are rejected. `T('a)` is by definition
a first-class shared read-only value; applying an owned-storage uniqueness
qualifier would suggest a first-class exclusive reference and delayed mutation
channel that this RFC does not define. The supported exclusive form is:

```text
var('a) value: unique T
```

This is not a loss of current expressiveness: RFC-0083 and RFC-0112 expose only
shared borrowed values. It is a deliberate boundary against accidentally adding
Rust-style `&mut` semantics under the name `unique`.

### `local unique`

`local unique T` remains a unique owned value confined to one activation. It
composes with `let` and `var`; a named result may borrow from it only when the
result itself cannot escape the activation:

```text
fn inspect_local(let('a) value: local unique T) -> T('a)
```

Such a function may be private and immediately consumed within the proven local
scope. A public or escaping result is rejected because the owner's local-unique
contract cannot be extended by naming a lifetime.

### `frozen`

`frozen T` is deeply immutable owned storage. Shared named access is valid and
can be returned:

```text
fn slice(let('a) text: frozen String) -> frozen String('a)
```

`var frozen T` and `var('a) frozen T` remain errors. `frozen T('a)` is useful
because it carries both the owner's deep-immutability promise and the view's
validity relation.

### `own`

`own` consumes its argument value. It does not introduce a lifetime because the
caller owner does not remain available to bound a returned borrow:

```text
fn digest(own bytes: Bytes) -> Digest
fn count(own parser: Parser('a)) -> Int
fn consume_view(own text: String('a)) -> Int
```

The latter two consume first-class relation-bearing values and transfer their
root obligations into the callee. The underlying owner is not consumed through
a shared view; it merely remains loaned until the consumed value's final use.

There is no `own('a)` form. A function that consumes owned storage and returns
part of the same allocation should return an owned result, use `var('a)` so a
caller place survives as the owner, or package the owner and projection in a
new owned nominal type. A borrow may not depend on a caller binding that `own`
killed.

### `var` applied to borrowed values

This form remains distinct:

```text
fn select(var view: String('a), replacement: String('a)) -> Nil
```

It writes a final read-only view handle back to a caller variable already typed
`String('a)`. It does not mutate the owner named by `'a`. Only `var('a) x: T`
opens named exclusive access to an owned caller place.

`var('a)` rejects a parameter annotation that already has a direct relation,
such as `var('new) view: String('old)`. That spelling would ambiguously reserve
the caller's view-handle slot while appearing to grant exclusive access to the
underlying string. Use plain `var view: String('old)` to replace the handle, or
`var('new) owner: String` to reserve and mutate owned string storage. A nominal
shell may still contain declared borrowed fields:

```text
fn advance(var('cursor) cursor: Parser('input)) -> Parser('input, 'cursor)
```

Here named `var` reserves the owned `Parser` shell; it does not grant mutation of
the separate input owner named by `'input`.

## Materialization and ownership recovery

`.owned()` removes a direct borrowed relation by producing an independent owned
logical value:

```text
fn copy(view: String('a)) -> String:
    view.owned()
```

The materialization use can close the loan immediately. On an owned value,
`.owned()` remains identity through the existing blanket trait.

Borrowed nominal aggregates use an explicit owned-companion conversion when
their owned shape differs. The compiler never drops lifetime arguments and
guesses an owned representation.

There is no conversion from `T('a)` to `unique T`. A caller may materialize and
then prove the new owned copy unique:

```text
let owned = view.owned()
```

but this does not recover exclusive access to the original owner.

## Mutation, failure, and observability

Named `var` access preserves RFC-0087 exactly:

1. evaluate the caller place and projection coordinates once;
2. reserve the place against overlapping `var` arguments;
3. move or copy its logical value into the callee;
4. run the callee with an ordinary mutable local binding;
5. produce the ordinary result and every final `var` value;
6. atomically commit all write-backs on structured completion; and
7. open result loans against the committed roots.

The return expression is evaluated against the callee's final local value, but
the caller observes the result only after write-back. Lowering transports the
result's owner relation to the committed root, including when replacement
changes the runtime root or capacity token.

No pointer identity or mutation-through-view is observable. Equality,
reflection, and pattern matching see logical values. A forced-copy execution is
the semantic oracle for shared views and named exclusive write-back.

## Flow-sensitive loan analysis

### Origins and loans

A source lifetime is an origin in a callable contract. Each access at a call or
construction creates concrete loans:

```text
LoanFact {
    id,
    origin,
    kind: Shared | ExclusiveReservation,
    owner_root,
    projection,
    introduced_at,
}
```

`ExclusiveReservation` exists only while a `var` call is evaluating. Returned
`T('a)` values carry shared loans after commit. There is no first-class exclusive
loan value in this RFC.

The checker records point-indexed relations:

```text
origin_subset_at(sub, sup, point)
origin_live_at(origin, point)
loan_killed_at(loan, point)
place_invalidated_at(root, projection, access, point)
var_commit_at(origin, old_root, new_root, point)
```

A loan is live at a point when it can reach a live origin through the
point-sensitive subset graph and has not been killed by last use, overwrite,
materialization, or a valid transfer. An invalidation is accepted only when no
incompatible loan reaches that exact point.

This adopts the useful Polonius distinction between origins and loans without
requiring its historical Datalog implementation. Point-indexed facts are the
semantic interface. The solver may be a graph worklist, localized reachability,
incremental engine, or another measured implementation.

### Conflict rules

For overlapping places:

| Existing access | Shared read | `var` reservation | Move/drop owner |
|---|---:|---:|---:|
| shared loan | allowed | rejected | rejected |
| live `var` reservation | rejected | rejected | rejected |

Reads and writes of the callee's move-in local are operations inside the
reservation, not new accesses to the caller place. No independently evaluated
caller expression may read that place until write-back commits.

Two `var` reservations must be proven disjoint under RFC-0087. Static
record/tuple fields and distinct constant indices are the initial proof set.
Unknown overlap is overlap. Dynamic range disjointness is a later precision
improvement, not a reason to weaken safety.

### Conditional returns

The solver tracks origin subsets at control-flow points so a returned borrow on
one branch does not keep an impossible loan alive on a sibling branch:

```text
fn get_or_insert(var('a) table: unique Dict(String, Value), key: String)
    -> Value('a)
```

If the existing-value path returns a projection and the missing-value path
updates before returning a new projection, the update is legal when no loan from
the first path reaches that point. A whole-function union would reject this
sound program; point-sensitive propagation accepts it.

### Lending iteration

A lending iterator may return a view whose relation is bounded by named access
to the iterator's caller place:

```text
trait LendingIterator(item):
    fn next(var('next) self: Self) -> Option(item('next))
```

Each item loans the committed iterator state. Calling `next` again requires the
previous item to be dead or materialized. This expresses the main lending
pattern without a first-class mutable reference: mutation remains one
synchronous `var` call, while each yielded item is read-only.

The exact trait syntax for relation-lifting an associated `item` follows the
ordinary `t('a)` rule and must be proven through direct, witness, and function-
value calls before stabilization.

### Precision stages

Analysis precision advances without changing source syntax:

1. preserve current straight-line last-use and projection owner sets;
2. add named `var` commit-to-result-root facts;
3. compute path-sensitive origin subsets for conditional returns;
4. compute loop fixpoints with overwrite kills for lending iterators; and
5. add dynamic disjoint-range proofs only when corpus evidence justifies cost.

A cheap pass may select conflict-relevant CFG components for precise solving,
but it may not reject a program. Rejection requires a concrete incompatible loan
at the invalidating point. Bodies with no named or first-class borrowed values
stay on the existing cheap path.

## Escapes and boundaries

A `T('a)` value may be:

- passed under ordinary, `let`, or `own` conventions;
- returned when `'a` is bound by an input relation;
- copied into another relation-preserving aggregate;
- projected, destructured, and placed in supported borrowed containers; and
- captured by a proven non-escaping closure within every owner lifetime.

It may not be:

- stored in a type that erases `'a`;
- converted to `Dynamic` or an owned existential without materialization;
- sent through a channel or isolated worker;
- captured by an escaping closure or task;
- held live across `await` or `yield` in the initial implementation; or
- serialized or reflected as an address-bearing representation.

Async `var('a)` is excluded initially because it reserves a caller place across
suspension. Synchronous named-access calls before or after suspension remain
valid. A future scoped-concurrency or coroutine-access RFC may relax this only
with an explicit owner and cancellation/cleanup contract.

Capabilities remain outside ordinary lifetime lifting. Host buffers require a
lease-bearing API whose callable envelope transports both data lifetime and host
lease. A lifetime cannot widen a grant or keep authority alive by itself.

## Representation and lowering

Lifetime arguments have no payload representation. The checker and lowering
retain:

- owner-root identities;
- projection descriptors;
- relation positions in callable and nominal types;
- open/transfer/commit/close events; and
- representation-specific root or lease obligations.

Compiled shared views retain each distinct linear-memory owner root until the
checked last use. Typed GC roots remain typed references. Lowering never emits
owning retain/drop operations on an interior projection.

Named `var` access uses the existing logical envelope:

```text
(explicit arguments, ownership inputs)
    -> (ordinary result, var write-backs, ownership outputs, result owner relations)
```

The physical ABI may flatten or omit empty components. Direct caller-storage
lowering is legal only when checked-place, uniqueness, overlap, escape, and
layout proofs all hold. Otherwise normal mode uses copy-in/copy-back. `mode opt`
rejects a missing proof when the signature promises `unique` no-copy access.

The interpreter may materialize shared views and implement `var` as copy-in/
copy-back. Compiled Wasm may retain roots and update direct storage. Both consume
the same checked facts and must agree on values, write-backs, traps, and accepted
programs.

Cleanup covers fallthrough, explicit return, `?`, branches, loops, and generated
adapters. A trap makes the VM terminal so no host API can resume or inspect a
partially committed instance.

## Diagnostics

Diagnostics name:

- the owner and projection;
- the named `let` or `var` access that introduced the relation;
- the borrowed value keeping it live;
- the conflicting mutation, write-back, move, drop, erasure, or suspension;
- the path-sensitive final use when available; and
- a repair: shorten use, preserve the relation, materialize with `.owned()`,
  split relations, or move the mutation after the loan closes.

They render source forms such as `String('a)` and `var('a)`, never internal
`View`, origin numbers, hidden roots, capacity tokens, or solver edges.

Targeted syntax diagnostics guide migration:

```text
`let('a) String` is the retired borrowed-type spelling;
name the parameter access instead: `let('a) text: String`

`View(String, 'a)` is the retired borrowed-value spelling;
write `String('a)`
```

## Migration

Acceptance triggers one mechanical source migration:

| Old declaration | New declaration |
|---|---|
| `let text: let('a) String` | `let('a) text: String` |
| `text: let('a) String` | `let('a) text: String` |
| `var text: let('a) String` | rejected today; choose `var('a) text: String` only when exclusive semantics are intended |
| `View(String, 'a)` | `String('a)` |
| `View(List(Int), 'a)` | `List(Int, 'a)` |
| `View(t, 'a)` | `t('a)` |
| `type Parser('a)` | unchanged |

`witchy migrate borrowed-relations <paths...>` performs AST-based rewrites for
unambiguous shared forms, including nested types, function types, aliases,
quoted types, generated fixtures, and documentation examples. It reports rather
than guesses for old illegal or ambiguous convention combinations.

The compiler, formatter, reflection vocabulary, `meta.type_*` builders,
highlighter, language server, stdlib docs, book, spec, examples, and differential
fixtures change in the same cut. The old `View` and type-position `let('a)` forms
remain only as targeted parse diagnostics; they are not aliases and do not
survive formatting.

Serialized compiler metadata and cached modules bump their format version.
There is no compatibility shim. Packages must migrate source before rebuilding.

RFC-0083 and RFC-0112 remain frozen historical records. Once this RFC is
accepted and implemented, their metadata receives a supersession note; their
bodies are not rewritten.

## Implementation plan

### Phase 0: executable design model

- Add parser-independent signature tests for every form in this RFC.
- Freeze the complete RFC-0083/RFC-0112 positive and negative corpus.
- Add named-`var` return, conditional-return, lending-iterator, multi-owner, and
  callable-erasure fixtures before changing syntax.
- Build a small reference access interpreter over values, owner sets, and
  structured write-back; use it as the semantic oracle for new exclusive cases.
- Record checker time, fact counts, peak memory, root counters, allocations, and
  parser/iterator throughput on the pinned corpus.

### Phase 1: syntax and kind model

- Parse `let('a)` and `var('a)` as conventions with optional origin binders.
- Parse lifetime arguments through the ordinary type-argument loop.
- Replace source `TypeQual::Borrow` with relation lifting in the checked type
  model while preserving the current internal lowering path during migration.
- Record each nominal constructor's declared relation arity and one optional
  direct relation.
- Implement generic `t('a)` relation lifting and reject capability lifting.
- Update aliases, formatter, linker, type resolution, quotations, reflection,
  derive expansion, highlighter, LSP, and diagnostics.
- Ship the AST migration command.

Phase 1 migrates shared syntax only. Matched tests must prove identical
acceptance, owner sets, generated values, root operations, and materialization
counters before named `var` is enabled.

### Phase 2: callable access envelopes

- Extend RFC-0110 callable identity with optional origins on `let` and `var`.
- Preserve relation binders through direct calls, methods, UFCS, closures,
  function values, traits, witnesses, generated adapters, and tail dispatch.
- Define owned-to-shared borrowing and borrowed-to-shared reborrowing.
- Reject every convention, origin, result-relation, or ownership-state erasure.

### Phase 3: named `var` access

- Extend checked-place facts with named reservation and commit-to-root events.
- Reuse RFC-0087 evaluation order, overlap, structured return, and atomic commit.
- Attach returned shared relations to the final committed roots, including root
  replacement and capacity-token changes.
- Implement interpreter and forced-copy Wasm copy-in/copy-back first.
- Enable direct-storage lowering only from the same checked facts.
- Cover `var('a) T`, `var('a) unique T`, multiple disjoint `var` parameters,
  `?`, explicit return, traps, projections, and result aggregates.

### Phase 4: point-sensitive precision

- Introduce origin/loan subset reachability at CFG points.
- Handle conditional returns and sibling-path invalidations.
- Add loop fixpoints and overwrite kills for lending iteration.
- Keep precise solving local to conflict-relevant components.
- Validate against the reference access model and frozen negative corpus.

### Phase 5: migration and documentation

- Rewrite the repository with the migration command and inspect every reported
  ambiguity.
- Update `spec/language.md`, `spec/performance.md`, book ownership/performance
  chapters, standard-library docs, reflection docs, and runnable examples.
- Publish old/new syntax, shared/named-exclusive behavior, materialization, and
  diagnostic examples.
- Land independently green compiler, runtime, tooling, tests, docs, and evidence
  slices through the merge queue while one integration track stays runnable.
- Mark the RFC implemented only when every criterion below has current evidence
  on `master`.

## Acceptance criteria

1. `let('a)` and `var('a)` parse, format, reflect, quote, and survive every
   callable representation as named shared/exclusive access conventions.
2. `T('a)` parses, kind-checks, aliases, reflects, and resolves uniformly for
   built-ins, type variables, nominal types, fields, nested containers,
   parameters, results, and function types.
3. Every migrated RFC-0083/RFC-0112 shared fixture has identical acceptance,
   owner sets, diagnostics intent, interpreter result, Wasm result, root balance,
   and materialization counters.
4. `let('a) x: T`, `var('a) x: T`, and `x: T('a)` remain distinct in callable
   identity and no cast, trait witness, closure, adapter, or tail edge erases the
   distinction.
5. Shared loans permit overlapping reads and reject overlapping mutation,
   write-back, move, consumption, and erasing escape until path-sensitive last
   use or materialization.
6. Named `var` writes back before exposing its ordinary result and attaches every
   returned `'a` relation to the final committed root on tail, explicit return,
   and `?` paths.
7. `var('a) unique T` enforces the existing no-copy contract without creating a
   first-class exclusive reference. `unique T('a)` is rejected with a diagnostic
   pointing to named `var` access when mutation is intended.
8. Conditional-return and lending-iterator fixtures compile under point-sensitive
   analysis without weakening any negative case. Rejections identify the exact
   reaching loan and invalidation point.
9. Borrowed nominal shells, nested projections, multiple owner relations, and
   `List(B('a))` preserve exact roots through copy, overwrite, destructure,
   iteration, return, and drop.
10. Interpreter copy-in/copy-back, forced-copy Wasm, and optimized direct-storage
    Wasm agree on values, write-backs, traps, and accepted programs. Checked-heap,
    poison, no-reuse, and UAF tests detect stale roots, double commits, premature
    release, and leaks.
11. Async, generator, task, channel, escaping closure, `Dynamic`, serialization,
    existential, and host-capability boundaries either preserve every relation
    and lease explicitly or reject with a scoping/materialization remedy.
12. The migration command rewrites every unambiguous old spelling, reports every
    ambiguous convention combination, leaves no accepted legacy syntax, and a
    clean rebuild validates the new metadata format.
13. Borrow-free bodies show no material checker-time regression. The pinned
    corpus reports checker time, loan count, subset-edge count, peak memory,
    allocations, root operations, and execution throughput before and after each
    precision phase.

## Alternatives

### `let text: T('a)`

Compact, but overloaded: it can mean either “open a borrow from this owned
argument” or “receive an existing borrowed value.” The distinction matters to
callable identity, owner rooting, `var`, and `own`. This RFC uses
`let('a) text: T` for access creation and `text: T('a)` for a first-class
borrowed input.

### Keep `let('a) T` and `View(T, 'a)`

Implemented and explicit, but the parameter convention and input type repeat
`let`, results use a different shape, and nominal aggregates already use
relation arguments. The chosen syntax preserves the semantic distinction while
removing the duplication.

### Prefix lifetime types: `'a T`

Concise, but composition with qualified and nested types is less clear, and it
does not match existing nominal relation arguments. `T('a)` keeps the logical
type at the head and uses one argument grammar.

### `View(T, 'a)` everywhere

Uniform and unambiguous, but presents a logical `T` as a parallel wrapper type,
does not align with `Parser('a)`, and makes generic/nested signatures noisier.
`T('a)` is relation lifting, not a user-defined nominal wrapper.

### First-class `unique T('a)` exclusive references

Expressive, but materially changes Witchy's source semantics. Such a value would
need affine movement, mutable reborrowing, delayed commit or mutation-through-
reference, interaction with `own`, and rules for storage in aggregates and
across suspension. Forced-copy parity would also need to preserve observable
mutation timing.

Named `var('a)` handles the identified workloads - mutation followed by a
returned view, conditional lookup/insert, and lending iteration - while keeping
exclusive mutation synchronous and value-based. This RFC therefore rejects
first-class exclusive references rather than pretending `unique` already means
one.

### `own('a)`

Rejected because `own` kills the caller binding that would bound `'a`. Returning
a view of consumed storage either needs an owned result, a surviving `var('a)`
place, or an owned object packaging both storage and projection.

### Infer every result relation

Inference should determine concrete roots and duration, but public and
multi-owner APIs must state which result depends on which input so modules,
traits, function values, and reviewers retain the contract.

### Keep the current conservative loan engine

Safe, but unnecessarily rejects conditional-return and lending patterns. The
point-sensitive origin/loan model improves precision behind the same API syntax
and keeps solver implementation replaceable.

## Drawbacks

- `let('a)` and `var('a)` make parameter conventions slightly heavier when an
  access escapes through a result.
- `T('a)` resembles ordinary generic application even though lifetime arguments
  have relation kind and no runtime representation.
- A nominal type with declared field relations plus a direct shell relation can
  have forms such as `Parser('input, 'parser)`, which require explanation.
- `List(T('a))` and `List(T, 'a)` are intentionally different.
- The syntax and compiler metadata migration are breaking.
- Point-sensitive loan solving can regress compile time on very large bodies and
  requires cardinality metrics plus a cheap borrow-free path.
- Named `var` results add owner-root transport to write-back lowering and every
  callable adapter.
- The design deliberately withholds first-class mutable references. A future
  workload may prove that boundary too restrictive and require a separate RFC.

## Prior art

- Rust lifetimes demonstrate erased API relations; Rust's [2026 Polonius
  goal](https://rust-lang.github.io/rust-project-goals/2026/polonius.html) and
  [nightly alpha announcement](https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nighty/)
  motivate origins-as-loan-sets, point-sensitive propagation, conditional-return
  acceptance, lending iterators, formal modeling, and measured rollout. Witchy
  adopts those analytical lessons, not Rust's reference syntax or implementation
  wholesale.
- [Implementation Strategies for Mutable Value
  Semantics](../external-refs/mutable-value-semantics-2022/notes.md) supplies the
  read/exclusive/consume access model behind `let`/`var`/`own` and supports
  exclusive mutation through move-in/write-back without shared mutable aliases.
- [Counting Immutable
  Beans](../external-refs/counting-immutable-beans-2019/notes.md) demonstrates
  inferred borrowed references as an RC-traffic optimization.
- Swift borrowing, Hylo access lifetimes, Vale regions, Cyclone regions, and
  lending iterators reinforce the separation between access mode, ownership,
  and validity relation.

---

> 2026-08-13 design revision: the initial draft placed a lifetime directly on
> input types (`let text: T('a)`) and proposed first-class `unique T('a)`
> exclusive references. Long-term analysis found that this overloaded access
> creation with borrowed-value passing and silently expanded `unique` into a
> mutable-reference kind. The revised design names origins on `let`/`var`, keeps
> `T('a)` read-only, and confines exclusive mutation to synchronous write-back.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->
