# Shared references

`&T` is a value: it can be returned, bound with `let`, held in a variant
(`Result<&T, U64>`, `Option<&T>`) and passed on. It points at a value that stays
where it is.

```
impl<T: Any> Vec<T>:
    fn at(value: &Vec<T>, index: U64) -> Result<&T, U64>

match names.at(index):
    Ok(name):
        console.print_int(name.byte_len())
    Err(code):
        console.print_int(code)
```

`tests/fixtures/references/elements.wy` reads a `Vec<String>` in place, hands a
reference back through a second function, and grows the vector again once the
reference is no longer used.

## The rule

A call whose result holds a reference keeps what its arguments borrowed lent.
Containment is recursive: `Result<Option<&U64>, U64>` and deeper declared sums
keep the same loans through constructors, copies, calls and matches.
The loan passes to whatever holds the result:

- a `let` keeps it until the last statement of its block that names it;
- a `match` binder keeps it until the last statement of its arm that names it;
- a copy of a reference, or a reference returned by a call that was given one,
  keeps the same loan;
- anything else ends it with the statement.

While a value is lent it can be read and lent again with `&`. It cannot be
written, moved, closed or lent with `&mut`: "this value is lent: a reference
into it is still in use".

A record may hold `&` references (it is then a view, and keeps their loans while it is
used). A reference cannot be stored in a `Buffer` or an array, and a plain reference
variable cannot be assigned again. A reference may point at a view record, but not at a
sum that itself contains references. The checker tracks
loans on bindings and match payloads; it does not implement lifetimes for stored
references.

A function may return a reference only if every loan it depends on is on one of
its own `&` parameters or references carried in its by-value sum parameters.
Borrowing the address of the parameter itself still cannot escape the function.
The caller keeps all its borrowed arguments lent, so it does not matter which parameter the result came from. A function that returns a
reference may also take a `&mut` parameter that the result cannot point into (below).

Mutable borrows delegate write permission. Borrowing a field or an indexed
element with `&mut` requires permission from every sealed record along the
projection, just like assignment. This also applies to automatic method receiver
borrows and memory primitive arguments. Borrowing a whole sealed value for its
public API remains allowed; changing an invariant-bearing field in its defining
file still requires a supported proof that covers the mutation.

## Where references come from

`memory.ref_unchecked(&buffer, index)` gives the address of a cell. Like
`memory.get_unchecked` it is allowed only in a function with a contract and a
proof, and the proof must show the index is in bounds and the cell holds a
value. In a proof a shared reference stands for the value it points at: the
buffer cannot change while the reference is in use, so `memory.cell(buffer,
index, result)` says the same thing for a reference as for a copy.

`Vec.at` checks the index at run time and calls it; the type invariant of
`Vec` says the cells below the length hold values.

`&v[i]` and `&mut v[i]` lend one element of a container that is no array or slice:
they are the container's `cell(i)` and `cell_mut(i)`, a reference to the element or, past
the end, a trap, as `v[i]` reads one. `Vec` has both. A program's own container may
define them; they must take the receiver by `&` (`&mut`) and an index, and return a reference
of the same kind, so the element is borrowed from the container, which stays lent while the
reference is used. `v.at(i)` is the form that gives `Err` instead of trapping.
(`tests/fixtures/vec-access/lend-element*`, `element-borrow-*`.)

## Borrowing a value that has no name

`double(&21)`, `area(&Square(5))` and `Point(3, 4).sum()` are accepted. The
value is written to a frame cell of its own, made for that one expression,
and the reference points at the cell. Two temporaries in one call have a cell
each; one inside a loop is written again each time round. The cell lasts as
long as the function's frame, so a reference the call hands back stays good.

Only shared borrows of Copy values work this way. `&mut 3` is refused (the
change would be lost), and so is `&make_string()` (the owner could not be
consumed afterwards). A function with a contract borrows named values only.
Fixture: `tests/fixtures/references/temporary.wy`.

## Checked and trusted

Proved: `Vec.at` against its contract; three refusal controls (off-by-one
bound, wrong cell, no bound check).

Tested, not proved: the lending rule itself (`src/compiler/references_tests.rs`).
The rule is part of the checker, which is trusted, as is the emitted address
arithmetic.

## Unique references

`&mut T` is a value too: it can be returned, bound with `let`, held in a
variant and passed on.

```
impl<K: Any + Hash + Eq, V: Any> HashMap<K, V>:
    fn get_mut(map: &mut HashMap<K, V>, key: &K) -> Result<&mut V, U64>

match counts.get_mut(&word):
    Ok(seen):
        *seen += 1
    Err(code):
        ...
```

The rules, beyond those for `&T`:

- While a `&mut` reference is in use, what it borrows is not read, written,
  moved or lent at all. The loan ends at the last use, as for `&T`.
- It is never copied. `let s = r` and `f(r)` lend `r` on: `r` cannot be used
  until the last use of `s`, or until the call returns, and then it can again.
  A variant that holds one is moved, once.
- One reference is not given to a call twice (`two(r, r)`).
- A function returns a `&mut` reference only if it borrows from one of its own
  `&mut` parameters (or a reference carried in a by-value parameter). The
  caller keeps every borrowed argument lent while the result is used.
- A function that returns `&` references may take a `&mut` parameter only when its result
  cannot point into it.
- A kept `&mut` reference does not reach a field that an invariant of its type
  reads, in the defining file either: the writes made through it later would
  not be seen by the proof that keeps the invariant. A field no invariant reads
  is lent, in the type's own file and in a function proved over numbers
  (`(*ring).slots.at_mut(i)` beside `invariant count <= capacity`): the proof
  forgets that field from the lend on and keeps what it knows of the others
  (`tests/fixtures/sealed-storage/`; decisions.md, 2026-11-22). The other way
  in is a proved `memory.mut_unchecked`, below. Nor does it reach inside a sealed type from
  outside its file. In a sealed type's own file, with no invariant, the fields
  are the file's to write and so to lend
  (`tests/fixtures/references/unique-own-sealed.wy`; the refusals are
  `unique-sealed-outside.refused.wy` and `unique-sealed-invariant.refused.wy`).
  A reference to the whole value is fine anywhere, since only the type's own
  functions can use it.
- A function with a contract holds no `&mut` reference as a value, except in
  what a function proved about a Buffer returns. Its proof reads the body in
  order and would not know which value a later write changes.
- Type parameters are not instantiated with references, so generic code that
  copies a `T` cannot copy one.

`memory.mut(&mut buffer, index)` is the checked source: `Result<&mut T, U64>`,
`Err` when the index is out of range or the cell is empty. It is the same
allocator operation as `memory.ref`.

How it is checked (`src/compiler/check/loans.rs`): each lent root has a count of shared
loans and a count of unique ones. A new unique loan needs both to be zero. A
value derived from a `&mut` local takes a unique loan on that local and repeats
the loans the local holds, so the source stays lent for as long as either is
used. `tests/fixtures/references/unique.wy` runs in Wasmtime; seven refusal
fixtures sit beside it and `src/compiler/unique_reference_tests.rs` checks 30
programs, 21 of them refusals.

Trusted: this rule, like the shared one, is part of the checker and is tested,
not proved. Nothing changed in the emitted code's shape: a `&mut` value is an
address, as a `&mut` parameter already was.

## A `&mut` reference to an element of a sealed container

```
impl<T: Any> Vec<T>:
    fn at_mut(value: &mut Vec<T>, index: U64) -> Result<&mut T, U64>

match names.at_mut(1):
    Ok(name):
        name.push_text(allocator, " changed")?    // a String inside a Vec<String>
    Err(code):
        ...
```

`memory.mut_unchecked(&mut buffer, index)` gives the address of a cell for
writing. It is allowed only in a function with a contract and a proof about a
Buffer, and only in the value the function returns. The proof must show the
index in bounds and the cell occupied, as for `memory.ref_unchecked`.

The holder of the reference may store any value of the element type in the
cell after the function has returned. So from the point where the reference is
made the proof treats the cell as holding an arbitrary value: one unknown,
quantified over all values. Everything proved afterwards, the type's invariant
included, therefore holds whatever is stored later. The cell stays occupied: a
value cannot be moved out through a reference.

Consequences, each with a control:

- `Vec.at_mut` proves: `Vec`'s invariant speaks of which cells are occupied,
  not of what they hold.
- It cannot claim the buffer is unchanged, or that the old value is the one
  referred to (`scripts/check-vec-refusals.py`, six `Vec.at_mut` cases).
- A type whose invariant names a cell's value can hand out `&` to that cell and
  not `&mut`: `tests/fixtures/references/invariant-value.wy` proves and
  `invariant-value-mut.refused.wy`, the same function with `&mut`, is refused.

While the reference is in use the vector is lent uniquely, so it cannot grow
and move its cells under the reference. `tests/fixtures/references/elements-mut.wy`
changes numbers, appends to a String inside a `Vec<String>`, and runs in Wasmtime.

Trusted here: the model rule itself (`src/compiler/memory_model.rs`, the
`MutUnchecked` arm), that a write through `&mut T` stores a whole `T` and
cannot empty the cell, and the borrow rule that keeps the vector lent.

## Fields and elements through a reference

```
fn larger(pair: &Pair) -> U64:
    if pair.left > pair.right:
        return pair.left
    return pair.right

fn clear(heap: &mut Heap):
    heap.count = 0
    heap.items.truncate_mut(0)
```

`r.field` and `r[i]`, where `r` is a reference, mean `(*r).field` and
`(*r)[i]`. The checker writes the long form out before it does anything else
with the expression (`src/compiler/check/places.rs`, `through`), so every rule is the one
for the long form: a field of `&T` cannot be assigned, a value cannot be moved
out, loans are counted as before.

`src/compiler/through_reference_tests.rs` compiles fourteen programs both ways.
Accepted, the two modules are the same bytes; refused, the same refusal.
`tests/fixtures/programs/paths.wy` (shortest paths with a binary heap over
`Vec<Vec<Edge>>`) is written this way and runs in Wasmtime.

A reference used whole still needs its `*`: `*r = value`, `*r + 1`. The proof
models read the checked form of a body, so a proved function may use the short
form too: `Vec.push_spare_mut` rewritten with `vector.length` was checked by
Lean afresh and ran. Contract clauses already named fields this way.

## `match` through `&mut`

```
fn add(doc: &mut Doc, allocator: &Allocator, more: Text) -> Result<(), U64>:
    match doc:
        Words(words):
            return words.push_text(allocator, more)
        Many(items):
            ...items.push_mut(allocator, move made)...
        Nothing:
            return false
```

Matching a `&mut` reference to a sum leaves the sum where it is and makes each
arm's name a `&mut` reference to the value its variant holds, as `&` already
made it a shared one. A sum that owns a String or a Vec is changed in place;
before, it had to be taken apart and rebuilt.

The danger is the name outliving its variant: replace the sum and the name
would refer to another variant's value. The name takes over the matched
reference's unique loan, by the same code that gives a shared name its loan,
and holds it until its last use in the arm. Until then the sum cannot be read,
written, replaced, moved or lent; a use inside a loop counts as a use after
everything in the loop. After the last use the arm may replace the sum
(`*light = Red`).

Limits. An arm names one of the values of a variant that holds several and
writes `_` for the others, because loans are counted by the value borrowed and
not by field; hold the values in a record (`Sized(Size)`) to change several.
A function with a contract cannot match this way. The scalar proof walk treats
whatever the matched reference borrows as unknown from the match on.

`src/compiler/match_unique_tests.rs`: eight programs accepted, eleven refused.
`tests/fixtures/match/through-mut.wy` runs in Wasmtime, with two refusals
beside it. Trusted: this rule in the checker; it is tested, not proved.

## Not yet

A reference that a call returns may be
passed straight on: `f(names.at(0))`. Functions with
a contract can call `Vec.at` and read what it returns as an unknown value; they
cannot yet hold a `&mut` reference as a value, so not `Vec.at_mut` or
`HashMap.get_mut`. `&mut` cell references are not taken inside a proved loop.

## What a `&mut` result keeps lent

```
match groups.get_mut(&name):
    Ok(group):
        name.close()                    // the key is free
        group.push_mut(allocator, move word)
```

A call whose result holds a reference keeps its reference arguments lent while
the result is in use. When the result's only references are `&mut` (`&mut T`,
`Result<&mut V, U64>`), that is narrowed to the arguments borrowed with `&mut`:
what the call borrowed with `&` is free as soon as the call returns
(`src/compiler/check/calls.rs`).

Why that is the same program: a `&mut` in the result has to come from
somewhere the function could write. It cannot come from a `&` parameter,
because no `&mut` is made from a `&`
(`tests/fixtures/references/unique-key-sneak.refused.wy`), and a `&` points at
no value that itself holds a reference, so nothing unique is reachable through
one. It cannot come from the function's own locals, which a returned reference
may not point into. That leaves the `&mut` arguments, which stay lent
(`unique-key-map.refused.wy`). A result that can also hold a `&`, such as
`Result<&mut U64, &U64>`, keeps everything lent as before
(`unique-key-mixed.refused.wy`).

`unique-key-free.wy`, `unique-key-wrapped.wy` and `programs/anagrams.wy` run in
Wasmtime. Like the rest of the borrow checker this rule is tested, not proved,
and it leans on the two refusals named above staying in place.

## A reference last used in an `if` condition

```
for letter in &letters:
    if let Err(_) = made.push(allocator, *letter):
        letters.close()                 // `letter` is not used from here on
        return Err(1)
```

A name that holds a reference keeps its value lent until the last statement of
its block that uses it. When that statement is an `if` and the name appears in
the condition only, in neither branch, the loan ends once the condition has
been evaluated, so a branch may change or consume what was borrowed.

Why that is the same program: the name is bound in this block, so nothing
reads it after this statement, in this pass of a loop or a later one; the
branches do not name it; and a copy of the reference under another name holds
the loan separately. A `while` condition is read again after the body and is
not treated this way (`tests/fixtures/references/last-use-while.refused.wy`).
A reference used in either branch or after the `if` keeps the loan
(`last-use-else.refused.wy`, `last-use-later.refused.wy`,
`last-use-for-branch.refused.wy`). `last-use-if.wy`, `last-use-nested.wy` and
`last-use-for.wy` run in Wasmtime. Tested, not proved.

A `match` is treated the same way: a name used in the matched value and in no
arm is free in the arms (`last-use-match.wy`, `last-use-match-value.wy`). When
the match is through a reference, each binder is a reference into the matched
value and lends again for itself until its own last use
(`last-use-match-binder.refused.wy`); a name an arm reads keeps its loan in
every arm (`last-use-match-arm.refused.wy`).

## What a result can point into, by type

```
match groups.get(&name):                // HashMap<String, U64>
    Ok(count):
        name.close()                    // the key is free: `count` points into the map
for entry in &map:                      // `next(&map, &mut cursor)` returns `&Entry`
```

After a call whose result holds a reference, a reference argument stays lent
only if the result could point into it. That is decided from the function's
signature as it is written, with two facts:

- a `&mut` in the result comes only from a `&mut` parameter (the section "What
  a `&mut` result keeps lent" above);
- a reference to an `A` points only into a parameter whose type can hold an
  `A`: the type itself, or a field, a variant's value, or an array or buffer
  element of it, at any depth (`can_hold` in `src/compiler/semantic_types.rs`).

The signature's own type parameters are judged as the function sees them:
opaque. `fn get(map: &HashMap<K, V>, key: &K) -> Result<&V, U64>` cannot turn
a `&K` into a `&V`, because inside `get` they are different types whatever
they stand for at a call, so the key is free even in a `HashMap<U64, U64>`.
The one way a function reaches inside a value of a type parameter is a trait
method. A trait's signature names `Self`, fixed types and the trait's own type
arguments, so a parameter of type `&T` is taken to be able to yield a reference
to any type that mentions no type parameter other than `T` and those `T`'s
bounds give as type arguments, and theirs in turn: with `B: Leak<A>`, a `&B`
can yield an `&A`, and with `B: Wrap<C>` and `C: Leak<A>` it can too, in two
calls (`points-trait.refused.wy`, `trait-argument-loan.refused.wy`,
`trait-argument-chain.refused.wy`). Until
2026-11-16 the rule forgot the bounds' arguments, which traits had gained the
day before; an adversarial review found a program that freed a `Vec<String>`
while a reference from `sneak(&bag)` still pointed into it. `HashMap.get` keeps
its precision: `Q: Equivalent<K>` can yield a `&K`, never a `&V`, so the key is
free while the value is used (`hashmap/key-released.wy`).

The same test decides which signatures are allowed. A function that returns a
`&` could not take any `&mut`; now it may take a `&mut` the result cannot
point into, which is what lets `next` take its cursor
(`points-cursor.wy`; `points-cursor-same.refused.wy` is the signature that is
still refused).

## Where a variant borrows from

```
pub fn choose(left: &U64, right: &U64, flag: Bool) -> Result<&U64, &U64>:
    borrows Ok from left
    borrows Err from right
    ...

match choose(&a, &b, flag):
    Ok(item):
        b = 3                           // `item` borrows only from `a`
    Err(item):
        a = 3
```

When two parameters have the same type, the signature alone cannot tell which one a
reference in the result points into, so both stay lent. A clause `borrows Variant from p,
q` says that the references a result of that variant holds borrow only from those
parameters. The borrow checker holds the function to it, with no proof: every `return
Variant(...)` may carry loans only on the parameters named, and a value returned that is
not built at the `return` (a local, a call) could be any variant, so it may carry loans
only on parameters every clause names (`borrows-swapped.refused.wy`,
`borrows-unbuilt.refused.wy`).

A caller that matches the call directly gives each arm only its variant's loans: the
other inputs are free in that arm (`borrows-other.wy`, across files; the arm's own
source stays lent, `borrows-own-ok.refused.wy`, and two arguments that are one place stay
lent in both, `borrows-alias.refused.wy`). A parameter no clause names, when every variant
that holds a reference has a clause, is free as soon as the call returns, matched or
not; and a `&mut` parameter no clause names no longer stops the function from returning
a `&` (`borrows-scratch.wy`: a parser takes `&mut` scratch and returns text from its input).

A clause may name a `&mut` parameter: `borrows Decoded from scratch` lets that variant hand
out a `&` into what the parameter points at (`return Decoded(&*scratch)`), and a caller's
arm for it keeps the scratch lent, uniquely, while the reference is in use; the other arm's
input is free (`borrows-mut-source.wy`, `borrows-mut-source-held.refused.wy`). This is what
lets a parser return text from its input or decoded into its scratch
(`packages/config-app.wy`, with the package in `packages/config/`).

A result bound with `let` and matched later is split the same way, when the match is the
name's last use (`borrows-let.wy`); a name used again after the match keeps every loan until
then (`borrows-let-later.refused.wy`, `borrows-mut-source-let.refused.wy`).

A function may forward another's result, `return choose(left, right, flag)`: each variant's
loans are judged against its own clause for that variant (`borrows-forward.wy`,
`borrows-forward-swapped.refused.wy`). A result bound to a local and returned is still held
to every clause at once.

What this rests on, all of it checked elsewhere and listed here so that a
change to any of them is seen to matter:

- a function body is checked with its type parameters opaque, so `return b`
  where a `&A` is wanted is a type error (`points-params-sneak.refused.wy`);
- there is no way to ask what a type parameter is, and no specialisation that
  generic code can reach: `impl Name<U64>` makes ordinary functions on that
  one type;
- trait methods have no type parameters of their own (traits themselves may);
- a reference points at no sum that holds a reference (a view record is allowed), and a returned
  reference never points into the function's own locals;
- the ways one type holds another are fields, variants, array elements and
  buffer elements. A new kind of container type has to be added to
  `can_hold`, `references` and `lends` together.

Arguments that are not references (a view passed by value) are not judged and
stay lent as before. Fixtures: `tests/fixtures/references/points-*.wy` and
`tests/fixtures/hashmap/walk*.wy`. Tested, not proved.

## Loans on alternative paths

The branches of an `if` and the arms of a `match` are alternatives, so each is
checked from the loans in force before it: a loan one arm keeps (a reference it
gives to `let name = match ...`) is not in force in another arm, and a loan one
arm releases is not released for another. After all of them, the changes are
combined. So the arm that leaves may close a container that the arm giving a
reference keeps lent (`tests/fixtures/borrow-paths/`). Before this, arms were
checked one after another with the loans of the arms before them in force,
which refused the first and could have let a release in one arm count in the next.

## Fields are lent separately

A loan covers the field path it was taken through: `&p.left` and `&mut p.right` may be held
at once, and so may `&mut p.inner.a` beside `&p.inner.b`. Two loans conflict when they are of
one value and one path begins the other (`p.inner` and `p.inner.a`; the whole `p` and any of
its fields): `borrow-order/disjoint-fields.wy`, `overlapping-fields.refused.wy`,
`field-then-whole.refused.wy`. Reading or writing a field is checked against the loans of that field's path, so
`b.count = 3` is fine while a reference into `b.items` is in use
(`field-write-beside-loan.wy`); reading or writing the whole value still counts every
field's loan. In `check/loans.rs` a field loan has an id of its own (`loan_id`, `FIELD_LOAN`), and `overlaps`
decides conflicts. Fields reached through a reference parameter are lent separately too
(`param-fields.wy`). Elements of a fixed array named by literal indexes are separate places
(`g.cells[0]` beside `g.cells[1]`, `array-elements.wy`); any other index may be any element,
so it overlaps all of them (`array-element-unknown.refused.wy`).

## Methods that borrow some fields: view receivers

```
impl Counter:
    fn bump(self: Self { &mut count, &step }):
        self.count += self.step
```

`c.bump()` lends `c.count` mutably and `c.step` shared, and nothing else of `c`, so
`let name = &c.name` stays usable across the call (`borrow-order/view-receiver.wy`). In the
body, `self.count` is the listed field; any other field, or `self` as a whole, is refused
(`view-receiver-unlisted.refused.wy`). The receiver is one reference parameter per field
(`Counter.total(&c.count, &c.step)` calls it directly), so the checker and code generator
see nothing new: the parser expands it, methods.rs gives each the record's field type, and
the method call lends the fields (`check/methods.rs` `method_call`). A view receiver is for a type's
own methods, not a trait's. A type with an invariant lends its fields to a view receiver
shared only (`view-receiver-invariant.refused.wy`): writing one field alone could break it. Through a shared reference a `&mut` field is refused
(`view-receiver-shared.refused.wy`); outside a sealed type's file, so is a `&mut` field of
it, as any write to its fields is.

