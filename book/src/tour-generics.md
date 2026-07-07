# Generics and Traits

## Generic functions

A lowercase, argument-less name in a type is a **type variable**. A function
that works for any element type just uses one:

```witchy
fn pair_up(x: a, y: a) -> (a, a):
    (x, y)

fn first(xs: List(a)) -> a:
    xs.at(0)

fn main(console: Console):
    let (lo, hi) = pair_up(1, 2)
    print(console, "${lo}, ${hi}")
    print(console, first(["alpha", "beta"]))
```

```text
1, 2
alpha
```

`pair_up` and `first` are checked once and specialized for each concrete type
you call them with, so there's no runtime cost to being generic.

## Traits

A type variable on its own can only be moved around — copied, put in a tuple,
returned. To *do* something with it (compare it, show it, order it) the function
needs to know the type supports that operation. Traits express the requirement.

```witchy
trait Greet:
    fn greet(self) -> String

type Dog:
    Dog

type Robot:
    Robot

impl Greet for Dog:
    fn greet(self) -> String:
        "woof"

impl Greet for Robot:
    fn greet(self) -> String:
        "beep boop"

fn main(console: Console):
    print(console, Dog.greet())
    print(console, Robot.greet())
```

```text
woof
beep boop
```

A `trait` lists method signatures; an `impl ... for ...` provides them for a
concrete type. `Self` inside an impl refers to the implementing type.

## Bounded generics

Now combine the two: a generic function that requires its type to implement a
trait, written `where a: TraitName`. The standard library's `Ord` trait gives
ordering; here's a generic "biggest element" for anything orderable:

```witchy
import cmp

fn largest(xs: List(a)) -> a where a: Ord:
    var best = xs.at(0)
    for x in xs:
        if x > best:
            best = x
    best

fn main(console: Console):
    print(console, "${largest([3, 9, 2, 7])}")
    print(console, largest(["apple", "pear", "fig"]))
```

```text
9
pear
```

`largest` works for `Int` and `String` here because both implement `Ord`, and it
would work for any type of yours that does too — implement (or derive) the
comparison hierarchy for it and the same function applies. The `>` operator
desugars to the type's `Ord` impl, so you never call a `greater`/`compare`
function by name. The standard `cmp` and `show` modules provide these traits
(`PartialEq` → `Eq` → `PartialOrd` → `Ord`) along with generic algorithms built
on them (`list.contains`, `list.sort`, `cmp.max_of`, and so on).

## `impl Trait` and rendering with `Show`

When a parameter is generic *only* to carry a bound, write `x: impl Trait` — it
is exactly a fresh type variable plus a `where` bound, so it reuses everything
above. The `Show` trait (`fn show(self) -> String`) is the idiomatic case:
implement it to give a type a custom rendering, and take `impl Show` wherever you
want to accept "anything renderable".

```witchy
import show

type Temp:
    celsius: Int

impl Show for Temp:
    fn show(self) -> String:
        "${self.celsius} deg C"          // a custom rendering for Temp

fn announce(console: Console, label: String, x: impl Show):
    print(console, "${label}: ${show(x)}")

fn main(console: Console):
    announce(console, "now", Temp(21))   // uses Temp's Show
    announce(console, "count", 42)       // and Int's
    show.say(console, Temp(5))           // `say` = the Show-accepting `print`
```

```text
now: 21 deg C
count: 42
5 deg C
```

`say(console, x)` is the `Show`-accepting `print` — reach for it instead of
`print(console, "${x}")`. Note the division of labor: interpolation and the
built-in rendering already covers *every* value structurally (including bare
lists, tuples, and dicts, which can't carry a `Show` impl); `Show` is for giving
*your own* types a rendering you choose.

## Deriving the common traits

For a record, the obvious comparison and `Show` impls are mechanical — so the
compiler writes them for you. List each trait you want in `derive(...)`, the same
way Rust does (`PartialEq, Eq, PartialOrd, Ord`):

```witchy
import cmp

type Score derive(Show, PartialEq, Eq, PartialOrd, Ord):
    points: Int
    label: String

fn main(console: Console):
    let a = Score(10, "alpha")
    let b = Score(12, "beta")
    print(console, "${a == Score(10, "alpha")}")   // derived PartialEq
    print(console, "${cmp.max_of(a, b)}")          // derived Ord (field order)
```

```text
true
Score(12, beta)
```

- `derive(Show)` renders the record structurally (`Score(12, beta)`), which
  also feeds `${...}` and `say`.
- `derive(PartialEq)` is field-by-field structural equality (it backs `==`/`!=`);
  `derive(Eq)` marks it as a total equality, usable as a `Set`/`Dict` key.
- `derive(PartialOrd)`/`derive(Ord)` compare fields lexicographically, in
  declaration order, and back the `<` `>` `<=` `>=` operators. These two are
  **records only** (a single constructor with named fields); deriving them on an
  enum or multi-variant sum type is an error, so order an enum with a
  hand-written `impl Ord`. (`Show`/`PartialEq`/`Eq` derive for any type.) Note a
  derived `Ord` requires every field's type to be `Ord` too — derive it on the
  field types as well.
- `derive(Reflect)` (with `import reflect`) makes the record *reflectable*, so
  the reflection-based encoders serialize it with no per-type code:
  `json.stringify(score)` returns `{"points":12,"label":"beta"}` and
  `json.from_value(score)` the `Json` value. Scalars, lists, options, and nested
  `derive(Reflect)` records all map. (There is **no** `derive(Json)` /
  `to_json`; serialization is reflective, only decoding is generated — next.)
  When you don't even want a named type, an [anonymous record](tour-data.md)
  (`.{field: expr}`) is reflectable too, so `json.stringify(.{ok: true})` works
  with no declaration at all. [Reflection](tour-reflection.md) covers the full
  story — the `Mirror` type and writing your own reflective consumers.
- `derive(Deserialize)` (with `import json`, `import result`, and `import
  option` if any field is `Option`) generates the inverse,
  `from_json(j) -> Result(Self, String)`, so `Score.from_json(j)` rebuilds a
  record from a parsed `Json` and reports a bad shape as `Err`.

Because an operator dispatches on its operands' type, a comparison works
wherever the value comes from — including one you just bound in a `match` arm, an
`if let`, or a tuple destructure:

```witchy
import cmp

type Version derive(Show, PartialEq, Eq, PartialOrd, Ord):
    major: Int
    minor: Int

fn parse(s: String) -> Option(Version):
    match s.split("."):
        [major, minor] -> Some(Version(string.to_int(major), string.to_int(minor)))
        _ -> None

fn main(console: Console):
    if let Some(v) = parse("1.4"):
        print(console, "${v == Version(1, 4)}")   // `v` is bound by `if let`
        print(console, "${v < Version(2, 0)}")
```

```text
true
true
```

Write an explicit `impl` only when you want behavior the mechanical version
doesn't give you — a custom display format, a comparison that ignores a field.
A derive and a hand-written impl of the same trait on the same type is an
error, not an override.

That rounds out the type system. One more pure-language idea remains before we
reach the heart of witchy — describing sequences that are computed on demand
rather than all at once.

Next: generators and iterators.
