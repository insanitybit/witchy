# Records that hold references

```
sealed type Span:
    text: &String
    from: U64
    to: U64

var rest = text.span()
match rest.split(59):
    Ok(halves):
        report(&console, halves.head)
        rest = halves.tail
```

A record field may be a `&` reference. Such a record is a view: making, copying,
narrowing and returning one copies no data, and what it points at stays lent
for as long as the view, a copy of it, or anything read out of it is in use.
`library/wasm-span.wy` is part of a `String` this way; a generic `Slice<T>` over
a `Vec<T>` is in `tests/fixtures/views/slice.wy`.

## What a view is

- A named record, generic or not, sealed or not, with one or more `&T` fields;
  a record with a field that is a view; an anonymous record `.{...}` holding
  either. A sum may hold a view, as it may hold a reference.
- A view is Copy. It is passed and returned by value and has methods that take
  `self` by value.
- It lives in locals only. It has no stored form, so it is never a `Vec` or
  `Buffer` element, an array element or a type argument, and nothing points at
  it: no `&View`, no `&view.field`.
- Its number and Bool fields are assigned like any record's.

## The rules

1. **A view carries its loans.** Building one keeps lent whatever its reference
   arguments borrow. A copy, a reference read out of it (`let items =
   view.items`), a record or sum built around it, and the result of a call it
   was passed to, each keep the same things lent. The loans end at the last use
   of the last of these, as for a reference bound with `let`.
2. **It is returned only when it borrows from the caller.** The rule for a
   returned reference: every loan the returned value carries must come from a
   reference parameter or from a by-value parameter that holds references.
3. **A view variable is assigned only a value that borrows from what it
   already borrows.** `rest = halves.tail` is accepted when `halves` came from
   `rest`. The variable's loans therefore never change. That is what makes a
   loop sound without a second pass over its body: what the variable keeps lent
   on the first pass is what it keeps lent on every pass. A view of another
   value needs a new `let`. A reference field is never assigned, and neither is
   a view inside a record or sum.
4. **It is not opened.** `let Span(text, from, to) = span` and a pattern with
   several names would give each name a reference without its loans; read the
   fields instead.
5. **`&` only.** A `&mut` field would need loans counted by field.

## Why the old refusal could go

The borrow fix of 2026-11-14 closed records with references because the
reference test was shallow. Since that fix one predicate, `lends`, walks sums
and records to any depth with a cycle check, and every path uses it: bindings,
calls, matches, returns. Views use those paths unchanged. What was added:

- the type layer admits `&T` fields and refuses `&mut T` ones;
- the finished type graph is checked: no Buffer element or array holds a
  reference, and nothing a reference points at does, including through a
  record that names itself;
- a view's layout is marked as having no stored form, so a frame slot for one
  cannot be planned;
- rule 3, rule 4 and the refusal of `&view.field`;
- an argument that is a reference field is passed on as the value it is.

## Proofs

No proof reads a view. A function with a contract cannot have one as a
parameter, a result or a value, and a type that holds a reference states no
invariant. A view's own claims, such as `from <= to` for `Span`, hold by
construction in a sealed type's file and are not checked.

## What is trusted

The rules above are part of the [borrow checker](../src/compiler/check/), including
its [loan flow analysis](../src/compiler/check/loan_flow.rs). They are
tested, not proved: `src/compiler/view_tests.rs` (seven groups, 68 programs),
`tests/fixtures/views/` (two programs run in Wasmtime, nine refusals). The
escapes looked for and refused: changing, closing or moving the source while a
view, a copy, a field read from it, a record or sum around it, or a view
returned by a call is still used; the same inside a `match` arm and across the
passes of a loop; returning a view of a local, directly, through a `let`,
through `?`, inside a `Result` or an anonymous record; a view beside a `&mut`
of its source in one call; assigning a view of another source; storing one;
pointing at one or at part of one; opening one.

Found while testing: opening a view with `let Name(a, b) = view` lost its
loans. It is refused now (rule 4).

## A reference chosen by `match` or `if`

```
let first = match whole.split(44):
    Ok(halves):
        halves.head
    Err(all):
        all
```

A `let` whose value is a `match` or an `if` may give a reference or a view. Each
path's value depends on some loans; those stay taken when the path ends, and the
name holds all of them, from every path, until its last use, as a plain `let` holds
its value's loans. So the name is treated as borrowing everything any path could
have given it: stricter than needed when the paths differ, never looser.

Two refusals are particular to it. A path may not give a reference into one of its
own locals, the value an arm names included: that local is gone when the path ends.
And two paths may not each take `&mut` of the same value, because the first loan is
still counted when the second is asked for; bind the `&mut` once before the choice.

`tests/fixtures/views/chosen.wy` runs in Wasmtime. Seven `chosen-*.refused.wy` must
not compile: a value assigned or read while the chosen reference may point at it
(`&` and `&mut`), a reference into a path's local and into an arm's value, a String
cleared or closed under a chosen Span, and a Span that may be of either of two
Strings while one of them is cleared.

This rule is in the borrow checker (`src/compiler/check/statements.rs`, `Choose` and `Yield`) and
is tested, not proved.

## Not yet

- `view.reference.field` and `*view.reference`: bind the reference with `let`
  first. The place model reaches through one reference, held in a local.
- A view as a type argument, so no generic function over views and no trait
  bound satisfied by one.
- `&mut` fields, which mutable cursors and iterators that change a container
  need.
- `for item in view`.
