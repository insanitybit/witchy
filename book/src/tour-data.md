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
    console.print(a.name)
    console.print("${b.balance}")
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
    console.print("${richer.balance}")
    console.print("${a.balance}")
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
import reflect

fn main(console: Console):
    let point = .{x: 1, y: 2}
    console.print("${point.x}, ${point.y}")
    console.print(json.stringify(.{name: "ada", scores: [10, 20]}))
```

```text
1, 2
{"name":"ada","scores":[10,20]}
```

Field access (`point.x`) works exactly as on a named record, and because an
anonymous record is reflectable, `json.stringify` — and the other reflection-based
encoders (see [Generics](tour-generics.md)) — serialize it with no per-type code.
The same anonymous record can go directly to a JSON response or another
reflection-based encoder without a one-off `type`. A bare `"${rec}"` structural print works too
— an anonymous record renders as `.{x: 1, y: hi}`, exactly the way a named one
does.

## Structural aliases and anonymous unions

`type X = ...` names a shape without minting a new type. That makes it useful for
local data plumbing: start with a structural record or union when the shape is
obvious and close by. Move to `type X:` when the data needs invariants, custom
behavior, sealing, or a public contract.

```witchy
type Point = .{x: Int, y: Int}
type LoadErr = .[NotFound | BadPort(Int) | Missing(String)]

fn move_right(p: Point) -> .{y: Int, x: Int}:
    .{x: p.x + 1, ..p}

fn describe(e: LoadErr) -> String:
    match e:
        .NotFound -> "not found"
        .BadPort(p) -> "bad port ${p}"
        .Missing(k) -> "missing ${k}"

fn main(console: Console):
    console.print("${move_right(.{y: 2, x: 1})}")
    console.print(describe(.BadPort(70000)))
    console.print(describe(.Missing("host")))
```

```text
.{x: 2, y: 2}
bad port 70000
missing host
```

Anonymous-record identity is exact: `.{x: Int, y: Int}` and
`.{y: Int, x: Int}` are the same type. At a place that explicitly expects a
smaller anonymous shape, however, Witchy can project a richer record to that
exact target. Extra fields are really removed; they are not hidden dynamic
properties.

Use `.{..Base, field: Type}` to compose a new exact type from an existing
structural-record alias:

```witchy
type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, note: String}

fn label(row: Summary) -> String:
    row.label

fn main(console: Console):
    let detailed: Detailed = .{id: 7, label: "ready", note: "kept"}
    console.print(label(detailed))

    let summary: Summary = detailed
    console.print("${summary}")
    console.print("${detailed}")
```

```text
ready
.{id: 7, label: ready}
.{id: 7, label: ready, note: kept}
```

Projection is directed by an expected type: annotations, assignments, ordinary
and `let`/`own` arguments, returns, typed aggregate slots, and `as Summary` can
request it. Inference remains exact—lists and function values do not become
covariant, and unannotated branch joins do not silently drop fields. A borrowed
call leaves `detailed` unchanged; an `own` call consumes it. `var` arguments
stay invariant because a callee could replace the smaller record and there is
no sound way to write that replacement back while restoring omitted fields.

Type spread and value spread are different operations. `.{..Summary, note:
String}` composes a type; `.{label: "new", ..detailed}` updates a value of one
already-exact shape. Repeating a field with the identical type is harmless,
but a conflicting type, a cyclic base, or a non-record base is a compile error.

### Proving projection cost in the browser

`mode opt` runnable cells show deterministic compiled-resource counters below
their ordinary output. They are operation counts, not timings. Run this shallow
case, then change `1` to `64`: the additional 63 projections add at most 63
`rc_alloc_calls` and 63 `bump_alloc_calls`, while the output proves the richer
source remains unchanged. The current baseline constructs one exact target
record per iteration; a future scalar-replacement optimization may reduce the
counts without changing that upper bound.

```witchy
mode opt

type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, revision: Int}

fn main(console: Console):
    let row: Detailed = .{id: 7, label: "ready", revision: 3}
    var i = 0
    var total = 0
    while i < 1:
        let summary: Summary = row
        total = total + summary.id
        i = i + 1
    console.print("${total} ${row.revision}")
```

```text
7 3
```

Anonymous unions remain closed tag sets: `.BadPort(70000)` is only valid where
a `.[BadPort(Int) | ...]` type is expected, and matching must cover the tags.
Union values may widen into a larger tag set at calls, returns, and `?`
propagation.

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
    console.print("${North == North}")
    console.print("${area(Circle(2))}")
    console.print("${area(Rectangle(3, 4))}")
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
        Click(x, y) -> "click at ${x},${y}"
        Key(k) -> "key ${k}"
        Close -> "close"

fn main(console: Console):
    console.print(describe(Click(3, 9)))
    console.print(describe(Key("Enter")))
    console.print(describe(Close))
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
        [only] -> "one: ${only}"
        [first, ..rest] -> "first ${first} then ${rest.length()}"

fn sign(n: Int) -> String:
    match n:
        0 -> "zero"
        m if m > 0 -> "positive"
        _ -> "negative"

fn main(console: Console):
    console.print(head([]))
    console.print(head([7]))
    console.print(head([1, 2, 3]))
    console.print(sign(0))
    console.print(sign(4))
    console.print(sign(0 - 1))
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
knows the guard might not hold, so it still expects the cases below it. A
`match` with an unhandled variant is rejected with the missing cases named.

An arm can match **several values at once** with an or-pattern (`a | b | c`), or
a **range** with `lo..hi` (half-open) or `lo..=hi` (inclusive):

```witchy
fn size(n: Int) -> String:
    match n:
        0 -> "none"
        1 | 2 | 3 -> "a few"
        4..10 -> "several"
        10..=100 -> "many"
        _ -> "lots"

fn main(console: Console):
    console.print(size(2))
    console.print(size(7))
    console.print(size(50))
    console.print(size(1000))
```

```text
a few
several
many
lots
```

Or-patterns nest anywhere a pattern is allowed (`Some(1 | 2)`), and every
alternative must bind the same names with the same types. Range and or-patterns
are refutable — they can fail to match — so a `match` over them still needs a
final `_` (or exhaustive coverage) to compile.

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
    console.print("clamped to ${p.value}")
```

```text
clamped to 100
```

Sealing restricts **construction only** — reading fields (`p.value`) and
`match`ing on the value work exactly as before, from anywhere. Field assignment
and record spread build a new whole value, so those updates are also confined to
the declaring module. Inside that module the constructor and updates remain
available, which is how `percent` builds one. This is the same mechanism that
makes capabilities unforgeable (only the host mints a `Net`); a `sealed type`
just opens it to your own types. The standard library uses it widely — `Set`
guarantees distinct members, `semver.Version` non-negative components,
`time.DateTime` a real calendar date — each an invariant its smart constructor
enforces and its type then carries.

With records and enums in hand, we can talk about the witchy way to handle
things going wrong.
