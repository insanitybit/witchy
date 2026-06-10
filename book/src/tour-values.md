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
    print(console, int_to_string(answer))
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

`${expr}` is sugar for `to_string(expr)`. Strings are UTF-8 and the common
operations (`string_length`, `char_count`, `split`, `contains`, …) live in the
`string` module and as builtins; the [stdlib reference](appendix-stdlib.md) has
the full list.

## Conversions

Crossing between types is always explicit — there's no silent coercion:

```witchy
fn main(console: Console):
    print(console, int_to_string(7))
    print(console, to_string(int_to_float(7)))
    print(console, int_to_string(float_to_int(7.9)))   // truncates toward zero
    print(console, int_to_string(string_to_int("123")))
```

```text
7
7
7
123
```

`string_to_int` is strict: it errors on non-numeric input or on a value that
won't fit in an `Int`, rather than silently returning a wrong number.

## Lists, tuples, and dicts

Three built-in compound types. **Lists** are homogeneous and immutable;
**tuples** are fixed-size and heterogeneous; **dicts** are immutable maps.

```witchy
import list
import string

fn show(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), " ")

fn main(console: Console):
    let xs = [1, 2, 3]
    print(console, show(xs))
    print(console, int_to_string(length(xs)))
    print(console, int_to_string(xs[0]))     // indexing is sugar for at(xs, 0)

    let pair = (1, "one")
    let (n, word) = pair                      // destructure
    print(console, "${n} = ${word}")

    let ages = insert(insert(dict_new(), "ada", 36), "bob", 41)
    print(console, int_to_string(get_or(ages, "ada", 0)))
    print(console, int_to_string(get_or(ages, "nobody", 0)))   // default
```

```text
1 2 3
3
1
1 = one
36
0
```

A couple of practical notes you'll bump into:

- `to_string` works on scalars, but rendering a whole *list* or *tuple* to a
  string is interpreter-only, so portable code builds the string itself —
  that's why `show` above maps `int_to_string` over the list and joins. You'll
  see this idiom a lot.
- Builtins like `int_to_string` aren't first-class function values, so to pass
  one to `list.map` you wrap it in a lambda: `fn(n: Int): int_to_string(n)`.

Indexing out of bounds (`xs[9]`) is a runtime error on every backend, never a
garbage value.

## Equality is structural

`==` compares *contents*, recursively, for every type that can be compared —
lists, tuples, records, enums, `Option`, dicts — not identity:

```witchy
import option

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
