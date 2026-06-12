# Values and Types

## The primitives

witchy has the types you'd expect: `Int` (64-bit, signed), `Float` (IEEE-754
double), `Bool`, and `String` (UTF-8). It also has one you might not: `Duration`
is a *distinct* type with literal syntax, so you can't accidentally add a
timeout to a byte count.

```witchy
fn main(console: Console):
    let answer = 42
    let half = 0.5
    let ok = true
    let name = "witchy"
    let timeout = 30s
    print(console, to_string(answer))
    print(console, to_string(half))
    print(console, to_string(ok))
    print(console, name)
    print(console, to_string(timeout < 1m))
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

One display gotcha worth knowing now: a `Duration` is carried as whole
milliseconds, and that is what `${timeout}` and `to_string(timeout)` print —
`30000`, not `30s`. For human output, reach for `duration.human(timeout)`
(`"30s"`, `"1m30s"`) or `duration.clock(timeout)` (`"0:00:30"`), or `say` it —
`Duration` implements `Show` with the human form.

## Strings

Strings concatenate with `<>` and interpolate with `${...}`:

```witchy
fn main(console: Console):
    let who = "world"
    let n = 3
    print(console, "hello, " <> who)
    print(console, "n is ${n}, doubled ${n * 2}")
```

```text
hello, world
n is 3, doubled 6
```

`${expr}` renders *any* value — scalars, lists, tuples, records, sum types,
dicts, and any nesting — identically on both backends (it is sugar for the
built-in `to_string`). So you rarely call `to_string` by hand;
reach for `"${x}"`. Strings are UTF-8 and the common operations
(`string.length`, `string.char_count`, `string.split`, `string.contains`, …)
live in the `string` module — part of the prelude, so no import line is
needed; the [stdlib reference](appendix-stdlib.md) has the full list.

## Conversions

Crossing between types is always explicit — there's no silent coercion:

```witchy
fn main(console: Console):
    print(console, to_string(7))
    print(console, to_string(math.to_float(7)))
    print(console, to_string(math.to_int(7.9)))   // truncates toward zero
    print(console, to_string(string.to_int("123")))
```

```text
7
7
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
    string.join(list.map(xs, fn(n: Int): to_string(n)), " ")

fn main(console: Console):
    let xs = [1, 2, 3]
    print(console, show(xs))
    print(console, to_string(list.length(xs)))
    print(console, to_string(xs[0]))     // indexing is sugar for list.at(xs, 0)

    let pair = (1, "one")
    let (n, word) = pair                      // destructure...
    print(console, "${n} = ${word}")
    print(console, "${pair.0} = ${pair.1}")   // ...or read by position

    let ages = dict.insert(dict.insert(dict.new(), "ada", 36), "bob", 41)
    print(console, to_string(dict.get_or(ages, "ada", 0)))
    print(console, to_string(dict.get_or(ages, "nobody", 0)))   // default
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
  `[1, 2, 3]`, `"${pair}"` prints `(1, one)`, and a dict prints `{ada: 36}`. The
  hand-rolled `show` above isn't *needed* to print a list anymore; it's there
  when you want a **custom** format (here, space-separated and unbracketed)
  instead of that structural default. For a type of your own, implement the
  `Show` trait to give it a custom rendering (see [Generics](tour-generics.md)).
- `list`, `dict`, `string`, `math`, `option`, and `result` form **the
  prelude**: their functions are available in every program with no `import`
  line. That's why none of the examples above import anything.
- Intrinsics like `to_string` aren't first-class function values, so to pass
  one to `list.map` you wrap it in a lambda: `fn(n: Int): to_string(n)`.

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
    print(console, "${[to_string(n) for n in xs if n > 3]}")
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
    print(console, to_string([1, 2, 3] == [1, 2, 3]))
    print(console, to_string((1, "a") == (1, "a")))
    print(console, to_string(Some(5) == Some(5)))
    print(console, to_string(Some(5) == None))
```

```text
true
true
true
false
```

Now let's give these values somewhere to live: functions.
