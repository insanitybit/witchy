# Ownership, borrowing, and cleanup

A vector owns its backing storage. Moving it transfers the obligation to deal with that storage. A shared borrow such as `&vector` lets a function inspect the value. An exclusive borrow such as `&mut vector` lets a function update it under the checker's aliasing rules.

A slice obtained from a vector borrows its elements. Try reading the [refused mutation example](../tests/fixtures/slices/mut-vec-push.refused.wy). Find where the slice is created and where it's used after the attempted mutation. That later use is why the loan must remain live.

Pay attention to whether a function accepts a value or a reference. A function can consume a collection and return one of its elements as a new owner. A fallible operation may return the original owner in its error value. Read the result type to see what cleanup remains yours to handle. Some types offer scoped cleanup through `using` and `Discard`.

## Do I write lifetime annotations?

Witchy has shared and mutable references (`&T` and `&mut T`). The checker tracks their loans from the code. There is currently no named lifetime syntax like Rust's `&'a T`.

For example, a function can return a reference derived from an input borrow. When a call returns a reference, the caller can have to keep multiple arguments lent while the result is used. Witchy's rules infer which loans to carry and are sometimes conservative. See the [reference rules](../docs/wasm-references.md) for the supported cases.

## Borrowing part of a receiver

A method that takes `&mut Self` asks for an exclusive borrow of the whole receiver. A view receiver instead lists the fields it needs.

```witchy
impl Counter:
    fn bump(self: Self { &mut count, &step }):
        self.count += self.step
```

Calling `c.bump()` lends `count` exclusively and `step` for shared access. A reference to `c.name` can remain live across the call. In the method body, accessing an unlisted field is refused. A change that needs more fields changes the declared receiver footprint.

View receivers are supported on inherent methods. When the receiver type has an invariant, they can borrow fields only for shared access. See the [working Counter example](../tests/fixtures/borrow-order/view-receiver.wy) and the [unlisted-field refusal](../tests/fixtures/borrow-order/view-receiver-unlisted.refused.wy).

[Reference and loan rules](../docs/wasm-references.md) · [Scoped cleanup](../docs/wasm-using.md) · [Next chapter, brands](brands.md)
