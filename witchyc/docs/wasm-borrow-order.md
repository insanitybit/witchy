# Arguments that read what the call borrows

```
items.push_mut(allocator, items.len() * 10)
items.reserve_mut(allocator, items.capacity() + 1)?
add(&mut counter, add(&mut counter, 2))
```

Each of these was refused as "overlapping unique call-argument loans": the
receiver was lent before the other arguments were checked, so an argument could
not look at it. They are accepted now.

## The rule

In a call, a reference argument whose evaluation runs nothing is checked after
the other arguments. Such an argument is a reference passed on by name, or `&`
or `&mut` of a place written with names, fields, one dereference, and indexes
that are names or literals (`src/compiler/check/places.rs`, `still`). The other arguments
are checked first, in the written order; loans they take end with them, as
before.

The rule applies only when every reference argument of the call is of that
kind and no argument holds a reference inside a value. Otherwise the written
order is kept, as it is in a function proved by a memory or record model, whose
models read arguments in place.

## Why this is the same program

The emitted code still evaluates arguments in the written order. Taking the
address of such a place reads no memory that another argument can change: a
local's address is fixed, a reference local cannot be reassigned, and an index
that is a name is read at its written position at run time. So the address is
the same whether it is worked out before or after the other arguments, and the
call receives the value as the other arguments left it.

The checks that matter still run, after the other arguments:

- the borrowed value must still be there: an argument that moved it out is
  refused (`tests/fixtures/borrow-order/moved.refused.wy`);
- two references to one value, one of them `&mut`, are refused
  (`two-unique.refused.wy`);
- a reference kept in a `let` keeps the value lent across the call
  (`held.refused.wy`).

`tests/fixtures/borrow-order/reads.wy` runs in Wasmtime.

Nothing new is trusted. The scalar proof walk already reads a borrowed
argument's parts after all arguments have been evaluated.
