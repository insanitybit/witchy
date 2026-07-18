# Generators and Iterators

Lists are eager: building one computes every element up front and holds them all
in memory. Often you want the opposite — a sequence computed *on demand*, so you
can describe "the even numbers" or "the Fibonacci sequence" without deciding in
advance how many you need. witchy gives you two cooperating tools for that: the
`std/iter` library of lazy iterators, and `gen fn` generators that produce one.

## Lazy iterators

`import iter` brings in `Iter(a)`, a lazy stream. Its combinators — `map`,
`filter`, `take`, `range`, and friends — build a *description* of a computation;
nothing runs until a consumer pulls it: `collect`, `fold`, `count`, `find`, or
`iter.for_each`.

```witchy
import iter

fn show(xs: List(Int)) -> String:
    var parts = []
    for x in xs:
        parts.push("${x}")
    parts.join(", ")

fn main(console: Console):
    // "the even numbers in [1, 20), each doubled" — built lazily, then realized.
    let evens = iter.range(1, 20).filter(fn(n: Int): n % 2 == 0)
    let doubled = evens.map(fn(n: Int): n * 2)
    let firsts: List(Int) = iter.collect(doubled.take(5))
    console.print(show(firsts))
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
        parts.push("${x}")
    parts.join(", ")

fn main(console: Console):
    let first10: List(Int) = iter.collect(fibs().take(10))
    console.print(show(first10))
```

```text
0, 1, 1, 2, 3, 5, 8, 13, 21, 34
```

The `while true` loop never finishes on its own; `fibs().take(10)` stops
pulling after ten values, so only ten Fibonacci numbers are ever computed. A
generator can branch and loop as freely as any function. The Collatz sequence is
finite, but its length is not known before iteration:

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
        parts.push("${x}")
    parts.join(", ")

fn main(console: Console):
    console.print("collatz(6): ${show(iter.collect(collatz(6)))}")
    console.print("collatz(27) steps: ${collatz(27).count()}")
```

```text
collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1
collatz(27) steps: 112
```

### Generator methods

A `gen fn` can also be a **method** in an inherent `impl` block. It dispatches by
receiver type like any other method and returns an `Iter(a)`; the body reads the
receiver's fields through `self`:

```witchy
import iter

type Counter:
    n: Int

impl Counter:
    gen fn upto(self) -> Iter(Int):
        var i = 0
        while i < self.n:
            yield i
            i = i + 1

fn main(console: Console):
    let c = Counter(4)
    let xs: List(Int) = iter.collect(c.upto())
    console.print("${xs}")
```

```text
[0, 1, 2, 3]
```

One restriction: a `gen fn` may not be a *trait* method (neither declared in a
`trait` nor implementing one in an `impl Trait for T`) — the compiler rejects it
at parse time. A trait that wants a lazy sequence declares a plain
`fn … -> Iter(a)`, and the impl can delegate to an inherent generator method.

## Why this stays simple

`collect` builds **whatever the call site expects** — any type implementing
`FromIterator`. The ascription chooses: a `List(Int)`, a
`Dict(String, Int)` from an iterator of pairs, or a `String` from an
iterator of pieces. With no expected type (say, collecting just to print),
the compiler asks you to ascribe the binding rather than guess.

`Iter(a)` yields owned values; it is not a lending iterator. A `gen fn` is lowered to an
ordinary function behind the scenes. The same generator runs identically on the
interpreter and the compiled backend — laziness is a library and a lowering, not
a special runtime.

A `gen fn` may mutate `var` across a `yield`; `a`, `b`, and `n` above all carry
forward. An `async fn` can also carry a `var` across an `await`
in supported positions ([Concurrency](tour-async.md)); the current async
lowering threads live locals through state-machine segments. The remaining
restriction is placement: `await` works in loop bodies, but not in branch
conditions or match scrutinees.

A generator with no capability parameters is also, by construction, **pure**: a
`gen fn` that takes no `Console`/`Dir`/`Net` provably cannot do I/O — it can only
compute the next value. That word — *provably* — is the thread we pull on next.

Modules organize these definitions; compile-time code can generate more of them
before type checking.
