# Functions and Control Flow

## Functions

A function annotates its parameters and (optionally) its return type:

```witchy
fn add(a: Int, b: Int) -> Int:
    a + b

fn main(console: Console):
    console.print("${add(2, 3)}")
```

```text
5
```

The body is a block; its value is the last expression — no `return` needed for
the common case (though `return` exists for early exits). Parameters *must* be
annotated; locals are inferred. A `pub fn` is exported from its module;
everything else is module-private.

Method-call syntax, `value.method(args)`, is the idiomatic way to call the
standard data libraries — `xs.map(f)`, `d.insert(k, v)`, `s.to_upper()` — and it
uses the same parameter conventions as free calls. A `var` receiver such as
`insert` requires a mutable place and writes back; pure transformations such as
`map` may chain. Build a dictionary value with `dict.from_pairs([("a", 1),
("b", 2)])`, since a temporary receiver has no write-back place. For standard
data types, the module-qualified form remains available too: it is the same
receiver-first module function (`dict.insert(d, k, v)`) or a compiler alias to
the method implementation (`list.map(xs, f)`). Module qualification is also
the only form when a helper lives in a module other than the receiver's type —
`json.stringify(x)`, `math.to_float(n)`. The same dot syntax also calls
**methods** you declare in an `impl` block with a `self` parameter — which we'll
meet properly in the types chapter; the shape looks like this:

```witchy
type Score:
    points: Int

impl Score:
    fn doubled(self) -> Score:
        Score(self.points * 2)

    fn bumped(self) -> Score:
        Score(self.points + 1)

fn main(console: Console):
    let s = Score(3).doubled().bumped().doubled()
    console.print("${s.points}")
```

```text
14
```

A method without `self` is a *static*, called on the type itself:
`Score.zero()` — handy for constructors with defaults.

## Labels and defaults

A direct call to a free or module function may **label** its arguments by
parameter name, and a suffix parameter may declare a **default** (a
compile-time constant), so a caller can omit it:

```witchy
fn greet(name: String, greeting: String = "hello") -> String:
    "${greeting}, ${name}"

fn main(console: Console):
    console.print(greet("ada"))                     // default greeting
    console.print(greet("bob", greeting: "hi"))     // override by label
    console.print(greet(greeting: "yo", name: "cy")) // labels may reorder
```

```text
hello, ada
hi, bob
yo, cy
```

Two rules keep this predictable. **Arguments always evaluate in source order**,
left to right, even when labels reorder them relative to the parameter list —
so a call reads the way it runs. And labels/defaults are a feature of *direct*
free and module calls only: **method calls (`x.f(...)`) and calls through a
function value are positional in v1**. When in doubt, positional always works;
reach for labels when a call site has several same-typed arguments and the
names make it readable.

## `if` is an expression

```witchy
fn classify(n: Int) -> String:
    if n > 0:
        "positive"
    else if n == 0:
        "zero"
    else:
        "negative"

fn main(console: Console):
    console.print(classify(5))
    console.print(classify(0))
    console.print(classify(0 - 2))
```

```text
positive
zero
negative
```

Because `if` has a value, you rarely need a mutable variable just to pick
between two things.

## Loops

`for ... in` walks a list, a range (`lo..hi`, half-open), or a dict — iterating a
dict directly binds each key/value pair (`for (k, v) in d:`), or use
`dict.keys(d)` / `dict.values(d)` / `dict.pairs(d)` for an explicit view.
`while` loops on a condition. `break` and `continue` work as you'd expect.

```witchy
fn main(console: Console):
    var total = 0
    for n in 1..5:
        total = total + n
    console.print("${total}")

    var count = 0
    for x in [10, 20, 30, 40]:
        if x == 30:
            continue
        if x > 30:
            break
        count = count + 1
    console.print("${count}")
```

```text
10
2
```

`var` is the opt-in for mutation; `let` bindings are immutable, and assigning to
one is a compile error. Ranges are never materialized into a list — `for n in
0..1000000` allocates nothing.

A `var` collection updates in place by subscript or field — `xs[i] = v`,
`d[k] = v`, `acct.balance = b` (and compound forms like `xs[i] += v` / `d[k] += v`). It's shorthand for
the value update (`xs.set_at(i, v)`), so witchy's value semantics hold while
it reads like mutation, and the optimizer keeps it in place:

```witchy
fn main(console: Console):
    var xs = [1, 2, 3]
    xs[0] = 9
    xs[2] += 5
    console.print("${xs}")
```

```text
[9, 2, 8]
```

## Closures

A `fn(params): body` expression is a closure. It captures the variables it uses
*by value*:

```witchy
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn adder(by: Int) -> fn(Int) -> Int:
    fn(n: Int): n + by

fn main(console: Console):
    let add10 = adder(10)
    console.print("${apply(add10, 5)}")
    console.print("${apply(fn(n: Int): n * n, 6)}")
```

```text
15
36
```

"By value" means a closure can't reach back and mutate a variable from the
enclosing scope — that's a compile error, not a silent surprise. If you need to
produce a changed value, return it; if you need to mutate a caller's variable,
the next section's `var` is the tool.

## Mutating a caller's variable: `var`

Most functions take their arguments as read-only views (the default). When you
genuinely want a function to mutate the caller's variable, mark the parameter
`var`; the final value is written back:

```witchy
fn bump(var n: Int):
    n = n + 1

fn main(console: Console):
    var counter = 41
    bump(counter)
    console.print("${counter}")
```

```text
42
```

This is witchy's version of mutable references, but it's explicit at both the
definition (`var n`) and — because the variable is visibly handed over — the
call. There's no aliasing to reason about: `bump` has the only handle to `n`
while it runs.

## Ownership: borrow and transfer

Every parameter has an *ownership convention* that says what the function may do
with its argument. The default — no keyword — is an **owned, immutable value**:
the function reads it but the caller sees no change. Annotate it `let` to make it
an explicit **borrow**: same read-only meaning, but the compiler guarantees it
doesn't escape the call — returning a `let`-borrowed parameter is a type
error — which is what lets backends share it without a defensive copy.

```witchy
fn sum(let xs: List(Int), i: Int) -> Int:
    if i >= xs.length():
        0
    else:
        xs.at(i) + sum(xs, i + 1)

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    console.print("${sum(xs, 0)}")
    console.print("${xs.length()}")
```

```text
10
4
```

To *take* a value — so the caller can no longer use it — mark the parameter `own`.
Spell the hand-off `move` at the call site; afterwards,
touching the original is a compile error, not a dangling reference:

```witchy
fn into_label(own name: String) -> String:
    "[" + name + "]"

fn main(console: Console):
    let name = "witchy"
    console.print(into_label(move name))

// console.print(name)   // <- compile error: `name` was moved
```

```text
[witchy]
```

`move` is the caller's half of the transfer, and it stands on its own: it ends the
binding *here* — a later use of `name` is a compile error — whether or not the
callee asked for `own`. (Move it into a plain owned parameter and the binding is
still consumed; the callee just got a copy.) Pairing `move` with an `own`
parameter is the idiom, and on the compiled backend that pair is a guaranteed
copy-free hand-off. You don't write `move` for a `var` parameter, though — there
you hand the variable over directly and it's written back.

So the whole model is four choices, all visible in the signature: owned-immutable
by default, `let` to borrow, `own`/`move` to transfer, `var` to mutate the
caller's variable in place. There's no aliasing and no garbage-collector surprise
— who may change what is part of every function's type.

These conventions are also witchy's **performance knobs** (what may alias
determines what the compiler may mutate in place): see
[Appendix: Performance — the Ownership Knobs](appendix-performance.md) for
what each one means to the optimizer and when to reach for it.

Next: defining your own types.
