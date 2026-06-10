# Generics and Traits

## Generic functions

A lowercase, argument-less name in a type is a **type variable**. A function
that works for any element type just uses one:

```witchy
fn pair_up(x: a, y: a) -> (a, a):
    (x, y)

fn first(xs: List(a)) -> a:
    at(xs, 0)

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
import ord

fn largest(xs: List(a)) -> a where a: Ord:
    var best = at(xs, 0)
    for x in xs:
        if greater(x, best):     // `greater` comes from the Ord trait
            best = x
    best

fn main(console: Console):
    print(console, int_to_string(largest([3, 9, 2, 7])))
    print(console, largest(["apple", "pear", "fig"]))
```

```text
9
pear
```

`largest` works for `Int` and `String` here because both implement `Ord`, and it
would work for any type of yours that does too — implement `Ord` for it and the
same function applies. The standard `eq`, `ord`, and `show` modules provide
these traits along with generic algorithms built on them (`eq.member`,
`ord.max`, and so on).

That's the language. Everything so far has been pure — capability-free code that
computes and returns values. Now we get to the part witchy exists for: what
happens when a program needs to actually *do* something.
