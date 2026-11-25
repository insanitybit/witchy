# `for`

```
for index in 0..count:
    total += index

for name in &names:
    console.print_int(name.byte_len())
```

`for i in a..b:` counts from `a` up to but not including `b`, in `U64`; `for i in
a..=b:` includes `b`, and stops by comparing with `b`, so a range ending at the largest U64
ends (`loops/inclusive.wy`). Both ends are evaluated once. A loop over `a..=b` takes no
invariants, and a generator counts with `a..b` only (`loops/inclusive-invariant.refused.wy`,
`generators/inclusive.refused.wy`).

`for item in container:` walks a container without taking anything out. It
calls `container.at(0)`, `container.at(1)`, ... and stops at the first `Err`.
Any type with a method `at(self: &Type, index: U64) -> Result<Item, Error>`
can be walked: the library's `Vec` returns `&T`, and
`tests/fixtures/for/count.wy` walks a record of the program's own that returns
`U64`. The `&` in `for name in &names` is optional and changes nothing; it
says that `names` is only borrowed.

The loop variable is a `let`. When `at` returns a reference, the container is
lent until the last use of the variable in the loop body, by the ordinary rule
for references (`docs/wasm-references.md`). After that the body may change the
container; the next step asks `at` again, so a loop over a shrinking or growing
container is still safe.

## `break` and `continue`

`break` leaves the innermost loop; `continue` goes on to its next step. Both
work in `while` and `for`, and inside the arms of a `match` in the loop body:

```
for value in values:
    if *value == 0:
        break
    if *value % 2 == 1:
        continue
    sum += *value
```

The ownership rules at the jump are the ones at the end of the loop body: an
owner made in the body must already be consumed, and an owner from outside the
loop must be as the loop found it. A function with a contract may use both;
after a loop that can `break`, its proof knows only the loop's invariants.
`tests/fixtures/loops/jumps.wy` runs in Wasmtime.

## How

The hidden counter moves on before the body runs, so `continue` cannot skip the
step. `src/compiler/iteration.rs` rewrites `for` into a hidden counter, a `while` and,
for a container, a `match` on `at`. The checker, the emitter and the proofs see
only statements they already know. Nothing is specific to `Vec`.

## Walking a slice

`for item in s` over a shared slice (`&[T]`) binds the slice once to a hidden `let`, reads
its length once, and reaches each element at a counter below that length: no bounds test and
no `Result` per element, and the counter's step is unchecked, as a counting `for`'s. Nothing can
change a shared slice, or what it lends from, while the loop holds it, so every position the
counter takes is in range by construction; no proof is involved.

A type that has `elements(self: &Self) -> &[T]`, its elements in order as one slice, is walked
over that slice when the loop is shared and the body does not name the container. `Vec` has
it. A body that names the container is walked through `at`, asked again each step, so it may
still change the container once it is done with the element (`for/slice-walk-change.wy`). A
type whose `elements` is shaped otherwise is walked through `at` as before.

`for item in &mut container` is walked the same way over `elements_mut(self: &mut Self) -> &mut [T]`
where the type has `at_mut` and the body does not name the container; `Vec` has it.

A loop over a million-element `Vec<U64>` summing its elements: 0.99 to 0.51 ns per element; one
writing each element through `&mut`: 1.06 to 0.56
(`for/slice-walk.wy`, `for/elements-own.wy`).

## Changing the elements

```
for name in &mut names:
    name.push_text(allocator, "!")?
for value in &mut numbers:
    *value += 1
```

`for item in &mut container:` calls `container.at_mut(0)`, `at_mut(1)`, ...
and the loop variable is a `&mut` reference. Any type with a method
`at_mut(self: &mut Type, index: U64) -> Result<&mut Item, Error>` can be walked
this way. The container is lent uniquely until the last use of the variable in
the body. `tests/fixtures/for/change.wy` runs in Wasmtime.

## Not yet

Iterating by value (taking the elements out) and iterators as values wait for
traits.

## Arrays

```
for value in values:          // values: [U64; 4], or a reference to one
    sum += *value
for value in &mut values:
    *value = *value * 10
for value in [4, 5, 6]:
```

An array has no methods, so it is not asked for `at`: it is walked by index, from
zero to its length, and `item` is `&array[index]` (`&mut` after `&mut`). The index
is below the length by construction, so no element can be missing.

To tell an array from a container, a `for` over a container is now expanded when the
checker reaches it, where the type of what it walks is known, into a block of its
own; a counting `for` is still expanded with the rest of its block. The loop lends
the array while an element is in hand, so assigning to the array inside the loop is
refused, as it is for a Vec.

`tests/fixtures/for/array.wy` runs in Wasmtime; `array-assigned.refused.wy` and
`no-at.refused.wy` (a record, which is neither) must not compile.

## `for` in a function with a contract

```
for index in 0..n:
    invariant sum == index * 3
    invariant index <= n
    sum += 3
```

A `for` takes `invariant` lines as a `while` does. In a counting loop the name, in
an invariant, stands for the index the next pass will have: `start` on entry, and
`end` when the loop is left, provided an invariant says `index <= end` (the loop
alone says only that the index is not below the end). In a walk over a container
or an array the element has no name at the head of the loop, so invariants speak of
the other locals.

A walk over a container stops when the container answers `Err`, and also if its
hidden counter reached the largest U64, which no container does. That bound is what
lets a function with a contract show that stepping the counter cannot overflow;
before it, such a function could not walk a Vec with `for` at all.

`tests/fixtures/scalar/for-loops.wy` runs in Wasmtime: `result == n * 3`, a count
from `low` to `high`, a bounded count over a Vec, and a bounded sum over an array.
Four `for-*.refused.wy` must not compile: a step that does not keep the invariant,
a missing `index <= n`, a false bound over a Vec, and an invariant in a function
with no contract.

Nothing new is trusted: the loop is rewritten into the `while` the proof walk
already reads.

## The position of each element

```
for index, item in &numbers:
    console.write_u64(index)
```

`for index, item in container:` also names where each element is, counting
from zero. It is the loop's hidden counter read before the body, so `continue`
and `break` leave it right, and it cannot be assigned
(`tests/fixtures/for/positions-assigned.refused.wy`). It works for arrays and
for anything with `at`, with `&` and `&mut`. A count has one name; `for i, x in
a..b` is refused. Invariants cannot speak of the position: at the head of the
loop it has no name. `tests/fixtures/for/positions.wy` runs in Wasmtime.

## Walking values that are not stored: `next`

```
for scalar in &text:                    // a String's scalars
for position, scalar in &text:          // and the byte each starts at
```

A type with no `at` method and a `next` method is walked with a cursor:

```
fn next(self: &String, cursor: &mut U64) -> Result<U64, U64>
```

The loop holds a `U64` that starts at zero and calls `next(&mut cursor)` until
it answers `Err`; `Ok` carries the value and `next` has moved the cursor past
it. What the cursor counts is the type's business: for `String` it is a byte
offset (`library/wasm-text.wy`), for the `Countdown` in
`tests/fixtures/for/scalars.wy` a step. With `for position, value in` the
position is the cursor before the call. `break` and `continue` work as in any
loop.

The values are made by the call, not lent from the container, so the container
is not borrowed across the body. Changing it there is allowed and safe: every
`next` is an ordinary checked call that reads the container as it then is.
`for … in &mut` asks for `next_mut`, which no library type has
(`scalars-mut.refused.wy`).

Which way a `for` goes: an array by index; otherwise `at` if the type has it;
otherwise `next`. A type with both is walked with `at`. This is still a rule
about method names, not a trait.

## Counting up to a slice's length

In `for j in a..s.len():`, with `s` a shared slice of Copy elements, each read `s[j]` in the body
has no test: the counter is below the length, read once before the loop, and a slice's length
cannot change (no slice is held in a `var`, and the counter is not written in the body).
The checker uses the receiver's type at each marked read, including slices inferred
from earlier local bindings. A range over a user type evaluates its start, calls the written
`v.len()` once for its end, and uses the type's ordinary `index` method for each `v[j]`,
unless the type's own methods say, as written, that the read is a slice's element (below).

**A container whose `index` reads a slice it says is as long as `len()`** (2026-11-24). In
`for j in a..v.len():` with `v` a name bound once (a parameter or a `let`) of a type whose
`index(c, i)` is `return c.m()[i]`, whose `len(c)` is `return c.p` for a path of fields and
argument-free methods, and whose `m` takes `&c`, requires nothing, returns a `&[T]` and has
`ensures result.len() == c.p` (or `== c.len()`), each `v[j]` is `v.m()[j]` read with no test:
`index` is that read, the range's end is `c.p`, `v` cannot change in the loop, and `m`'s proved
`ensures` makes the slice that long. When `v` is a `&` and `m` runs no loop and no call (only
fields and `memory.` operations under its proof, or forwarding to such a method of a field),
`v.m()` is made once after the end (`for#N.view`), so the loop is one load and one comparison a
pass. The library's `Vec.index` is `return vector.as_slice()[index]` and `as_slice` says
`result.len() == value.length`; a type named `Vec` with another `index` keeps its own
(`slices/counted-view*`, `slices/counted-container.wy`, `check/methods.rs` `counted_container`).
Measured: Astra's 64-element sum 32 times, 138,648 fuel before, 57,656 after; the explicit
`let s = v.as_slice()` form is 57,560. To count through a container that does not say this,
bind its slice once (`let s = v.as_slice()`) and use `for j in a..s.len():` with `s[j]`. Writes, element
borrows and reads of a unique slice keep their ordinary behavior. See `slices/counted-read*`.

A proved loop can also use the container's own methods. In `slices/counted-projection.wy`,
`Tape.len()` returns the logical count. The loop proves `i < tape.len()` at the call to
`Tape.index`. Inside that method the type invariant bounds the count by the size of the
backing array, so the proof covers the read from its slice. This works with spare cells.

The same holds after a test in `&&`: in `i < s.len() && ... s[i] ...` (or `s.len() > i`), the
right side runs only once the left held, and an expression assigns nothing, so each `s[i]` there
on a shared slice of Copy elements has no test (`check/rewrites.rs` `guarded_reads`). An expression that
takes `&mut i`, or calls a method on `i`, anywhere (also on the left after the test: `&&` groups
to the left) is left as written. `slices/guarded-read*`.

And at the head of a `while`: in `while i < s.len() ...:` (the test among the condition's `&&`
conjuncts), each `s[i]` in the body's first statements, before the first that could change `i`
(assigns it or through it, takes `&mut i`, calls a method on it, binds the name, or is a statement
this does not know), runs right after the test held, so it has no test (`check/statements.rs`, the `While`
arm; `touches`). `slices/while-prefix-read*`.
