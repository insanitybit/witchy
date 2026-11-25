# Write an abstraction that works for more than one type

A generic function introduces a type parameter such as `T`. Each call supplies a concrete type, often inferred from the argument. Methods let you group operations by the type they work on.

Save this as `pair.wy`.

```witchy
import console

type Pair<T>:
    first: T
    second: T

impl<T> Pair<T>:
    fn head(self: &Pair<T>) -> T:
        return (*self).first

fn same<T>(value: T) -> T:
    return value

fn main(console: Console<Write>):
    let pair: Pair<U64> = Pair(7, 9)
    console.print_int(same(pair.head()))
    let flags: Pair<Bool> = Pair(true, false)
    if same(flags.head()):
        console.line("enabled")
```

`pair.head()` lends the receiver because `head` takes `&Pair<T>`. It returns a copy of the first field. The same definition works for `U64` and `Bool`. Run it as in [Your first Witchy program](start.md) and expect `7` followed by `enabled`.

The plain `T` bound in this example permits copyable values. `T: Any` can also admit owning values. If you broaden a generic API to accept owners, decide whether to lend the element or move it out. Copying an arbitrary owning element would duplicate its cleanup obligation.

A receiver can be shared, exclusive, or passed by value. You can also list the exact fields to lend with a [view receiver](ownership.md#borrowing-part-of-a-receiver). Traits let a generic function request specific operations, and derive recipes can generate implementations from the shape of the type.

[Generic functions](../docs/wasm-generic-functions.md) · [Methods and receivers](../docs/wasm-methods.md) · [Traits and operators](../docs/wasm-traits.md) · [Derive recipes](../docs/wasm-derive.md)
