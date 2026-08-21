# Functions as Values

Functions in witchy are ordinary values: you can store one in a variable, pass
it to `list.map`, and return one from another function. The `func` module is
deliberately small. witchy leans on named helpers and explicit closures for most
work; `func` covers the handful of spots where a point-free adapter genuinely
reads better - composition, argument shuffling, and tuple projection.

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

Reach for these when a lambda would just be noise. Anywhere else, write the
named helper.

## Pure plugins

Use `pure fn` when an extension point promises computation without host effects.
The plugin may capture ordinary immutable data, and it can still be widened to
an ordinary reusable function when a less restrictive API needs it.

```witchy
pure fn run_plugin(plugin: pure fn(Int) -> Int, value: Int) -> Int:
    plugin(value)

fn main(console: Console):
    let offset = 2
    let plugin: pure fn(Int) -> Int = pure fn(value: Int): value * 2 + offset
    console.print("plugin: ${run_plugin(plugin, 20)}")
```

```text
plugin: 42
```

The qualifier is a checked promise, not an ascription that suppresses effects.
This does not compile because the ordinary callback could exercise authority:

```witchy-static
pure fn run_plugin(plugin: fn(Int) -> Int, value: Int) -> Int:
    plugin(value)

fn main():
    let _ = 0
```

## Reusable delegated behavior

An ordinary function value delegates its callable behavior. Its receiver does
not need to name capabilities hidden in the closure environment; the caller
chooses what behavior to provide. Taking the callback with `own` transfers the
caller's binding, but the callback remains reusable inside the callee.

```witchy
fn log_twice(own logger: fn(String) -> Nil):
    logger("first")
    logger("second")

fn main(console: Console):
    let logger = fn(message: String): console.print("delegated: ${message}")
    log_twice(logger)
```

```text
delegated: first
delegated: second
```

Delegated authority is not purity. This version does not compile because the
closure captures `Console` and calls it under a `pure fn` promise:

```witchy-static
fn main(console: Console):
    let logger: pure fn(String) -> Nil =
        pure fn(message: String): console.print(message)
    logger("effect")
```

## At most one completion

Use `once fn` for a callback that may be invoked at most once. Passing it to an
`own` parameter transfers the one remaining invocation, and attempting the call
consumes it even if the call returns an error or traps.

```witchy
fn complete(own callback: once fn(String) -> Nil, message: String):
    callback(message)

fn main(console: Console):
    let completion: once fn(String) -> Nil =
        once fn(message: String): console.print("completed: ${message}")
    complete(completion, "ready")
```

```text
completed: ready
```

The callback is affine. A second invocation is rejected before either backend
runs the program:

```witchy-static
fn main():
    let completion: once fn(Int) -> Int = once fn(value: Int): value
    let _ = completion(1)
    completion(2)
```

## Exactly-once disposition with a `must` wrapper

`once` alone is at-most-once and may be dropped unused. Wrap it in a nominal
`must` protocol when every control-flow path must either complete or explicitly
cancel. The wrapper carries the disposition obligation across opaque APIs; it
does not hide that obligation in the closure environment.

```witchy
must sealed type Completion:
    Completion(once fn(String) -> Nil)

fn finish(own completion: Completion, message: String):
    match completion:
        Completion(callback) -> callback(message)

fn cancel(own completion: Completion):
    match completion:
        Completion(_) -> ()

fn main(console: Console):
    let completed = Completion(
        once fn(message: String): console.print("finished: ${message}")
    )
    finish(completed, "saved")

    let cancelled = Completion(
        once fn(message: String): console.print("unexpected: ${message}")
    )
    cancel(cancelled)
    console.print("cancelled")
```

```text
finished: saved
cancelled
```

Forgetting both operations is a type error because the `must` value reaches the
end of its scope without disposition:

```witchy-static
must sealed type Completion:
    Completion(once fn(String) -> Nil)

fn main():
    let completion = Completion(once fn(message: String): ())
```
