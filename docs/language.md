# The witchy language reference

This is the reference for witchy's syntax and semantics. The behavioral
contract is enforced by differential testing: the tree-walking interpreter is
the reference semantics, and the compiled backends (WebAssembly, native) must
produce identical results — `witchy parity <file>` checks any program, and the
test suite holds the backends to **zero silent divergence**, including error
paths.

Every ` ```witchy ` block below is a complete program that the test suite
type-checks — and, when it needs nothing beyond `Console`, runs on both
backends and confirms the output matches. The examples are executable, not
illustrative.

Companion documents: [capabilities.md](capabilities.md) (the security model),
[stdlib.md](stdlib.md) (the module-by-module API), [architecture.md](architecture.md)
(how the compiler is built).

## 1. Lexical structure

**Layout.** Blocks are indentation-delimited (the off-side rule), opened by a
trailing `:`. Four spaces per level is canonical (`witchy fmt` enforces it).

```witchy
fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"

fn main(console: Console):
    print(console, classify(0))
    print(console, classify(7))
    print(console, classify(0 - 3))
```

**Comments.** `//` to end of line; `/* ... */` blocks. `witchy fmt` preserves
comments, including in-body and nested ones.

**Identifiers.** `lower_snake_case` for functions/variables, `UpperCamel` for
types and constructors. A lowercase, argument-less name in type position
(`a`, `b`) is a type variable.

**Literals.**

| Form | Type | Notes |
|---|---|---|
| `42`, `-7` | `Int` | 64-bit signed; arithmetic **wraps** on overflow (two's complement, both backends) |
| `3.5`, `0.5` | `Float` | IEEE-754 double; `to_string` renders the shortest round-trip form |
| `true` / `false` | `Bool` | |
| `"hi\n"` | `String` | UTF-8; escapes `\n \t \r \0 \\ \" \$` |
| `"sum: ${a + b}"` | `String` | interpolation — `${expr}` splices `to_string(expr)` |
| `30s`, `250ms`, `5m`, `2h`/`2hr`, `1d`, `1w` | `Duration` | a distinct type carried as milliseconds; not mixable with bare `Int` |
| `[1, 2, 3]` | `List(Int)` | immutable |
| `(1, "a")` | tuple | fixed arity, mixed types |

```witchy
fn main(console: Console):
    let a = 6
    let b = 7
    print(console, "sum: ${a + b}")          // string interpolation
    print(console, to_string(1500ms < 2s))   // durations are a distinct type
    let pair = (1, "a")                       // a tuple
    let (n, s) = pair
    print(console, "${n}${s}")
```

## 2. Types

Builtins: `Int`, `Float`, `Bool`, `String`, `Duration`, `Nil` (the unit type),
`List(a)`, `Dict(k, v)`, tuples `(a, b, ...)`, function types
`fn(Int, String) -> Bool`, and the capability types (`Console`, `Clock`, `Env`,
`Dir[...]`, `Net[...]`, `SigningKey` — see [capabilities.md](capabilities.md)).

**Algebraic data types.** One `type` declaration covers enums, tagged unions,
and records:

```witchy
type Color:                 // enum: nullary variants
    Red
    Green
    Blue

type Shape:                 // sum type: variants with positional fields
    Circle(Int)
    Square(Int)

type Account:               // record: a single variant with named fields
    name: String
    balance: Int

fn main(console: Console):
    print(console, to_string(Red == Red))
    let acc = Account("ada", 100)                 // positional construction
    let named = Account(name: "bob", balance: 5)  // by-name construction
    print(console, acc.name)                       // field access
    let richer = Account(balance: acc.balance + 1, ..acc)  // functional update
    print(console, int_to_string(richer.balance))
    print(console, int_to_string(named.balance))
```

Records construct positionally (`Account("ada", 100)`) or by name; field access
is `account.name`; the spread form `Account(balance: ..., ..acc)` makes a fresh
record (overrides first, then `..base`).

`Option(a)` (`Some(x)` / `None`) and `Result(a, e)` (`Ok(x)` / `Err(e)`) come
from `import option` / `import result`.

**Type aliases.** `type Id = Int` names a type without creating a new one.

## 3. Bindings and assignment

```witchy
fn bounds(xs: List(Int)) -> (Int, Int):
    (at(xs, 0), at(xs, length(xs) - 1))

fn main(console: Console):
    let x = 1          // immutable binding
    var count = 0      // mutable binding
    count = count + 1  // assignment (only to `var`)
    let (lo, hi) = bounds([3, 5, 9])   // tuple destructuring
    print(console, int_to_string(x + count))
    print(console, "${lo}..${hi}")
```

Top-level `let` declares a module constant (inlined at compile time).
Assigning to a `let`, or to a variable captured by a closure, is a check-time
error (closures capture **by value**; return the new value or use `inout`).

## 4. Expressions and operators

Everything is an expression; a block's value is its final expression.

| Operators | Meaning |
|---|---|
| `+ - * / %` | arithmetic (`Int` wraps; `/ 0` and `Int.MIN / -1` are runtime errors on every backend) |
| `<>` | string concatenation |
| `== !=` | **structural** equality — deep, on lists, tuples, records, enums, `Option`, `Dict` (insertion-order-sensitive), on every backend |
| `< <= > >=` | ordering on `Int`/`Float`/`String`/`Duration` only; ordering a NaN is a runtime error; compounds don't order |
| `&& \|\|` | short-circuit boolean |
| `!` | negation |
| `& \| ^ ~ << >>` | bitwise on `Int` (shifts mask the count to 6 bits) |
| `xs[i]` | list indexing, sugar for `at(xs, i)`; out of bounds is a runtime error on every backend |
| `lo..hi` | a half-open range (for-loop iteration; never materialized) |
| `x.f(args)` | method-call sugar for `f(x, args)` (UFCS) |
| `e?` | unwrap `Ok`/`Some` or return the `Err`/`None` from the enclosing function |
| `cap as Dir[Read]` | capability narrowing (drop rights; never widen) |

```witchy
fn double(n: Int) -> Int:
    n * 2

fn main(console: Console):
    print(console, int_to_string(7 % 3))
    print(console, "a" <> "b")
    print(console, to_string([1, 2] == [1, 2]))     // structural equality
    print(console, to_string(2.5 < 3.0))
    let xs = [10, 20, 30]
    print(console, int_to_string(xs[1]))            // indexing sugar
    print(console, int_to_string(8.double()))       // UFCS: double(8)
```

Float notes: `0.0 / 0.0` is NaN; `1.0 / 0.0` is infinity; NaN `==` anything is
`false` (IEEE), while NaN *ordering* errors. Conversions: `int_to_float`,
`float_to_int` (saturating truncation), `string_to_int` (strict; errors on
junk or overflow), `sqrt`, `to_string`.

## 5. Control flow

```witchy
fn main(console: Console):
    let n = 5
    if n > 10:
        print(console, "big")
    else if n > 3:
        print(console, "medium")
    else:
        print(console, "small")

    var total = 0
    for x in [1, 2, 3, 4]:        // lists, ranges (0..n), and dict views
        if x == 2:
            continue
        if x > 3:
            break
        total = total + x
    print(console, int_to_string(total))   // 1 + 3 = 4

    var i = 0
    while i < 3:
        i = i + 1
    print(console, int_to_string(i))
```

`if let PAT = e:` binds and runs only on a match (with an optional `else`);
`while let PAT = e:` loops as long as the scrutinee keeps matching. `return e`
exits early (and works in `inout` functions — the written-back parameters are
still delivered).

A `retain a, b:` / `without a, b:` block is a capability firewall: inside it,
only the named capabilities stay in scope (`retain`) or the named ones are
dropped (`without`). It is a compile-time scoping restriction — the checker hides
the bindings, every backend runs the block normally — that seals a region of code
against capabilities the surrounding scope holds (or later gains). `retain:` with
no names drops all of them. See `docs/capabilities.md`.

```witchy
import option

fn first_even(xs: List(Int)) -> Option(Int):
    for x in xs:
        if x % 2 == 0:
            return Some(x)
    None

fn main(console: Console):
    if let Some(v) = first_even([1, 3, 4, 5]):
        print(console, "first even: ${v}")
    else:
        print(console, "none")
```

## 6. Pattern matching

`match` is exhaustiveness-checked (missing variants are named in the error) and
unreachable arms are rejected. Patterns: literals, `_`, variables, constructors
with nested patterns, tuples, list shapes (`[]`, `[first, ..rest]`), and guards
(`PAT if cond ->`, which don't count toward exhaustiveness).

```witchy
type Shape:
    Circle(Int)
    Square(Int)

fn describe(s: Shape) -> String:
    match s:
        Circle(r) if r > 100 -> "big circle"
        Circle(r) -> "circle " <> int_to_string(r)
        Square(w) -> "square " <> int_to_string(w)

fn head(xs: List(Int)) -> String:
    match xs:
        [] -> "empty"
        [first, ..rest] -> "first " <> int_to_string(first) <> ", " <> int_to_string(length(rest)) <> " more"

fn main(console: Console):
    print(console, describe(Circle(2)))
    print(console, describe(Square(5)))
    print(console, head([10, 20, 30]))
    print(console, head([]))
```

## 7. Functions

```witchy
type Shape:
    Circle(Int)
    Square(Int)

pub fn area(s: Shape) -> Int:      // `pub` exports from the module
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    print(console, int_to_string(area(Circle(2))))
    print(console, int_to_string(area(Square(3))))
```

Parameter annotations are required; the return type may be inferred for
non-`pub` functions. Locals are inferred (Hindley-Milner-style unification with
an occurs check).

**Parameter conventions** (Hylo-style value semantics):

| Convention | Meaning |
|---|---|
| (default / `let`) | immutable view; the native backend passes a borrow (no clone) |
| `inout` | the callee mutates and the caller's `var` is **written back** — even on early `return`/`?` |
| `sink` / `own` | ownership transfer; using the source afterwards is a check-time error |
| `move e` | explicitly transfer a binding at a call site |

```witchy
fn bump(inout n: Int):
    n = n + 1

fn main(console: Console):
    var counter = 41
    bump(counter)            // counter is written back
    print(console, int_to_string(counter))   // 42
```

**Closures.** `fn(n: Int): n + by` captures by value; you call through a
`fn(...)` -typed value or parameter. Closures cannot assign to captured
variables (check-time error).

```witchy
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn adder(by: Int) -> fn(Int) -> Int:
    fn(n: Int): n + by

fn main(console: Console):
    let add10 = adder(10)
    print(console, int_to_string(apply(add10, 5)))   // 15
```

## 8. Generics and traits

```witchy
import ord

fn largest(xs: List(a)) -> a where a: Ord:
    var best = at(xs, 0)
    for x in xs:
        if greater(x, best):      // `greater` comes from the Ord trait
            best = x
    best

fn main(console: Console):
    print(console, int_to_string(largest([3, 9, 2, 7])))
    print(console, largest(["apple", "pear", "fig"]))
```

Generic functions are checked once and monomorphized per concrete use for the
compiled backends (both `where`-bounded and unbounded generics). `Self` in an
impl refers to the implementing type. Trait method calls inside a
`where a: Trait` function resolve on parameters and loop variables; an
intermediate expression may need a `let` first.

You can define traits and impls too:

```witchy
trait Greet:
    fn greet(self) -> String

type Dog:
    Dog

impl Greet for Dog:
    fn greet(self) -> String:
        "woof"

fn main(console: Console):
    print(console, Dog.greet())
```

The std `Eq`/`Ord`/`Show` traits (`import eq`, ...) provide bounded generic
algorithms (`eq.member`, `ord.max`, ...).

## 9. Errors, `Option`/`Result`, and failure

witchy has no exceptions. Expected failure is a value:

```witchy
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn ratio(a: Int, b: Int, c: Int) -> Result(Int, String):
    let first = checked_div(a, b)?      // Err short-circuits to the caller
    checked_div(first, c)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok: " <> int_to_string(v)
        Err(e) -> "err: " <> e

fn main(console: Console):
    print(console, show(ratio(100, 5, 2)))
    print(console, show(ratio(100, 0, 2)))
```

Unexpected failure is **loud on every backend**: out-of-bounds indexing,
division by zero, unparseable `string_to_int`, NaN ordering, and the `fail(msg)`
primitive all abort (a runtime error interpreted, a trap compiled). The parity
invariant covers these too — a program that errors on one backend errors on
both.

## 10. Comprehensions

`[elem for x in iter]`, optionally filtered with `if cond`, builds a list:

```witchy
import list
import string

fn show(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), " ")

fn main(console: Console):
    let squares = [n * n for n in 1..6]
    print(console, show(squares))                 // 1 4 9 16 25
    let evens = [n for n in 1..11 if n % 2 == 0]
    print(console, show(evens))                   // 2 4 6 8 10
```

## 11. Generators

A `gen fn` body runs imperatively but `yield`s a sequence; calling it returns a
lazy `iter.Iter` that computes only what's demanded, so an infinite generator
is fine when something bounds it.

```witchy
import iter
import list
import string

gen fn fibs() -> Iter(Int):
    var a = 0
    var b = 1
    while true:
        yield a
        let nxt = a + b
        a = b
        b = nxt

fn main(console: Console):
    let first8 = iter.collect(iter.take(fibs(), 8))
    print(console, string.join(list.map(first8, fn(n: Int): int_to_string(n)), " "))
    // 0 1 1 2 3 5 8 13
```

## 12. Modules and the standard library

```witchy
import list
import string

fn main(console: Console):
    let shouted = list.map(["a", "b", "c"], fn(s: String): to_upper(s))
    print(console, string.join(shouted, "-"))   // A-B-C
```

`import name` brings a module in under its name; calls are module-qualified
(`list.map`). Resolution order: a sibling `name.witchy` file, then the bundled
standard library (30+ modules — see [stdlib.md](stdlib.md)). `pub` items are
importable; everything else is module-private. Package dependencies ("runes")
come from the manifest — see [package-manager.md](package-manager.md).

## 13. Entry point

The program's root authority. `main` may take any number of **capability**
parameters plus an optional `args: List(String)` (the command-line arguments),
and may return `Nil` or `Int` (the process exit code):

```witchy
fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:
    print(console, "running with ${length(args)} arg(s)")
    0
```

The host mints exactly these capabilities and nothing else. (This block
type-checks but isn't run by the doc harness, since it needs `Dir` — run it
with `witchy sandbox --dir <root> prog.witchy a b c`.)

## 14. Actors

```witchy
actor Logger:
    console: Console            // immutable state field
    var count: Int = 0          // mutable state field

    on Log(line: String):
        count = count + 1
        print(console, line)

fn main(console: Console):
    let logger = spawn Logger(console)
    send(logger, Log("hello"))
    send(logger, Log("world"))
```

`spawn` creates an actor with its declared state (capabilities are granted
explicitly at spawn — attenuated, if you choose) and returns a `Subject`.
`send(subject, Msg(...))` is validated at check time against the declared
handlers (unknown messages, wrong arity, and wrong field types are errors).
Messages are copied; actors share nothing. Compiled actors run one VM per actor
(own memory, own grant), preemptible by the scheduler at loop back-edges.

## 15. In-language tests

```witchy
import testing

fn double(n: Int) -> Int:
    n * 2

fn test_doubling():
    testing.assert_int_eq(double(21), 42)
```

`witchy test <file|dir>` runs every zero-parameter `test_*` function; a test
fails by aborting (the `testing` assertions abort with a message). Tests take no
capabilities, so a suite provably has no effects.

## 16. Semantics guarantees (the parity contract)

The full behavioral surface below is identical on the interpreter and the
compiled backends, verified by differential tests:

- Integer arithmetic wraps; division/modulo by zero errors; shifts mask.
- Float formatting is shortest-round-trip everywhere; NaN/±infinity behave
  identically; NaN ordering errors.
- Equality is structural and deep for every comparable type; `Dict` equality is
  insertion-order-sensitive; multi-parameter generic payloads (`Result`) are
  the one compile-time-rejected comparison.
- String operations are byte-precise across backends; `trim`/case-mapping are
  ASCII-scoped by design.
- Out-of-bounds, overflow-on-parse, and `fail` abort on every backend.
- Capability confinement (`Dir` path resolution, `Net` allowlists) uses the
  same rules in the interpreter and the sandbox.

Anything a backend cannot run identically is a **loud compile or runtime error,
never a silently different answer**.
