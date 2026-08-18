# Opt-mode References and Lifetimes

RFC-0122 adds a lower-level ownership model without changing ordinary Witchy.
Normal files remain reference-free: they use owned values and the conventional
`let`, `var`, `own`, and `move` call vocabulary. A file opts into explicit
references with a leading directive:

```text
mode opt
```

This chapter is the user-facing guide to the complete RFC-0122 feature set. The
[performance appendix](appendix-performance.md) explains the optimization
passes and counters. The [RFC-0122 acceptance
ledger](https://github.com/insanitybit/witchy/blob/master/rfcs/0122-acceptance-ledger.md)
records the executable evidence for each criterion.

The repository's `scratch/learn-witchy-2026-08-18/` learning corpus is useful
for trying these forms, but the acceptance ledger remains the implementation
authority. It highlights three practical distinctions:

- The reference type is still `&'a T` or `&'a mut T`. The current executable
  `mode opt` validation also requires an explicit `let`, `var`, or `own`
  convention on ownership-relevant reference parameters. For example,
  `let text: &'a String` qualifies the reference handle; it does not turn the
  type back into the retired `let('a) String` form.
- Legacy `let('a) T` and `View(T, 'a)` inputs still run through the migration
  compatibility path. They are accepted as migration inputs, not current API
  spellings, and new code should use `&'a T`.
- Closure capture is conservative in the current implementation. The scratch
  escape probe is rejected; pass a reference explicitly to a callback or call
  `.owned()` before capture unless a positive non-escaping closure fixture
  proves the boundary you need.

## Two modes, one language

Normal mode has no lifetime burden. It cannot name, infer, accept, return, or
store a source reference. It also cannot be rejected because an internal
optimization loan was difficult to prove. A normal call into an opt module uses
the same value-oriented signature as any other call:

```text
fn inspect(let text: String) -> Int
fn normalize(var text: String) -> Nil
fn digest(own text: String) -> Digest
```

Opt mode keeps those conventions and adds first-class access types when access
itself must be returned, stored, reborrowed, or passed through a lending API.
The mode boundary is therefore a performance escape hatch, not a second source
language. An explicit reference in a normal file receives one concise
mode-boundary diagnostic before lifetime or loan analysis begins.

## Reference types

The two reference kinds are:

```text
&'a T          shared, read-only access valid for 'a
&'a mut T      exclusive, mutable access valid for 'a
```

Every lifetime written in an opt-mode declaration is explicit. Borrow
expressions infer their local lifetime:

```text
fn first(text: &'a String) -> &'a String:
    text

fn normalize(text: &'a mut String) -> &'a String:
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

```text
let text = make_string()
let view = &text
let field = &mut account.name
let item = &items.at(index)
```

Borrowing an unbound temporary is rejected. Bind the owner first rather than
depending on a temporary-lifetime extension:

```text
let bad = &make_string()       // error: bind the owner first
```

Dereference and projection operate on the same logical place. Reading through
either kind is allowed; assignment through a shared reference is not:

```text
fn clear(text: &'a mut String) -> Nil:
    *text = ""

fn rename(account: &'a mut Account) -> Nil:
    account.name = account.name.trim()
```

The second form is projection sugar for `(*account).name`. It does not copy,
materialize, or widen the reference.

## Shared and exclusive access

Shared references are copyable handles to read-only access. Several overlapping
shared references may coexist, but the owner cannot be moved, consumed,
reassigned, mutated, dropped, or mutably borrowed while one remains live:

```text
var text = "hello"
let left = &text
let right = &text
inspect(left)
inspect(right)
text = "done"                 // valid after both final uses
```

Exclusive references are affine. They grant read and write access to one place,
exclude overlapping access, and may be moved but never copied:

```text
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

```text
fn append_mark(text: &'call mut String) -> Nil:
    text.push("!")

fn twice(text: &'outer mut String) -> Nil:
    append_mark(text)
    append_mark(text)
```

`own` on an exclusive-reference parameter consumes the reference handle rather
than creating a reborrow. Consuming a shared handle ends that handle but does
not consume its owner.

## Reborrowing and lifetime relations

An exclusive reference can be shortened to a shared reference. The exclusive
capability is not recoverable through that shared result:

```text
fn finish(text: &'a mut String) -> &'a String:
    text.trim_in_place()
    text

fn inspect_then_edit(text: &'outer mut String) -> Nil:
    let view = &*text
    inspect(view)
    text.push("!")
```

An exclusive projection can also return an exclusive reference to a field:

```text
fn name(account: &'a mut Account) -> &'a mut String:
    &mut account.name
```

Lifetime names describe public relations, not concrete durations. A result
lifetime must be reachable from an input relation. The checker infers concrete
borrow duration, root provenance, reborrow shortening, branch joins, loop
fixpoints, overwrites, and final uses. There is no `'static` spelling or
general outlives clause in this model.

Distinct lifetime names express independent result dependencies:

```text
fn left(left: &'left String, right: &'right String) -> &'left String:
    left
```

The same lifetime on multiple inputs means every possible result must remain
valid for that relation. It does not prove that two exclusive inputs are
disjoint; unknown overlap remains an error.

## Nominal types, containers, and generics

Nominal lifetime parameters are retained only for relations stored in fields:

```text
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

```text
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

```text
fn identity(value: &'a t) -> &'a t:
    value

fn swap(left: &'a mut t, right: &'b mut t) -> Nil:
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

```text
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

| Qualifier | Meaning with RFC-0122 |
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

```text
mode opt

pub fn inspect(let text: String) -> Int:
    text.length()

pub fn normalize(var text: String) -> Nil:
    text = text.trim().to_lower()

pub fn digest(own text: String) -> Digest:
    digest_string(text)
```

The caller uses ordinary `let`, `var`, and `own` syntax. The compiler selects a
proven access entry when normal ownership facts suffice. Otherwise it selects
a generated repair adapter that establishes the required ownership through
copying, re-owning, or copy-in/write-back before entering the same source body.
There is one source function and one callable identity, not a hand-maintained
second overload.

An opt caller must satisfy the proven access contract directly. A reference-
typed item remains visible only to opt callers. Normal imports reject direct
and alias-hidden reference functions, reference-bearing nominal types, callable
fields, traits with reference methods, and generated wrappers that expose a
reference contract.

The source firewall is not a representation firewall. A normal result with
source type `T` may temporarily use an owner-backed representation after an
opt call. It remains logically owned, survives source drop, and detaches before
mutation, independent escape, serialization, host return, or another operation
that could observe aliasing. Normal code never receives a loan or a reference
diagnostic. Unsupported or uncertain boundaries materialize eagerly.

## Flow-sensitive checking

The checker separates symbolic origins from concrete loans. A loan records its
owner root, projection, access kind, introduction point, and lifetime relation.
The analysis is point-sensitive: a loan is live only while it can reach a live
origin and has not been killed by final use, overwrite, materialization, move,
or transfer.

For overlapping places, the conflict rules are:

| Existing access | Shared read | Shared borrow | Exclusive borrow | Owner mutation or move |
| --- | ---: | ---: | ---: | ---: |
| shared loan | allowed | allowed | rejected | rejected |
| exclusive loan | through that reference | reborrow only | reborrow only | through that reference |

An exclusive reborrow suspends the parent reference until the child ends. Static
record fields, tuple fields, and distinct constant indices form the initial
disjointness proof set. Unknown projection overlap is treated as overlap.

The analysis is precise across structured control flow:

- branch joins retain only roots that can reach the selected arm;
- loop back-edges close body-local loans at checked last uses;
- `break` and `continue` target exact loop completion and header points;
- explicit `return` transfers only the roots in its returned reference;
- `?` carries success roots and cleans up failure paths; and
- lending iteration can return a reference tied to a mutable iterator receiver.

For a lending iterator, calling `next` again requires the prior item to be
dead or materialized. A trait witness or function value must preserve the
relation between the receiver reborrow and yielded item.

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
- held across `await` or `yield` in the current model.

Host capabilities require lease-bearing APIs. A lifetime does not widen a
grant or keep authority alive on its own. A borrowed capability view is rejected
unless the capability-specific API transports both data and its lease.

Reference capture follows the same proof boundary. A closure that is proven
not to escape may preserve a reference relation in the RFC model, but an
uncertain or currently unsupported capture is rejected rather than silently
materialized. Passing the reference as an explicit callback argument is the
portable form until a positive closure-carrier fixture covers the exact shape.

These are source-reference restrictions. Lowering may retain an owning root
behind an ordinary normal `T`, subject to the detachment rules above.

## Runtime and backend meaning

The runtime value is a checked `PlaceReference`, not metadata attached to a
local variable. It carries an owner root, projection path, shared or exclusive
kind, affine state, lifetime positions, and any representation-specific
retain, repair, write-back, or lease obligations.

The interpreter realizes a place reference as a mutable owner cell plus
projections. Compiled Wasm may use a typed carrier, direct place descriptor, or
forced-copy cell-backed carrier. All carriers have the same meaning:

- reads follow the current logical place;
- writes update that place;
- reborrows preserve the root and extend the projection path;
- parent exclusive references remain suspended during a child reborrow; and
- cleanup balances fallthrough, return, `?`, branches, loops, moves, and drops.

The carrier crosses direct calls, function values, closures, trait dispatch,
generated adapters, aggregate construction, returns, and reborrows. Lowering
never recovers a caller place from the spelling of an argument after a call.
The access envelope is conceptually:

```text
(explicit arguments, value ownership inputs, reference access inputs)
    -> (ordinary result, var write-backs, ownership outputs, result references)
```

The interpreter is the semantic oracle. Optimized Wasm and forced-copy Wasm
must agree with it on values, owner mutations, write-backs, traps, accepted
programs, and rejection boundaries. `witchy parity` and the RFC-0122 fixtures
exercise this three-way contract.

## Diagnostics and migration

Reference diagnostics identify the owner, projection, borrow kind, live use,
conflicting operation, and a repair such as shortening the use, reborrowing,
materializing with `.owned()`, splitting places, or moving mutation after the
loan closes. Diagnostics expose `&'a T` and `&'a mut T`, not internal roots,
solver facts, or carrier details.

The settled syntax replaces the retired relation spellings:

| Retired spelling | Current spelling |
| --- | --- |
| `let text: let('a) String` | `text: &'a String` |
| `let('a) text: String` | `text: &'a String` |
| `var('a) text: String` | `text: &'a mut String` |
| `View(String, 'a)` | `&'a String` |
| `String('a)` when `String` has no lifetime parameter | `&'a String` |
| `List(Token('a))` | `List(&'a Token)` |

Declared nominal forms such as `Parser('input)` remain valid when the lifetime
is used by a field. A normal file receives the mode-boundary diagnostic before
any of these migration or loan rules run.

The migration command is AST-based and authenticated by owner provenance. It
rewrites only proven direct places, reports conflicting overloads instead of
guessing, supports check-only mode, and never mutates an ambiguous source. The
repository census and command evidence are recorded in the
[migration report](https://github.com/insanitybit/witchy/blob/master/rfcs/0122-migration-report.md).

## RFC-0122 feature map

The book coverage maps to the RFC's 22 acceptance criteria as follows:

| Criterion | Feature | Book coverage |
| ---: | --- | --- |
| 1 | normal-mode reference exclusion | Two modes, one language; Diagnostics |
| 2 | conventional normal calls | Normal-to-opt boundaries |
| 3 | normal interface filtering | Normal-to-opt boundaries |
| 4 | one opt source identity | Function values and ownership qualifiers; Normal-to-opt boundaries |
| 5 | proven versus repair parity | Normal-to-opt boundaries; Runtime and backend meaning |
| 6 | owner-backed normal results | Normal-to-opt boundaries |
| 7 | opt syntax pipeline | Reference types; Diagnostics and migration |
| 8 | uniform reference types | Reference types; Nominal types, containers, and generics |
| 9 | nominal lifetime versus reference | Nominal types, containers, and generics |
| 10 | migrated fixture parity | Diagnostics and migration; Runtime and backend meaning |
| 11 | shared loans | Shared and exclusive access; Flow-sensitive checking |
| 12 | exclusive loans | Shared and exclusive access; Reborrowing and lifetime relations |
| 13 | mutable-to-shared conversion | Reborrowing and lifetime relations |
| 14 | owned qualifiers | Function values and ownership qualifiers |
| 15 | convention/reference orthogonality | Function values and ownership qualifiers |
| 16 | preservation of the opt graph | Function values and ownership qualifiers; Normal-to-opt boundaries |
| 17 | CFG precision | Flow-sensitive checking |
| 18 | aggregate affine roots | Nominal types, containers, and generics |
| 19 | interpreter/Wasm parity | Runtime and backend meaning |
| 20 | async and escape boundaries | Escapes and suspension |
| 21 | migration command | Diagnostics and migration |
| 22 | performance telemetry | [Performance appendix](appendix-performance.md) and the acceptance ledger |

The implementation status belongs to the acceptance ledger, not to prose in
this chapter. The ledger currently marks all 22 criteria `PROVEN` on `master`
with named executable evidence.
