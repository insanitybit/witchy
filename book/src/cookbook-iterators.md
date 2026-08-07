# Iterator Pipelines

The tour introduced `iter` as lazy streams with `map`, `filter`, and `take`.
This chapter is the working reference for the rest of the combinator set - the
ones that pair, accumulate, and terminate a pipeline. The theme throughout:
build a *description* of a computation with lazy combinators, then realize it
once with a *consumer* (`collect`, `fold`, `sum`, `count`, `find`).

## Pairing streams: `enumerate` and `zip`

`enumerate` tags each element with its index; `zip` walks two streams in
lockstep, stopping at the shorter one. Both yield tuples you destructure with
`.0` / `.1`:

```witchy
import iter

fn main(console: Console):
    let names = iter.from_list(["ada", "bob", "cy"])
    // enumerate pairs each element with its index.
    iter.for_each(names.enumerate(), fn(pair: (Int, String)): console.print("${pair.0}: ${pair.1}"))
    // zip two streams together.
    let a = iter.from_list([1, 2, 3])
    let b = iter.from_list([10, 20, 30])
    let sums: List(Int) = iter.collect(a.zip(b).map(fn(p: (Int, Int)): p.0 + p.1))
    console.print("zipped sums: ${sums.length()} values, total ${list.sum(sums)}")
```

```text
0: ada
1: bob
2: cy
zipped sums: 3 values, total 66
```

Note the annotation `let sums: List(Int) = iter.collect(...)`. `collect` is
generic over the target type (`c where c: FromIterator`), so it needs to know
what you're collecting *into* - a `List(Int)` here, but a `Set(Int)` if you
wanted distinct values. Annotate the binding and the checker specializes it.

## Accumulating: `fold` and `scan`

`fold` collapses a stream to a single value; `scan` is a fold that *emits* each
intermediate result, giving you a running total or a state machine over a
stream:

```witchy
import iter

fn main(console: Console):
    // fold reduces a stream to one value.
    let product = iter.range(1, 6).fold(1, fn(acc: Int, n: Int): acc * n)
    console.print("5! = ${product}")
    // scan is fold that emits each intermediate — a running total.
    let running: List(Int) = iter.collect(
        iter.from_list([1, 2, 3, 4]).scan(0, fn(s: Int, x: Int): (s + x, s + x)))
    console.print("running sums: ${running.length()} -> last ${list.last(running) ?? 0}")
    // take_while pulls from an infinite stream until the predicate fails.
    let small: List(Int) = iter.collect(
        iter.count_from(1).map(fn(n: Int): n * n).take_while(fn(sq: Int): sq < 50))
    console.print("squares under 50: ${small.length()}")
```

```text
5! = 120
running sums: 4 -> last 10
squares under 50: 7
```

`scan`'s function returns a `(new_state, emitted_value)` tuple - here both are
the running sum, so you get `[1, 3, 6, 10]`. And `take_while` shows the payoff of
laziness: `iter.count_from(1)` is an *infinite* stream, but `take_while` stops
pulling the moment a square reaches 50, so the pipeline terminates. An eager
`list` version would have to bound the input up front and guess how many
elements it needs.

## The consumers

A pipeline does nothing until a consumer pulls it. Besides `collect` and `fold`,
the terminal operations answer common questions directly: `sum` and `count`
reduce to a number, `find` returns the first `Some` match, `any` / `all` test a
predicate across the stream, `min` / `max` find extremes, and `position` gives
the index of the first match. Reaching for the specific consumer - `xs.any(p)`
rather than `xs.filter(p).count() > 0` - is both clearer and lets the stream
stop early. Choose lazy combinators for the pipeline, then one consumer to make
it run.
