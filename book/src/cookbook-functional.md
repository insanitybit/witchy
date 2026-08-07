# Functions as Values

Functions in witchy are ordinary values: you can store one in a variable, pass
it to `list.map`, and return one from another function. The `func` module
supplies the small set of combinators that show up whenever you work in that
style - composition, argument shuffling, and tuple projection.

## Composing and adapting functions

```witchy
import func

fn double(n: Int) -> Int:
    n * 2

fn inc(n: Int) -> Int:
    n + 1

fn main(console: Console):
    // compose(f, g)(x) = f(g(x)) — inc first, then double.
    let inc_then_double = func.compose(double, inc)
    console.print("compose: ${inc_then_double(5)}")
    let always_zero = func.constant(0)
    console.print("constant: ${always_zero(999)}")
    let pair = (10, "ten")
    console.print("first: ${func.first(pair)}, second: ${func.second(pair)}")
```

```text
compose: 12
constant: 0
first: 10, second: ten
```

`compose(f, g)` reads right-to-left, like the maths: it applies `g`, then `f`.
`identity` returns its argument unchanged (useful as a default transform),
`constant(x)` ignores its argument and always returns `x`, and `flip` swaps the
two arguments of a binary function.

## Keying comparisons with `on_key`

The most practical combinator is `on_key`. Ordering functions like `list.sort_by`
want a *less-than predicate* over the elements - but often you want to compare by
some field of each element, not the whole thing. `on_key(op, key)` builds
exactly that: it applies `key` to each side, then compares the results with `op`.

```witchy
import func

fn main(console: Console):
    var people = [("ada", 36), ("babbage", 49), ("turing", 41)]
    // Sort by the second tuple field (age) using a key-derived comparison.
    people.sort_by(func.on_key(fn(a: Int, b: Int): a < b, func.second))
    for p in people:
        console.print("${func.first(p)}: ${func.second(p)}")
```

```text
ada: 36
turing: 41
babbage: 49
```

Here `func.second` is the key (each person's age) and `fn(a, b): a < b` is the
comparison. `on_key` glues them into the `fn(a, a) -> Bool` predicate `sort_by`
expects. Pair it with `func.first` to sort by the first field instead - the same
shape, a different projection.

These combinators are deliberately few. witchy leans on named helper functions
and explicit closures for most work; `func` is there for the handful of spots
where a point-free adapter genuinely reads better than a lambda.
