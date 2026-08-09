# Sets, Grouping, and Deduplication

`list` and `dict` are in the prelude and cover most collection work. Two jobs,
though, deserve their own tools: asking *"have I seen this before?"* and
*"how many of each?"*. The `set` module answers the first; a `dict` with
`update` answers the second.

## The `Set` type

`Set(a)` holds distinct values and answers membership in one step. The `Set`
*type* is in the prelude, but its constructors live in the `set` module, so you
still write `import set` to call `set.new()` or `set.from_list(...)`.

The set-algebra methods read exactly as the math does:

```witchy
import set

fn main(console: Console):
    let a = set.from_list([1, 2, 3, 4])
    let b = set.from_list([3, 4, 5, 6])
    let both = a.intersection(b)
    let either = a.union(b)
    let only_a = a.difference(b)
    console.print("in both: ${both.length()}")
    console.print("in either: ${either.length()}")
    console.print("only in a: ${only_a.length()}")
    console.print("a subset of either: ${a.is_subset(either)}")
```

```text
in both: 2
in either: 6
only in a: 2
a subset of either: true
```

`insert` returns a `Bool` - `true` if the value was new - which makes it a
one-line deduplication filter. And because a `Set` builds from any iterator, you
can pipe a lazy `iter` computation straight into one:

```witchy
import set
import iter

fn main(console: Console):
    // Count distinct words, case-folded.
    let text = "the cat the dog THE bird a cat"
    var seen: Set(String) = set.new()
    for w in text.split(" "):
        seen.insert(w.to_lower())
    console.print("distinct words: ${seen.length()}")

    // Build a set straight from an iterator pipeline.
    let squares: Set(Int) = iter.collect(iter.range(1, 6).map(fn(n: Int): n * n))
    console.print("has 16: ${squares.contains(16)}")
    console.print("has 20: ${squares.contains(20)}")
```

```text
distinct words: 5
has 16: true
has 20: false
```

`iter.collect` into a `Set(a)` works through the library's conditional
`FromIterator` impl, which requires `a: Eq` - the same bound every set operation
needs.

## Counting with `dict.update`

To tally occurrences, you want "look up the current count, add one, store it
back" as a single atomic step. `dict.update` is exactly that: it takes a key, a
default for the first sighting, and a function applied to the current value.

```witchy
fn main(console: Console):
    var counts: Dict(String, Int) = dict.new()
    for w in "apple pear apple fig pear apple".split(" "):
        counts.update(w, 0, fn(n: Int): n + 1)
    // A dict has no inherent order; sort the keys for stable output.
    var keys = counts.keys()
    keys.sort()
    for k in keys:
        console.print("${k}: ${counts.get_or(k, 0)}")
```

```text
apple: 3
fig: 1
pear: 2
```

Two habits worth forming. First, `get_or` (and `get_or_insert`) spare
you an `Option` match when a sensible default exists. Second, a `Dict` has no
guaranteed iteration order - so when output must be deterministic (as every book
example must, to run on both backends), sort the keys first. That isn't a
witchy quirk; it's the same discipline any hash-map-backed program needs, made
visible.
