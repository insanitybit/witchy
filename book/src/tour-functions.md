# Functions and Control Flow

## Functions

A function annotates its parameters and (optionally) its return type:

```witchy
fn add(a: Int, b: Int) -> Int:
    a + b

fn main(console: Console):
    print(console, "${add(2, 3)}")
```

```text
5
```

The body is a block; its value is the last expression — no `return` needed for
the common case (though `return` exists for early exits). Parameters *must* be
annotated; locals are inferred. A `pub fn` is exported from its module;
everything else is module-private.

Method-call syntax (`value.method(args)`) belongs to **methods** — functions
declared in an `impl` block with a `self` parameter. A plain function is
called as a function. We'll meet `impl` properly in the types chapter; the
shape looks like this:

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
    print(console, "${s.points}")
```

```text
14
```

A method without `self` is a *static*, called on the type itself:
`Score.zero()` — handy for constructors with defaults.

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
    print(console, classify(5))
    print(console, classify(0))
    print(console, classify(0 - 2))
```

```text
positive
zero
negative
```

Because `if` has a value, you rarely need a mutable variable just to pick
between two things.

## Loops

`for ... in` walks a list, a range (`lo..hi`, half-open), or a dict's views.
`while` loops on a condition. `break` and `continue` work as you'd expect.

```witchy
fn main(console: Console):
    var total = 0
    for n in 1..5:
        total = total + n
    print(console, "${total}")

    var count = 0
    for x in [10, 20, 30, 40]:
        if x == 30:
            continue
        if x > 30:
            break
        count = count + 1
    print(console, "${count}")
```

```text
10
2
```

`var` is the opt-in for mutation; `let` bindings are immutable, and assigning to
one is a compile error. Ranges are never materialized into a list — `for n in
0..1000000` allocates nothing.

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
    print(console, "${apply(add10, 5)}")
    print(console, "${apply(fn(n: Int): n * n, 6)}")
```

```text
15
36
```

"By value" means a closure can't reach back and mutate a variable from the
enclosing scope — that's a compile error, not a silent surprise. If you need to
produce a changed value, return it; if you need to mutate a caller's variable,
the next section's `inout` is the tool.

## Mutating a caller's variable: `inout`

Most functions take their arguments as read-only views (the default). When you
genuinely want a function to mutate the caller's variable, mark the parameter
`inout`; the final value is written back:

```witchy
fn bump(var n: Int):
    n = n + 1

fn main(console: Console):
    var counter = 41
    bump(counter)
    print(console, "${counter}")
```

```text
42
```

This is witchy's version of mutable references, but it's explicit at both the
definition (`inout n`) and — because the variable is visibly handed over — the
call. There's no aliasing to reason about: `bump` has the only handle to `n`
while it runs. (`var n` on a parameter means exactly the same thing as `inout n`,
if you prefer that spelling.)

## Ownership: borrow and transfer

Every parameter has an *ownership convention* that says what the function may do
with its argument. The default — no keyword — is an **owned, immutable value**:
the function reads it but the caller sees no change. Annotate it `let` to make it
an explicit **borrow**: same read-only meaning, but the compiler guarantees it
doesn't escape the call — returning a `let`-borrowed parameter is a type
error — which is what lets backends share it without a defensive copy.

```witchy
fn sum(let xs: List(Int), i: Int) -> Int:
    if i >= list.length(xs):
        0
    else:
        list.at(xs, i) + sum(xs, i + 1)

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    print(console, "${sum(xs, 0)}")
    print(console, "${list.length(xs)}")
```

```text
10
4
```

To *take* a value — so the caller can no longer use it — mark the parameter `own`
(its synonym is `sink`). Spell the hand-off `move` at the call site; afterwards,
touching the original is a compile error, not a dangling reference:

```witchy
fn into_label(own name: String) -> String:
    "[" <> name <> "]"

fn main(console: Console):
    let name = "witchy"
    print(console, into_label(move name))
    // print(console, name)   // <- compile error: `name` was moved
```

```text
[witchy]
```

So the whole model is four choices, all visible in the signature: owned-immutable
by default, `let` to borrow, `own`/`move` to transfer, `inout`/`var` to mutate the
caller's variable in place. There's no aliasing and no garbage-collector surprise
— who may change what is part of every function's type.

These conventions are also witchy's **performance knobs** (what may alias
determines what the compiler may mutate in place): see
[Appendix: Performance — the Ownership Knobs](appendix-performance.md) for
what each one means to the optimizer and when to reach for it.

Next: defining your own types.
