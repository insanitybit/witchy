# Values and Types

## The primitives

witchy has the types you'd expect: `Int` (64-bit, signed), `Float` (IEEE-754
double), `Bool`, and `String` (UTF-8). It also has one you might not: `Duration`
is a *distinct* type with literal syntax.

```witchy
fn main(console: Console):
    let answer = 42
    let half = 0.5
    let ok = true
    let name = "witchy"
    let timeout = 30s
    print(console, "${answer}")
    print(console, "${half}")
    print(console, "${ok}")
    print(console, name)
    print(console, "${timeout < 1m}")
```

```text
42
0.5
true
witchy
true
```

`Int` arithmetic *wraps* on overflow (two's complement) on every backend — a
deliberate choice for portability, which we'll revisit when we talk about
backends. Division or modulo by zero is a runtime error, loudly, on every
backend.

A `Duration` renders through its `Show` impl — the human form — so `${timeout}`
prints `30s` (and `${90s}` prints `1m30s`), the same everywhere and identical to
`say(console, timeout)`. Underneath it is carried as whole milliseconds; when you
want the raw count reach for `duration.to_milliseconds(timeout)` (`30000`), and
for a clock face `duration.clock(timeout)` (`"0:00:30"`) — both live in the
`duration` module, so they need an `import duration`.

## Strings

Strings concatenate with `+` and interpolate with `${...}`:

```witchy
fn main(console: Console):
    let who = "world"
    let n = 3
    print(console, "hello, " + who)
    print(console, "n is ${n}, doubled ${n * 2}")
```

```text
hello, world
n is 3, doubled 6
```

`${expr}` renders *any* value — scalars, lists, tuples, records, sum types,
dicts, and any nesting.
Strings are UTF-8 and the common operations
(`string.length`, `string.char_count`, `string.split`, `string.contains`, …)
live in the `string` module — part of the prelude, so no import line is
needed; the [stdlib reference](appendix-stdlib.md) has the full list.

## Conversions

Crossing between types is always explicit — there's no silent coercion:

```witchy
fn main(console: Console):
    print(console, "${7}")
    print(console, "${math.to_float(7)}")
    print(console, "${math.to_int(7.9)}")
    print(console, "${string.to_int("123")}")
```

```text
7
7.0
7
123
```

`string.to_int` is strict: it **aborts the program** on non-numeric input or
on a value that won't fit in an `Int`, rather than silently returning a wrong
number — it does not return an `Err`. When bad input is expected (user input,
file contents), use `string.parse_int`, which returns `Option(Int)`.

## Lists, tuples, and dicts

Three built-in compound types. **Lists** are homogeneous and immutable;
**tuples** are fixed-size and heterogeneous; **dicts** are immutable maps.

```witchy
fn show(xs: List(Int)) -> String:
    xs.map(fn(n: Int): "${n}").join(" ")

fn main(console: Console):
    let xs = [1, 2, 3]
    print(console, show(xs))
    print(console, "${xs.length()}")
    print(console, "${xs[0]}")

    let pair = (1, "one")
    let (n, word) = pair
    print(console, "${n} = ${word}")
    print(console, "${pair.0} = ${pair.1}")

    let ages = dict.new().insert("ada", 36).insert("bob", 41)
    print(console, "${ages.get_or("ada", 0)}")
    print(console, "${ages.get_or("nobody", 0)}")
```

```text
1 2 3
3
1
1 = one
1 = one
36
0
```

A couple of practical notes you'll bump into:

- Interpolation renders compounds directly on every backend — `"${xs}"` prints
  `[1, 2, 3]`, `"${pair}"` prints `(1, one)`, and a dict prints `{ada: 36}`.
  The hand-rolled `show` above exists for when you want a **custom** format
  (here, space-separated and unbracketed) instead of that structural
  default. For a type of your own, implement the
  `Show` trait to give it a custom rendering (see [Generics](tour-generics.md)).
- `list`, `dict`, `string`, `math`, `option`, and `result` form **the
  prelude**: their functions are available in every program with no `import`
  line. That's why none of the examples above import anything.
- A binding can carry its type: `let xs: List(Int) = []` pins an otherwise
  ambiguous literal, and `let d: DateTime = ...` turns a wrong assumption
  into an error at that line instead of a confusing one later. Locals are
  inferred by default; ascribe when it helps.
- Intrinsics like `to_string` aren't first-class function values, so to pass
  one to `list.map` you wrap it in a lambda: `fn(n: Int): "${n}"`.

Indexing out of bounds (`xs[9]`) is a runtime error on every backend, never a
garbage value.

## Comprehensions

The everyday map/filter shapes have a literal form: `[expr for x in xs]`
builds a new list from an old one, and an `if` clause filters as it goes:

```witchy
fn main(console: Console):
    let xs = [1, 2, 3, 4, 5]
    print(console, "${[n * n for n in xs]}")
    print(console, "${[n for n in xs if n % 2 == 1]}")
    print(console, "${["${n}" for n in xs if n > 3]}")
```

```text
[1, 4, 9, 16, 25]
[1, 3, 5]
[4, 5]
```

A comprehension is pure sugar for the equivalent `for` loop with an
accumulator, so it costs nothing extra and works the same on both backends.
For *lazy* pipelines over large or infinite sequences, reach for
[iterators](tour-iterators.md) instead.

## Equality is structural

`==` compares *contents*, recursively, for every type that can be compared —
lists, tuples, records, enums, `Option`, dicts — not identity:

```witchy
fn main(console: Console):
    print(console, "${[1, 2, 3] == [1, 2, 3]}")
    print(console, "${(1, "a") == (1, "a")}")
    print(console, "${Some(5) == Some(5)}")
    print(console, "${Some(5) == None}")
```

```text
true
true
true
false
```

Now let's give these values somewhere to live: functions.
