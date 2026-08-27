# Opt-mode References and Lifetimes

Most of the time you write Witchy with owned values and the ordinary `let`,
`var`, `own`, and `move` vocabulary, and you never think about lifetimes. When
you need to hand out access to a value without copying it, you can opt a single
file into explicit references with a leading directive:

```witchy-static
mode opt
```

Nothing outside that file changes. This chapter covers what you can write once
you have opted in; the [performance appendix](appendix-performance.md) covers
the optimization passes and counters that go with it.

A complete program that returns a shared reference through a named lifetime:

```witchy
mode opt

fn first(let text: &'a String) -> &'a String:
    text

fn main(console: Console):
    let text = "hello"
    console.print("${*first(&text)}")
```

```text
hello
```

Two practical points before the details:

- Reference types are written `&'a T` and `&'a mut T`. Ownership-relevant
  reference parameters also carry an explicit `let`, `var`, or `own`
  convention: in `let text: &'a String`, the `let` qualifies the reference
  handle, and the type itself stays `&'a String`.
- Closure capture is deliberately conservative. A capture the checker cannot
  prove non-escaping is rejected rather than silently copied - pass the
  reference explicitly to a callback, or call `.owned()` before capturing.

## Two modes, one language

In normal mode you never name a lifetime, and the compiler never rejects your
code because an internal optimization was hard to prove. When you call into an
opt module from normal code, the signature looks like any other call:

```witchy-static
fn inspect(let text: String) -> Int
fn normalize(var text: String) -> Nil
fn digest(own text: String) -> Digest
```

Opt mode keeps all of those conventions and adds reference types for the cases
where you need to return, store, or lend out access itself. Think of the mode
boundary as a performance escape hatch rather than a second language. If you
write a reference in a normal file, you get one short diagnostic about the mode
boundary - not a cascade of lifetime errors.

## Reference types

The two reference kinds are:

```witchy-static
&'a T          shared, read-only access valid for 'a
&'a mut T      exclusive, mutable access valid for 'a
```

Every lifetime written in an opt-mode declaration is explicit. Borrow
expressions infer their local lifetime:

```witchy-static
fn first(let text: &'a String) -> &'a String:
    text

fn normalize(let text: &'a mut String) -> &'a String:
    text.trim_in_place()
    text

let view = &text
let editable = &mut text
```

`mut` belongs to the reference type because it grants mutation through the
referent. It is not another spelling of `var`: `var` writes a parameter value
back to its caller slot, while `&mut` exposes an existing place that can be
read and written for a lifetime.

Borrow expressions require stable places such as locals, parameters,
dereferences, fields, tuple elements, or checked projections:

```witchy-static
let text = make_string()
let view = &text
let field = &mut account.name
let item = &items.at(index)
```

Borrowing an unbound temporary is rejected. Bind the owner first rather than
depending on a temporary-lifetime extension:

```witchy-static
let bad = &make_string()       // error: bind the owner first
```

Dereference and projection operate on the same logical place. Reading through
either kind is allowed; assignment through a shared reference is not:

```witchy-static
fn clear(let text: &'a mut String) -> Nil:
    *text = ""

fn rename(let account: &'a mut Account) -> Nil:
    account.name = "renamed"
```

The second form is projection sugar for `(*account).name`. It does not copy,
materialize, or widen the reference.

## Shared and exclusive access

Shared references are copyable handles to read-only access. Several overlapping
shared references may coexist, but the owner cannot be moved, consumed,
reassigned, mutated, dropped, or mutably borrowed while one remains live:

```witchy-static
var text = "hello"
let left = &text
let right = &text
inspect(left)
inspect(right)
text = "done"                 // valid after both final uses
```

Exclusive references are affine. They grant read and write access to one place,
exclude overlapping access, and may be moved but never copied:

```witchy-static
var text = "hello"
let editable = &mut text
editable.push("!")
// console.print(text)        // error while `editable` is live
```

Moving an exclusive reference transfers its capability and makes the old
binding unavailable. An aggregate containing exclusive references is affine;
an aggregate containing only shared references is copyable and carries all of
its owner obligations.

An exclusive reference passed to an ordinary parameter is reborrowed for that
call. The outer reference is suspended only for the returned reborrow's live
duration:

```witchy-static
fn append_mark(let text: &'call mut String) -> Nil:
    text.push("!")

fn twice(let text: &'outer mut String) -> Nil:
    append_mark(text)
    append_mark(text)
```

`own` on an exclusive-reference parameter consumes the reference handle rather
than creating a reborrow. Consuming a shared handle ends that handle but does
not consume its owner.

## Reborrowing and lifetime relations

An exclusive reference can be shortened to a shared reference. The exclusive
capability is not recoverable through that shared result:

```witchy-static
fn finish(let text: &'a mut String) -> &'a String:
    text.trim_in_place()
    text

fn inspect_then_edit(let text: &'outer mut String) -> Nil:
    let view = &*text
    inspect(view)
    text.push("!")
```

An exclusive projection can also return an exclusive reference to a field:

```witchy-static
fn name(let account: &'a mut Account) -> &'a mut String:
    &mut account.name
```

Lifetime names describe public relations, not concrete durations. A result
lifetime must be reachable from an input relation. The checker infers concrete
borrow duration, root provenance, reborrow shortening, branch joins, loop
fixpoints, overwrites, and final uses. There is no `'static` spelling or
general outlives clause in this model.

Distinct lifetime names express independent result dependencies:

```witchy-static
fn left(let left: &'left String, let right: &'right String) -> &'left String:
    left
```

The same lifetime on multiple inputs means every possible result must remain
valid for that relation. It does not prove that two exclusive inputs are
disjoint; unknown overlap remains an error.

## Nominal types, containers, and generics

Nominal lifetime parameters are retained only for relations stored in fields:

```witchy-static
type Parser('input):
    input: &'input String
    offset: Int

type PairView(t, 'left, 'right):
    first: &'left t
    second: &'right t
```

`Parser('input)` is an owned parser containing a reference. A reference to the
parser is written separately as `&'parser Parser('input)`. An unused declared
lifetime is an error, and variance follows from how each lifetime is used in
fields.

Reference placement identifies the borrowed storage precisely:

```witchy-static
List(&'input Token)             // owned list of shared references
&'list List(Token)              // shared reference to list storage
&'list mut List(Token)          // exclusive reference to list storage
&'list List(&'input Token)      // borrowed list whose elements borrow input
Option(&'a mut Buffer)
```

Tuples, structural records, nominal aggregates, `Option`, `Result`, and
supported nested lists carry the union of their fields' owner obligations.
Copying shared-reference aggregates copies those obligations. Moving an
exclusive-reference aggregate transfers them and kills the source binding.
Field access, indexing, destructuring, list iteration, and return/call
transport preserve the carrier rather than recovering a place from syntax.

References apply directly to type variables:

```witchy-static
fn identity(let value: &'a t) -> &'a t:
    value

fn swap(let left: &'a mut t, let right: &'b mut t) -> Nil:
    let temporary = (*left).owned()
    *left = (*right).owned()
    *right = temporary
```

`&'a Int` remains a meaningful API relation even when a backend can represent
the scalar without a retained heap root. Capability and host-lease references
are not manufactured by the generic syntax; they require an API that transports
the unforgeable lease with the data relation.

## Function values and ownership qualifiers

Reference kind, lifetime relation, affinity, and parameter convention are part
of callable identity:

```witchy-static
fn(own String) -> String
fn(let String) -> Int
fn(&'a String) -> &'a String
fn(&'a mut String) -> &'a String
```

Direct calls, methods, UFCS, closures, function values, trait witnesses,
existential adapters, generated wrappers, and tail dispatch preserve that full
contract. Lifetime names are alpha-normalized, so renaming `'a` to `'input` is
harmless while changing the owner relation is not. An adapter that erases
reference kind, lifetime, affinity, or convention is rejected.

The existing owned qualifiers remain distinct from reference kinds:

| Qualifier | Meaning |
| --- | --- |
| `unique T` | uniquely owned logical storage; it can support in-place mutation |
| `local unique T` | unique storage confined to one activation; references cannot escape |
| `frozen T` | deeply immutable owned storage; `&mut` is rejected |
| `own T` | consume an owned value or reference handle |
| `let T` | call-scoped read access to a value or reference handle |
| `var T` | move-in/write-back of a value or reference handle |

`unique` is not placed inside a reference type. `&'a mut T` already describes
temporary exclusive access, while `unique T` describes owned storage. The
uniqueness proof is attached when a mutable borrow opens. `local unique` adds a
confinement boundary, and `frozen` permits shared access but not mutation.

`let`, `var`, and `own` remain orthogonal to explicit reference types. A `var`
reference parameter writes a handle back to its caller slot; it does not mutate
the referent. These combinations are valid but uncommon. Most reference APIs
use the reference type directly.

## Normal-to-opt boundaries

An opt module may publish conventional value-oriented functions to normal code:

```witchy-static
mode opt

pub fn inspect(let text: String) -> Int:
    text.length()

pub fn normalize(var text: String) -> Nil:
    text = text.trim().to_lower()

pub fn digest(own text: String) -> Digest:
    digest_string(text)
```

You call these with ordinary `let`, `var`, and `own` syntax. If what you already
own satisfies the callee, the compiler calls straight through. If it doesn't,
the compiler generates an adapter that copies, re-owns, or copies in and writes
back before entering the same function body. Either way there is one function
and one callable identity - you never maintain a second overload by hand.

An opt caller gets no such adapter; it has to satisfy the contract directly.
Anything whose type mentions a reference stays visible only to opt callers, and
a normal import rejects it - whether it reaches you as a function, an alias, a
nominal type, a callable field, a trait method, or a generated wrapper.

That firewall applies to what you can write, not to how values are represented.
A normal result of type `T` may keep an owner-backed representation after an opt
call. It is still logically owned, it outlives the source it came from, and it
detaches before anything could observe the sharing - mutation, escape,
serialization, or a return to the host. You never see a loan or a reference
diagnostic in normal code, and any boundary the compiler is unsure about copies
eagerly.

## Flow-sensitive checking

Every borrow you write creates a *loan*, which records what it borrowed from,
which projection it reached through, whether it is shared or exclusive, where it
started, and which lifetime it answers to. A loan stays live only while
something can still reach it. Using it for the last time, overwriting the owner,
materializing a copy, moving it, or transferring it all end the loan.

For overlapping places, the conflict rules are:

| Existing access | Shared read | Shared borrow | Exclusive borrow | Owner mutation or move |
| --- | ---: | ---: | ---: | ---: |
| shared loan | allowed | allowed | rejected | rejected |
| exclusive loan | through that reference | reborrow only | reborrow only | through that reference |

An exclusive reborrow suspends the parent reference until the child ends. The
compiler can prove two places are distinct when they are different record
fields, different tuple fields, or different constant indices; anywhere it
cannot tell, it assumes they overlap.

Control flow does not blunt any of this:

- branch joins retain only roots that can reach the selected arm;
- loop back-edges close body-local loans at checked last uses;
- `break` and `continue` target exact loop completion and header points;
- explicit `return` transfers only the roots in its returned reference;
- `?` carries success roots and cleans up failure paths; and
- lending iteration can return a reference tied to a mutable iterator receiver.

With a lending iterator, you have to be done with the previous item - or have
copied it - before you call `next` again. A trait implementation or function
value has to keep the same relation between the receiver's reborrow and the item
it yields.

## Escapes and suspension

Within its lifetime, a shared reference may be returned, copied into a
relation-preserving aggregate, projected, destructured, or captured by a proven
non-escaping closure. An exclusive reference can do the same only by affine
move or reborrow.

Neither kind may be:

- erased into a type with no lifetime or access-kind relation;
- converted to `Dynamic` or an owned existential without materialization;
- serialized as an address-bearing value;
- sent through a channel or isolated worker;
- captured by an escaping closure or task; or
- held across `await` or `yield`.

Capabilities need APIs that carry a lease. A lifetime on its own never widens a
grant or keeps authority alive, so borrowing a view of a capability is rejected
unless that capability's API carries both the data and its lease.

Reference capture follows the same proof boundary. A closure proven not to
escape may preserve a reference relation, while an uncertain capture is
rejected rather than silently materialized. Passing the reference as an
explicit callback argument is the portable form.

These are source-reference restrictions. Lowering may retain an owning root
behind an ordinary normal `T`, subject to the detachment rules above.

## Runtime and backend meaning

At runtime a reference is a checked `PlaceReference` value, not a note attached
to some local variable. It carries the owner it came from, the path of
projections that reached the place, whether it is shared or exclusive, whether
it has been moved, its lifetime positions, and any retain, repair, write-back,
or lease obligations its representation implies.

The interpreter represents one as a mutable owner cell plus projections.
Compiled Wasm may pick a typed carrier, a direct place descriptor, or a
cell-backed carrier that forces a copy. Whichever it picks, the meaning is the
same:

- reads follow the current logical place;
- writes update that place;
- reborrows preserve the root and extend the projection path;
- parent exclusive references remain suspended during a child reborrow; and
- cleanup balances fallthrough, return, `?`, branches, loops, moves, and drops.

The carrier survives direct calls, function values, closures, trait dispatch,
generated adapters, aggregate construction, returns, and reborrows. Lowering
never reconstructs a caller's place from how an argument happened to be spelled.
Conceptually a call takes and returns:

```witchy-static
(explicit arguments, value ownership inputs, reference access inputs)
    -> (ordinary result, var write-backs, ownership outputs, result references)
```

The interpreter, optimized Wasm, and forced-copy Wasm must agree on values,
owner mutations, write-backs, traps, accepted programs, and rejection
boundaries. `witchy parity` checks that agreement for a given program.

## Diagnostics

When a borrow is rejected, the diagnostic names the owner, the projection, the
kind of borrow, the use that keeps it alive, and the operation that conflicts
with it. It then suggests a repair: shorten the use, reborrow, copy with
`.owned()`, split the places, or move the mutation after the loan closes. You
see `&'a T` and `&'a mut T` in these messages, never internal roots, solver
facts, or carrier details.

A normal file receives the mode-boundary diagnostic before any lifetime or
loan rule runs, so a stray `&` in ordinary code reads as one clear message
rather than a cascade of borrow errors.
