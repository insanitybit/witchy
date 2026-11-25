# `memory.exchange`

```
memory.exchange(&mut a, &mut b)
```

The two places exchange their values. Both hold a value before and after, so
it works for owners and through references, where a move out would leave a
hole:

```
// Put `next` where `slot` points and hand back what was there.
fn exchange<T: Any>(slot: &mut T, next: T) -> T:
    var held = move next
    memory.exchange(slot, &mut held)
    return move held
```

Before this there was no way to replace an owner behind `&mut`: `move *slot`
is refused, rightly. That is why a growing container had to be passed and
returned by value.

Each argument is `&mut place` or a `&mut` reference. The checker requires: both
can change (a `var`, or what a `&mut` points at); both hold a value now; one
type; not a type that holds a reference; two different places; no reference
into either still in use; and the authority an assignment to each place would
need. That last rule keeps sealing: a whole sealed value may be exchanged
anywhere, but a field of a sealed or invariant-bearing type only where that
type's fields may be written. Two fields of
one value are different places when their paths part at a field
(`memory.exchange(&mut (*pair).left, &mut (*pair).right)`); anything else on
the same value is refused as overlapping.

It is in the `memory` namespace, which a program cannot define, so library
code that uses it cannot be captured by a function of the program's own. An
unqualified `swap` was tried first and was captured by a fixture's `swap`.

No heap is needed: exchanging two numbers stays in the scalar profile.

## What it made possible

Library code, no new rules:

- `Vec.reserve_mut(&mut vec, allocator, n) -> Result<(), U64>` and
  `Vec.push_mut(&mut vec, allocator, item) -> Result<(), T>`, which doubles
  the capacity when the vector is full (`library/wasm-vec.wy`). An empty Vec
  stands in while `reserve` holds the real one by value.
- `HashMap.put(&mut map, allocator, key, value)`, which moves to a table twice
  the size at three quarters full (`library/wasm-hashmap.wy`).

`tests/fixtures/exchange/grow.wy` fills a Vec from capacity 0 to 100 elements
and a HashMap from 0 cells to 50 keys. The word-count program starts its map
at 0 cells.

## Proved, and not

`reserve_mut` and `push_mut` have contracts, proved: what they do to the length and the
capacity. Their calls into the other proved Vec methods are checked as `call_precondition`
goals.

A function whose contract speaks of memory cannot use `memory.exchange`: the memory model
has no rule for it, and it is refused there rather than ignored (`exchange/proved.refused.wy`);
a contract about numbers alone, as `reserve_mut`'s, may.

## Trusted

The checker's place-disjointness rule and the emitter's sequence (find both
places, read the left into locals, write the right over it, write the held
values to the right).

## Evidence

`tests/fixtures/exchange/places.wy` (numbers, records, fields of one record,
Strings, through a reference, a generic `exchange`), `grow.wy`, and eight
refusals: the same place twice, a `let`, two types, overlapping fields, a
moved-away owner, a value with a reference into it in use, the fields of two
sealed Vecs from outside the library, inside a contract.

The sealed-field refusal was missing from the first version: the test for it
passed because its expected text also appeared in the file's name.
`scripts/run-fixtures.py` now compares refusals with the diagnostic messages
only.

## Assigning an owner over a field

```
nopanic fn swap_in<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:
    var held: Slot<T> = Vacant
    memory.exchange(&mut (*shelf).slot, &mut held)
    (*shelf).slot = Full(move item)      // the slot is proved vacant: one store
    return move held
```

`place = value` for an owner in a field, or behind a `&mut`, would lose whatever
the place holds. It is accepted when the value there is proved to hold no owner
(2026-11-21; the review is in docs/decisions.md). A plain local keeps its own
rule: it is assigned once it has been moved away.

The assignment is checked as what an author wrote by hand before it:

```
var held = value
memory.exchange(&mut place, &mut held)
match move held:
    Vacant:                 // each variant that holds no owner: nothing
    Full(owner):            // each variant that holds one
        unreachable
```

So the rules are those of the parts:

- Authority: what `memory.exchange` asks of the place (it can change, its type's
  fields may be written here, nothing borrows it, it has not been moved), asked
  after the value has been evaluated.
- Shape: the place's type is a sum with at least one variant whose value is Copy
  or may be dropped. With none, the checker refuses at once and says to use
  `memory.exchange`.
- Vacancy: each `unreachable` is an obligation of the function's proof, stated on
  what is known after the value has been evaluated. A value that lends the root
  to a call (`(*shelf).slot = Full(make(&mut *shelf))`) forgets what was known of
  the slot, and is refused; so is a call between the emptying and the assignment,
  and a slot emptied on one branch only. The refusal points at the assignment and
  names the place (see "Where a refusal points"): `an owner assigned over a value
  that may still hold one: `(*shelf).slot``.
- Place: fields, or an element at an index that is a name or a number.
- Proof: the function is proved whether or not it says `nopanic` or has a
  contract (the assignment asks for one, as an operation that needs a proof
  does). `main` has no contract, so there the assignment is refused. An owner is
  never overwritten unproved, and never by a hidden trap in a function nobody
  proved.

Once the proof is accepted the three statements are emitted as one store
(`src/compiler/vacant_store.rs`): the trapping arms are never taken, the others do
nothing, and the temporary is moved away. The rewrite asks for that shape, made by
the checker (see "The store and the guarded form"), in a function all of whose
`unreachable`s are proved; a place reached through an index keeps the exchange.

Nothing new is trusted in Lean: the obligation is the `unreachable` goal. Trusted
in the compiler: the lowering above, and the rewrite to a store, which rests on
the same record (`proved_unreachable`) that `nopanic` already rests on.

A type with an invariant: a function that writes the fields of a `&mut` parameter
uses that parameter only through its fields (docs/wasm-scalar-proofs.md), so a
half-repaired value is not lent whole (`&*x`, `&mut *x`, a method call) before the
repair is finished, and every return shows the invariant.

Fixtures: `tests/fixtures/vacant-assign/` (`refill.wy`, `refill-string.wy`,
`sealed-repair.wy` run; refused by the proof: `possibly-full`, `stale`,
`one-branch`, `lends-root`, `unproved-function`, `sealed-left-broken`; refused by
the checker: `no-vacant-variant`, `in-main`, `sealed-lent-shared`,
`sealed-lent-unique`). The list library (`library/wasm-cell-list.wy`) refills its
tail this way in `push_back`, `pop_back` and `pop_penultimate`.

## Discarding a value proved to hold no owner

```
pub nopanic fn close_empty(list: DoublyLinkedList<T>) -> Result<(), DoublyLinkedList<T>>:
    match &list.tail:
        Some(_):
            return Err(move list)
        None:
            let DoublyLinkedList(head, tail) = move list
            memory.vacant(move tail)         // proved `None`: nothing to close
            return Ok(())
```

A value of a type that can hold an owner (`Option<OwnedNode<T>>`) cannot go out of scope
unseen, even where the proof knows it holds none: the author wrote a `match` with
`unreachable` in the arm that cannot be taken. `memory.vacant(value)` is that match
(2026-11-22), written by the checker (`check/patterns.rs` `vacant_discard`) and checked as if written
by hand:

```
match value:
    Vacant:                 // each variant that holds no owner: nothing
    Full(owner):            // each variant that holds one
        unreachable
```

It is the owner assignment's rule without the place, and the rules are the same:

- Shape: the value's type is a sum with at least one variant whose value is Copy or may be
  dropped; with none, the checker refuses at once. A record, a reference to the value, and a
  value whose type is not known where it is written are refused too. A sum none of whose
  variants holds an owner needs no proof, and none is asked.
- Vacancy: each `unreachable` is an obligation of the function's proof, on what is known
  where the statement stands. A value that may be full, a call that may have refilled it and
  a value emptied on one branch only are refused: `a value discarded that may still hold an
  owner: `tail``, pointing at the statement.
- Proof: the function is proved whether or not it says `nopanic` or has a contract; `main`,
  and a function whose contract speaks of memory, are refused. An owner never ends unproved.
- It is a statement. The value may be a call (`memory.vacant(slot.drain())`): see "Matching a
  call where it is made".

**Why a statement, and not the end of the scope.** Every other end of a linear value is
written (`close`, `discard`, `move`); an implicit end would be the one place an owner's fate
depended on what the reader cannot see, and its proof obligation would have no statement to
point a refusal at, and every function holding such a local would silently need a proof.

Nothing is trusted that was not: the obligation is the `unreachable` goal (the same
statements as the match written by hand, `rearrangement_tests.rs`), and what is emitted is
that match. Nothing is emitted on the strength of the proof except what `unreachable` already
is under `nopanic`.

Fixtures: `tests/fixtures/vacant-discard/` (`discard.wy` runs, with a String; refused by the
proof: `possibly-full`, `stale`, `one-branch`, `unproved-function`; by the checker:
`no-vacant-variant`, `not-a-sum`, `reference`, `as-a-value`, `in-main`, `moved-twice`). The
pointer list's `close_empty` is written this way; its client's module and goals are the same,
byte for byte (8,174 bytes, 134 goals). The older list (`library/wasm-list.wy`) keeps its
match: its `close_empty` is not proved, and tests `is_some()`, which the proof does not read.

## A helper that moves an owner

```
// library/wasm-option.wy
pub nopanic fn take(slot: &mut Option<T>) -> Option<T>:
    var previous: Option<T> = None
    memory.exchange(&mut *slot, &mut previous)
    return move previous

// the list, before                                   // and after
var tail: Option<OwnedNode<T>> = None                 let tail = (*list).tail.take()
memory.exchange(&mut (*list).tail, &mut tail)
```

`Option.take`, `Option.replace` and `replace(&mut place, next)` (`import replace`) are ordinary
Witchy, three lines over `memory.exchange`. Nothing in the compiler names them. Two rules make a
call of such a helper worth what the exchange written in place was worth (2026-11-21; the
decision is in docs/decisions.md).

**Lending the field.** `&mut (*list).tail` hands the callee the authority to write the tail. For a
type whose invariant reads storage through a method, that was refused outright: a reference
kept past the point where the invariant is shown again could break it unseen. It is now accepted
when the reference cannot outlive the call, which the types decide (`check/locals.rs`
`ends_at_return`): the callee's result holds no reference, and no parameter holds one except a
plain reference to a value that holds none. Then every write through it happens before the call
returns, and the lend is treated as `memory.exchange` on the field is: only through a `&mut`
parameter, only on that parameter's own type, only in a function proved over numbers; the
parameter is used through its fields alone until the return (not lent whole, not read whole, no
method called on it), and every return shows the invariant. A reference bound to a name
(`let slot = &mut (*list).tail`) and a helper that hands the reference back, plainly or inside a
value, keep the refusal. This is a rule about types, so a function called `take` that returns
`&mut` is refused like any other.

**Knowing where the values went.** A `&mut` lent to a call used to make the caller's proof forget
everything under it. For a callee whose body only moves values around, the proof now carries
what it knew of each value to where the callee put it, exactly as it does for an exchange
written in the caller. The summary (`rearrangement.rs`) is read from the callee's checked body,
by its definition:

- the body is a straight line of: a local set to a moved local, a copied local, `()` or a variant
  built around one of those; `memory.exchange` between two whole places, each a local or the
  whole of what a `&mut` parameter points at; a call of a function that has a summary (below);
  and one final `return` of such a value or call;
- anything else has no summary: a branch, a loop, a match, a call of a function that has none,
  arithmetic, a field or an element of a place, an assignment through the reference;
- the summary says, for the result and for what each `&mut` parameter points at afterwards, which
  value given to the call is there (what a `&mut` parameter pointed at on entry, or a by-value
  argument), or which variant was built around one.

At the call (`scalar_vc/calls.rs` `call`), what is known of each such value is moved to that place, a
built variant's tag is the literal the callee wrote, and the callee's `ensures`, if it has any,
are then stated of those same values. So after `take` the tail is known to be `None` and the
result is the old tail with everything the invariant said of it; after `replace(&mut place, v)`
the place holds `v` and the result is what was there. With no summary the caller forgets, as
before. A summary moves knowledge and grants nothing: whether the callee may perform each step
was decided when its body was checked, and whether the caller may lend the place is decided by
the rule above.

**A helper that calls helpers** (2026-11-22). A call in such a body is followed when the
function called has a summary: `fn empty(slot) -> Slot<T>: return drain(&mut *slot)` has
`drain`'s. The callee's summary is composed: each value it was given is the piece this body
held there (a by-value argument that is itself a move, a copy, `()` or a variant; or, for a
`&mut` parameter, the whole place lent, `&mut local`, `&mut *parameter` or the `&mut`
parameter handed on), and what it leaves in each place and returns is written in those terms.
The call is `let name = call(..)`, `return call(..)`, or a statement when it returns `()`.
Not followed, so no summary: a callee without one; a shared reference among the arguments; a
field, an element or a computed argument; a call inside another value; a function reached
again from inside its own body (directly or through others), and anything that calls one;
more than four bodies one inside another, or thirty-two in all. The caller then forgets what it
lent, as before. A summary is still read from the checked bodies each time a caller's
obligations are generated, so a changed inner helper changes the caller's statements.

**Matching a call where it is made** (2026-11-22). `match slot.drain():` knows what `let old =
slot.drain()` followed by `match old:` knows: which variant came back, and what it holds. The
proof generator gives the arms of a match on a call exactly what the `let` would have written
to its local and the match read back from it (`scalar_vc/flow.rs` `select`); a call with no such
knowledge is as it was, and its goals are unchanged.

A helper may still state its effect as a contract (`ensures when Full = *slot:` / `false`), and a
caller may use that instead. A false claim is refused where the helper's body is proved, before
any caller is looked at.

What goes stale: a later `&mut` lend of the place to a callee with no summary forgets it again; a
callee with a summary that fills the place says so. An assignment over the field after either is
refused by the proof.

Trusted: the `ends_at_return` rule; the summary derivation and its use at a call, which is a
second reading of `memory.exchange`, moves and variant construction beside the one the proof
already had for code written in place. Nothing new is trusted in Lean, and no goal changed
shape. The emitted code is the call (written into the caller by the existing inliner when
small): no check is dropped on the strength of a summary.

Fixtures: `tests/fixtures/owning-helpers/`. Run: `slot-drain.wy` (a user `Slot<T>.drain()`, a
String payload, a sealed shelf with an invariant; drain then refill, drain and leave empty,
`replace`), `view-take.wy` (a sealed holder whose invariant reads a cell through a method),
`library-take-replace.wy`, `direct-match.wy`, `composed-helper.wy`. Refused by the proof:
`false-vacant-claim` (in the helper), `helper-leaves-full`, `helper-may-refill`,
`refill-between`, `left-broken`, `view-left-broken`, `view-refill-between`,
`direct-match-wrong`, `direct-match-opaque`, `composed-helper-opaque`,
`composed-helper-recursive`, `composed-helper-leaves-full` (in the caller). Refused by the checker: `moved-owner-reused`,
`observed-half-repaired`, `view-observed-half-repaired`, `view-retained-borrow`,
`view-exported-borrow`. Unit tests: `src/compiler/rearrangement_tests.rs`.

## Where a refusal points

A proof that fails names the function, lists what was not proved, and points at the first
part that has a place of its own (2026-11-22):

```
proof failed: function `refill`
  not proved: an owner assigned over a value that may still hold one: `(*shelf).right`
  not proved: an owner assigned over a value that may still hold one: `(*shelf).left`
shelf.wy:16:5: W0402: this function's proof did not go through: what was not proved is listed
above; `(*shelf).right` may still hold an owner when this assigns over it, and that owner
would be lost (not proved either: shelf.wy:17)
      (*shelf).right = Full(move b)
```

An owner assignment and a `memory.vacant` are pointed at and their place named; an
`unreachable` that some path reaches is pointed at; a false `ensures` clause (a helper's claim
of what it leaves) is pointed at where it is written. Up to four other parts are named by
their lines. Anything else (arithmetic, an index, a loop invariant) points at the function,
as before. Where it was written travels with the `unreachable` statement to the obligation
and is no part of the goal: no statement changed, and nothing new is accepted.
`messages/proof-points-at-assignment`, `-unreachable`, `-claim` (`.refused.wy`); the line and
column are checked in `tests/wasm_cli.rs`.

## The store and the guarded form

Only the three statements the checker itself wrote for an assignment become one store: the
hidden local they exchange through is marked (`Function::overwrites`), and source code cannot
name or mark one. The same shape written by hand is emitted as written
(`vacant-assign/hand-effect.wy`, `hand-mixed.refused.wy`, `vacant-assign/hand-unproved.traps.wy`).

`WITCHY_AUDIT_GUARDED_STORES=1` in the environment of whoever runs the compiler emits every
assignment in its guarded form (the exchange, the match and the trapping arms). No program,
package or policy can set it. `scripts/check-guarded-stores.py --compiler BIN --runtime HOST
--binding FILE` builds the vacant-assign and owning-helper fixtures and the four-node list client
both ways and requires the same output, a trap both ways for the unproved lookalike, and two
different modules where an assignment is known to be present.
`scripts/run-fixtures.py` runs it after the fixtures whenever accepted fixtures of those two
directories are among them and a binding is given (all of them: the list client too), and fails
if it does; `--no-guarded-stores` leaves it out.

## Also

`x *= y`, `x /= y` and `x %= y` are `x = x * y` and so on, for a target whose
reading runs nothing: a name, a field, or an element at a named index
(`tests/fixtures/arithmetic/compound.wy`).
