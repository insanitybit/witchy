# Give your data a shape

A record bundles fields that belong together. A sum describes a choice between cases. A reading might contain a measurement, or it might be missing. Give those cases their own names so the rest of the program can handle each explicitly.

Save this as `reading.wy`.

```witchy
import console

type Sample:
    value: U64

type Reading:
    Measured(Sample)
    Missing

fn show(console: &Console<Write>, reading: Reading):
    match reading:
        Measured(sample):
            console.print_int(sample.value)
        Missing:
            console.line("no reading")

fn main(console: Console<Write>):
    show(&console, Measured(Sample(42)))
    show(&console, Missing)
```

`Sample(42)` constructs the record. `Measured(Sample(42))` wraps it in one case of `Reading`. The `Measured(sample)` pattern pulls the record back out, and the name `sample` is available in that arm.

Build and run it with the [same commands as Hello World](start.md), substituting `reading.wy` and `reading.wasm`. Expect `42` followed by `no reading`. Try removing the `Missing` arm and rebuilding. Witchy requires the match to cover every case.

The standard `Option<T>` and `Result<T, E>` types apply the same idea to absence and errors. Use a complete `match` when both paths need work. Use a refutable `let … else` when one path should exit early, as in the [directory example](capabilities.md#read-one-directory). The `?` operator propagates a compatible error to the caller. Leaving a scope still has to account for any owned values.

[More pattern forms](../docs/wasm-sums.md) · [Tuples and anonymous records](../docs/wasm-tuples.md) · [Anonymous sums](../docs/wasm-unions.md) · [Result and Option](../docs/wasm-results.md)

## Anonymous records and sums

Use an anonymous record for a shape described by its field names and types. Use an anonymous sum for a choice described by its tags and payload types. You can give either shape an alias to make signatures easier to read.

```witchy
import console

type Limits = .{low: U64, high: U64}
type ReadingStatus = .[InRange(U64) | Outside]

fn classify(limits: Limits, value: U64) -> ReadingStatus:
    if value < limits.low || value > limits.high:
        return .Outside
    return .InRange(value)

fn main(console: Console<Write>):
    let limits: Limits = .{low: 3, high: 9}
    match classify(limits, 7):
        .InRange(value):
            console.print_int(value)
        .Outside:
            console.line("outside the limits")
```

Read `Limits` as “a record with a low and high number.” The `.{…}` spelling works for both the shape and the value. Read `ReadingStatus` as “either an in-range number or an outside marker.” The `.[…]` spelling lists the alternatives. Put a dot before a tag when constructing it or matching it. Here the return type of `classify` tells Witchy which sum the tag belongs to.

Try passing `11` instead of `7` to take the other branch. For more, see [anonymous records and tuples](../docs/wasm-tuples.md) and [anonymous sums and widening](../docs/wasm-unions.md).
