# Generators and Iterators

Lists are eager: building one computes every element up front and holds them all
in memory. Often you want the opposite — a sequence computed *on demand*, so you
can describe "the even numbers" or "the Fibonacci sequence" without deciding in
advance how many you need. witchy gives you two cooperating tools for that: the
`std/iter` library of lazy iterators, and `gen fn` generators that produce one.

## Lazy iterators

`import iter` brings in `Iter(a)`, a lazy stream. Its combinators — `map`,
`filter`, `take`, `range`, and friends — build a *description* of a computation;
nothing runs until you `collect` it into a list (or fold, count, or loop over it).

```witchy
import iter

fn show(xs: List(Int)) -> String:
    var parts = []
    for x in xs:
        parts = list.push(parts, "${x}")
    string.join(parts, ", ")

fn main(console: Console):
    // "the even numbers in [1, 20), each doubled" — built lazily, then realized.
    let evens = iter.filter(iter.range(1, 20), fn(n: Int): n % 2 == 0)
    let doubled = iter.map(evens, fn(n: Int): n * 2)
    print(console, show(iter.collect(iter.take(doubled, 5))))
```

```text
4, 8, 12, 16, 20
```

`iter.range(1, 20)` never materializes twenty numbers; `filter` and `map` never
walk the whole thing. `take(…, 5)` pulls exactly five values through the pipeline,
and `collect` is the only step that builds a list. Laziness is what lets the next
section's *infinite* sequences work.

## Generators: `gen fn` and `yield`

Writing an iterator by hand (threading state through `unfold`) is fiddly. A
`gen fn` lets you write the sequence as an ordinary imperative loop and `yield`
each value; calling it returns an `Iter(a)` that runs only as far as it's asked.

```witchy
import iter

// Infinite — and that's fine, because the caller bounds it with `take`.
gen fn fibs() -> Iter(Int):
    var a = 0
    var b = 1
    while true:
        yield a
        let nxt = a + b
        a = b
        b = nxt

fn show(xs: List(Int)) -> String:
    var parts = []
    for x in xs:
        parts = list.push(parts, "${x}")
    string.join(parts, ", ")

fn main(console: Console):
    print(console, show(iter.collect(iter.take(fibs(), 10))))
```

```text
0, 1, 1, 2, 3, 5, 8, 13, 21, 34
```

The `while true` loop never finishes on its own; `iter.take(fibs(), 10)` stops
pulling after ten values, so only ten Fibonacci numbers are ever computed. A
generator can branch and loop as freely as any function — here is the Collatz
sequence, which is finite but whose length you can't predict:

```witchy
import iter

gen fn collatz(start: Int) -> Iter(Int):
    var n = start
    yield n
    while n > 1:
        if n % 2 == 0:
            n = n / 2
        else:
            n = 3 * n + 1
        yield n

fn show(xs: List(Int)) -> String:
    var parts = []
    for x in xs:
        parts = list.push(parts, "${x}")
    string.join(parts, ", ")

fn main(console: Console):
    print(console, "collatz(6): " + show(iter.collect(collatz(6))))
    print(console, "collatz(27) steps: " + "${iter.count(collatz(27))}")
```

```text
collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1
collatz(27) steps: 112
```

## Why this stays simple

If you've met iterators in Rust, you may be bracing for lifetimes and lending
iterators. There's none of that here: witchy values are plain data with no
borrowing, so an `Iter(a)` just yields values and a `gen fn` is lowered to an
ordinary function behind the scenes. The same generator runs identically on the
interpreter and the compiled backend — laziness is a library and a lowering, not
a special runtime.

A generator with no capability parameters is also, by construction, **pure**: a
`gen fn` that takes no `Console`/`Dir`/`Net` provably cannot do I/O — it can only
compute the next value. That word — *provably* — is the thread we pull on next.

Everything so far has been pure: code that computes and returns values. Now we
get to the part witchy exists for — what happens when a program needs to actually
*do* something in the world, and how the language keeps that authority honest.
One short stop first: code that runs at compile time.

Next: comptime.
