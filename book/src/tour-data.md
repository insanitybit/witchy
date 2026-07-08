# Data: Records and Enums

One keyword, `type`, defines all of witchy's user types. It covers three shapes
that other languages give separate syntax: records (structs), enums, and tagged
unions (sum types).

## Records

A record is a single shape with named fields:

```witchy
type Account:
    name: String
    balance: Int

fn main(console: Console):
    let a = Account("ada", 100)
    let b = Account(name: "bob", balance: 5)
    print(console, a.name)
    print(console, "${b.balance}")
```

```text
ada
5
```

Records are immutable. To "change" a field you make a fresh record; the spread
form `..base` fills in the fields you don't override. (When the record lives in a
`var`, the field-assignment shorthand `acct.balance = b` writes that fresh-record
update for you — sugar for `acct = Account(balance: b, ..acct)`, kept in place by
the optimizer.)

```witchy
type Account:
    name: String
    balance: Int

fn deposit(a: Account, amount: Int) -> Account:
    Account(balance: a.balance + amount, ..a)

fn main(console: Console):
    let a = Account("ada", 100)
    let richer = deposit(a, 50)
    print(console, "${richer.balance}")
    print(console, "${a.balance}")
```

```text
150
100
```

## Anonymous records

Sometimes you want a record's *shape* without naming a type for it — usually to
bundle a few values on the spot. `.{field: expr, ...}` is a record with no declared
type:

```witchy
import json

fn main(console: Console):
    let point = .{x: 1, y: 2}
    print(console, "${point.x}, ${point.y}")
    print(console, json.stringify(.{name: "ada", scores: [10, 20]}))
```

```text
1, 2
{"name":"ada","scores":[10,20]}
```

Field access (`point.x`) works exactly as on a named record, and because an
anonymous record is reflectable, `json.stringify` — and the other reflection-based
encoders (see [Generics](tour-generics.md)) — serialize it with no per-type code.
That's the payoff: you can hand structured data to a JSON response or an encoder
without declaring a one-off `type`. One limit to know: a bare `"${rec}"`
*structural* print works for a named record but not an anonymous one on the
compiled backend — render an anonymous record with `json.stringify`, or field by
field.

## Enums and sum types

List the variants. They can be nullary (a plain enum) or carry data (a sum
type):

```witchy
type Direction:
    North
    South
    East
    West

type Shape:
    Circle(Int)
    Rectangle(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Rectangle(w, h) -> w * h

fn main(console: Console):
    print(console, "${North == North}")
    print(console, "${area(Circle(2))}")
    print(console, "${area(Rectangle(3, 4))}")
```

```text
true
12
12
```

## `match` is exhaustive

`match` destructures a value and *must* cover every case — leave one out and the
compiler tells you which, by name. This is what makes adding a variant safe:
every `match` that needs updating becomes a compile error.

```witchy
type Event:
    Click(Int, Int)
    Key(String)
    Close

fn describe(e: Event) -> String:
    match e:
        Click(x, y) -> "click at " + "${x}" + "," + "${y}"
        Key(k) -> "key " + k
        Close -> "close"

fn main(console: Console):
    print(console, describe(Click(3, 9)))
    print(console, describe(Key("Enter")))
    print(console, describe(Close))
```

```text
click at 3,9
key Enter
close
```

Patterns nest, and they can match literals, bind variables, ignore with `_`,
destructure lists (`[]`, `[first, ..rest]`), and add a guard condition:

```witchy
fn head(xs: List(Int)) -> String:
    match xs:
        [] -> "empty"
        [only] -> "one: " + "${only}"
        [first, ..rest] -> "first " + "${first}" + " then " + "${list.length(rest)}"

fn sign(n: Int) -> String:
    match n:
        0 -> "zero"
        m if m > 0 -> "positive"
        _ -> "negative"

fn main(console: Console):
    print(console, head([]))
    print(console, head([7]))
    print(console, head([1, 2, 3]))
    print(console, sign(0))
    print(console, sign(4))
    print(console, sign(0 - 1))
```

```text
empty
one: 7
first 1 then 2
zero
positive
negative
```

A guarded arm (`m if m > 0`) doesn't count toward exhaustiveness — the checker
knows the guard might not hold, so it still expects the cases below it. Aim a
`match` at a value with an unhandled variant and witchy won't compile it; that's
the feature, not an annoyance.

An arm's body is usually an expression, but a single statement works inline
too — `0 -> return Err("zero")` to bail out of the enclosing function, or
`Some(v) -> total = total + v` to update a `var`. For more than one statement,
put the body on its own indented lines after the `->`.

## Sealed types

By default any code that can see a type can also build one — `Percent(140)` is a
valid expression even if 140 is a nonsense percentage. Prefix the declaration
with `sealed` and construction becomes the private business of the defining
module: outside code can no longer call the data constructor, so it must go
through the module's public functions. Those functions ("smart constructors")
are then the single place an invariant is established — and, because nothing can
bypass them, a value of the type is *proof* the invariant holds.

```witchy
sealed type Percent:
    value: Int

// The one choke point. Every Percent in the program came through here, so
// `0 <= value <= 100` holds everywhere without re-checking.
pub fn percent(n: Int) -> Percent:
    if n < 0:
        Percent(0)
    else if n > 100:
        Percent(100)
    else:
        Percent(n)

fn main(console: Console):
    let p = percent(140)
    print(console, "clamped to ${p.value}")
```

```text
clamped to 100
```

Sealing restricts **construction only** — reading fields (`p.value`) and
`match`ing on the value work exactly as before, from anywhere. Inside the
declaring module the constructor is still available, which is how `percent`
builds one. This is the same mechanism that makes capabilities unforgeable (only
the host mints a `Net`); a `sealed type` just opens it to your own types. The
standard library uses it widely — `Set` guarantees distinct members,
`semver.Version` non-negative components, `time.DateTime` a real calendar date —
each an invariant its smart constructor enforces and its type then carries.

With records and enums in hand, we can talk about the witchy way to handle
things going wrong.
