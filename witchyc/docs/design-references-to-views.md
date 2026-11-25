# Design: references to views

Status: decided 2026-11-16; step 1 (shared references) built, the rest to follow, each with
its own adversarial review.

## Why

Today a `&` never points at a value that holds a reference (a *view*, such as `Span`,
`View<T>` or a record with a `&Point` field). That keeps every loan one level deep, and it is
why the checker is simple. It also blocks:

- closures that capture a shared reference: their `call` takes `&Self`;
- generators that walk a container, the natural iterator: `next_mut` takes `&mut Self`;
- `Show`/`Format` and any trait method taking `&Self` on a view (they take `self` by value
  today as a workaround).

## The rule

A reference to a view is allowed, with these meanings:

1. **`&v` (shared).** Borrowing a view local lends the local, as for any value, *and* keeps
   every loan the view itself carries for as long as the reference is in use (the reference
   inherits the view's loans, as a copy of the view does today). Reading `(*r).point` through
   it gives a reference that depends on those same loans. Nothing can change through it.
2. **`&mut v` (unique), step 2.** The view's scalar fields may be written through it (a
   generator's counters). Its reference fields may not be written at all: they are bound once,
   as they are today for a local view, so a callee can never store a reference that lives
   shorter than the caller's view. A whole view is never assigned through a `&mut` reference
   (that would replace its reference fields), and `memory.exchange` keeps refusing views.
3. **Still refused.** A view in a Buffer or Vec element (heap cells outlive any loan the
   checker tracks), a `&` to a reference-to-view (two levels is the limit: the checker tracks a
   view's loans and one reference over it), a function with a contract over a view.

## What changes where

- **Layout** (layout.rs): a view gets a stored form like any record; its reference fields are
  addresses. It is still never a Buffer element.
- **Types** (semantic_types.rs `Borrow`): `&V`/`&mut V` with `V` a view is a type; `&&V`-shaped
  pointees (a pointee that is itself a reference to a view) stay refused.
- **Checker** (check.rs `borrow`, `inherit`): borrowing a view local takes the local's loan
  and inherits its held loans; reading a reference field through a view reference inherits the
  same.

## Order

1. Shared references to views, with `Show`/`Format` taking `&Self` again as the first user.
2. Unique references with frozen reference fields; generators over containers. **Built**
   (the references; generators next): `&mut v` of a shared view takes a unique loan on it
   and keeps what it borrows; through such a reference only fields that hold no reference
   are written (`*r = other` and `(*r).point = q` are refused); generic code never gets a
   view behind `&mut T` (refused where it is made concrete, since it could assign a whole
   `T`). Fixtures: `references/view-cursor.wy`, `view-mut-*.refused.wy`.
3. Closures capturing shared references (`call(self: &Self)` then works as is). **Built**:
   a closure may capture a shared reference or slice declared with its type; it is copied in,
   the closure is a view keeping the loan, and it cannot outlive what it borrows
   (`closures/capture-reference*.wy`).

## Step 1, as built

- `&v` of a whole view local (Copy, shared references only) is allowed; the reference
  inherits the view's loans (check.rs `borrow`, `whole_view`). A view's fields are still not
  lent one by one.
- Reading through a reference whose pointee lends inherits the reference's loans.
- `can_hold` and `references` (semantic_types.rs) follow a reference into what it points at,
  so a call returning `&Point` from a `&Seen` argument keeps the loans `Seen` carries. Without
  this, a reference read out of a view through a call outlived the view's loan; the probe that
  found it is `references/view-reference-bad1.refused.wy`.
- A view addressed this way is laid out in the frame; reference fields are 4-byte cells
  (layout.rs `cells`, emit.rs `memory`).
- Still refused: `&mut` to a view (also when generic code is made concrete), a record field
  that is a reference to a view, a view in a Buffer.

- A loan on a view itself (`&v`, kept apart as path `VIEW_ITSELF` from the references it
  carries) points into this frame, so a
  result may not keep it (check.rs `external_origin`). Round 15 found `return &s` of a view
  parameter accepted and reading a later call's frame; fixed,
  `references/view-reference-escape-*.refused.wy`. Over-strict meanwhile: `let r = &s;
  return (*r).point` is refused though the point is the caller's (write `return s.point`).

Fixtures: `references/view-reference*.wy`, `view-borrowed-generic*.wy`.
