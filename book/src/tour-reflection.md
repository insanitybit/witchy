# Reflection

`derive(Show)`, `derive(Eq)`, and friends generate a *new function per type* at
compile time. Reflection is the other half of the same idea: instead of generating
code for each type, it exposes a value's **structure as data**, so one function can
walk a value of *any* type. It is how `json.stringify` serializes a record it has
never seen, with no per-type encoder.

## Making a type reflectable

The scalars (`Int`, `Float`, `Bool`, `String`), `Bytes`, `Nil`, `Duration`,
`Ordering`, the built-in containers (`List`, `Option`, `Result`, `Set`, tuples
through arity 8, `Dict`), and [anonymous records](tour-data.md) are reflectable
out of the box. A `type` of your own becomes reflectable when you add
`derive(Reflect)`, which needs `import reflect`:

```witchy
import reflect
import json

type Point derive(Reflect):
    x: Int
    y: Int

fn main(console: Console):
    console.print(json.stringify(Point(1, 2)))
    console.print(reflect.debug(Point(1, 2)))
```

```text
{"x":1,"y":2}
Point { x: 1, y: 2 }
```

Two different consumers — `json.stringify` (from `import json`) and `reflect.debug`
(a structural string, handy in tests and logs) — both read `Point` with no per-type
code. That is the payoff: derive *once*, and every reflective consumer can handle
your type. It is opt-in per type (like Zig's `@typeInfo`, but you choose which types
participate), so reflection never sees a type that did not ask to be seen.

Sum types reflect too, carrying their variant:

```witchy
import reflect
import json

type Shape derive(Reflect):
    Circle(Int)
    Square(Int, Int)

fn main(console: Console):
    console.print(reflect.debug(Square(3, 4)))
    console.print(json.stringify(Circle(5)))
```

```text
Square(3, 4)
{"$variant":"Circle","$values":[5]}
```

## The `Mirror`: inspecting structure yourself

`reflect(x)` returns a `Mirror` — the value's shape as an ordinary sum type you can
`match` on. That is the whole mechanism `json` and `debug` are built on, and you
write your own consumer the same way. The variants:

| `Mirror` variant | Shape |
|---|---|
| `MInt(Int)`, `MFloat(Float)`, `MBool(Bool)`, `MString(String)` | the scalars |
| `MNil` | the unit value |
| `MList(List(Mirror))` | a list's elements, reflected |
| `MTuple(List(Mirror))` | a tuple's slots, reflected |
| `MRecord(String, List((String, Mirror)))` | a record: type name, then `(field, value)` pairs in order |
| `MVariant(String, String, List(Mirror))` | a sum value: type name, variant name, reflected payloads |

A consumer takes `impl Reflect` and matches the `Mirror`:

```witchy
import reflect

type Point derive(Reflect):
    x: Int
    y: Int

type Shape derive(Reflect):
    Circle(Int)
    Square(Int, Int)

fn kind(value: impl Reflect) -> String:
    match reflect(value):
        MInt(_n) -> "int"
        MString(_s) -> "string"
        MRecord(name, fields) -> "${name} with ${list.length(fields)} fields"
        MVariant(_t, v, payload) -> "variant ${v}/${list.length(payload)}"
        MList(xs) -> "list of ${list.length(xs)}"
        MTuple(xs) -> "tuple of ${list.length(xs)}"
        _ -> "other"

fn main(console: Console):
    console.print(kind(42))
    console.print(kind(Point(1, 2)))
    console.print(kind(Circle(5)))
    console.print(kind([1, 2, 3]))
```

```text
int
Point with 2 fields
variant Circle/1
list of 3
```

The `Mirror` constructors are module-scoped: after `import reflect` you name them
qualified (`reflect.MInt`, `reflect.MRecord`, …), or bind the type once with
`from reflect import Mirror` to write its variants bare (`MInt`, `MRecord`, …).
`value` is taken as
`impl Reflect` — sugar for a generic parameter with a `Reflect` bound — because
`reflect(...)` dispatches on any expression whose type the checker knows (a
parameter, loop variable, constructor-pattern binding, destructured tuple slot,
or call result), which is exactly what a trait method needs to resolve (see
[Generics and Traits](tour-generics.md)).

A `MRecord` carries its fields *in declared order* and a `MVariant` names both the
type and the variant, so a single recursive walk over `Mirror` is enough to
serialize, pretty-print, diff, or hash any reflectable value — which is precisely
how `std/json` and `reflect.debug` are written.

## Decoding: the other direction

Reflection covers *encoding* — any reflectable value becomes a `Mirror`, and from
there JSON, a debug string, or whatever you traverse it into. Going the other way,
*decoding* a parsed value back into a typed record, is generated per type with
`derive(Deserialize)` (`Type.from_json(j) -> Result(Type, String)`); see
[Generics and Traits](tour-generics.md). There is deliberately no `derive(Json)`
or `to_json` — encoding is reflective and needs nothing generated, while only
decoding has to be.

## It is ordinary witchy

`Mirror` is a normal sum type, the `Reflect` impls are normal trait impls, and
`derive(Reflect)` appends normal source before type-checking — so a reflective
consumer you write runs identically on the interpreter and the compiled backend,
and audits in `witchy caps` like any other code. Reflection adds no runtime magic:
it is a trait and a data type, nothing more.
