# The witchy language reference

This is the reference for witchy's syntax and semantics. The behavioral
contract is enforced by differential testing: the tree-walking interpreter is
the reference semantics, and the compiled backend (WebAssembly) must produce
identical results. The test suite and a CI sweep (the project's
`witchy parity` harness) hold the backends to **zero silent divergence**,
including error paths.

Every ` ```witchy ` block below is a complete program that the test suite
type-checks — and, when it needs nothing beyond `Console`, runs on both
backends and confirms the output matches. The examples are executable, not
illustrative.

Companion documents: [capabilities.md](capabilities.md) (the security model),
[stdlib.md](stdlib.md) (the module-by-module API), [architecture.md](architecture.md)
(how the compiler is built).

**Index:**
[1. Lexical structure](#1-lexical-structure) ·
[2. Types](#2-types) ·
[3. Bindings and assignment](#3-bindings-and-assignment) ·
[4. Expressions and operators](#4-expressions-and-operators) ·
[5. Control flow](#5-control-flow) ·
[6. Pattern matching](#6-pattern-matching) ·
[7. Functions](#7-functions) ·
[8. Generics and traits](#8-generics-and-traits) ·
[9. Errors, `Option`/`Result`, and failure](#9-errors-optionresult-and-failure) ·
[10. Comprehensions](#10-comprehensions) ·
[11. Generators](#11-generators) ·
[12. Modules and the standard library](#12-modules-and-the-standard-library) ·
[13. Entry point](#13-entry-point) ·
[14. Concurrency](#14-concurrency-async-spawn-and-channels) ·
[15. In-language tests](#15-in-language-tests) ·
[16. Semantics guarantees](#16-semantics-guarantees-the-parity-contract)

## 1. Lexical structure

**Layout.** Blocks are indentation-delimited (the off-side rule), opened by a
trailing `:`. Four spaces per level is canonical (`witchy fmt` enforces it).

```witchy
fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"

fn main(console: Console):
    console.print(classify(0))
    console.print(classify(7))
    console.print(classify(-3))
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
| `3.5`, `0.5` | `Float` | IEEE-754 double; `${...}` renders the shortest round-trip form |
| `true` / `false` | `Bool` | |
| `"hi\n"` | `String` | UTF-8; escapes `\n \t \r \0 \\ \" \$` |
| `"sum: ${a + b}"` | `String` | interpolation — `${expr}` renders data values (see below); inner strings may be bare (`"${f("x")}"`) or escaped (`"${f(\"x\")}"`) |
| `30s`, `250ms`, `5m`, `2h`/`2hr`, `1d`, `1w` | `Duration` | a distinct type carried as milliseconds; not mixable with bare `Int`; interpolation uses the prelude `Show` impl (`duration.human`) |
| `[1, 2, 3]` | `List(Int)` | immutable |
| `(1, "a")` | tuple | fixed arity, mixed types; elements read by position (`pair.0`, `grid.0.1`) or destructured (`let (n, s) = pair`) |

```witchy
fn main(console: Console):
    let a = 6
    let b = 7
    console.print("sum: ${a + b}") // string interpolation
    console.print("${1500ms < 2s}") // durations are a distinct type
    let pair = (1, "a") // a tuple
    let (n, s) = pair
    console.print("${n}${s}")
```

**Rendering values to strings.** Reach for interpolation first: `"${x}"` renders
ordinary data values — scalars, record fields, lists, tuples, records, sum types,
dicts, and their supported nesting — identically on both backends. Function
values are not data rendering targets and are rejected before runtime. `Show`
is preluded, so interpolation always honors a relevant implementation; imports
never select rendering semantics. Thus
`"${90000ms}"`, `show.render(90000ms)`, and `show.say(console, 90000ms)` all
produce `1m30s`. Tuple `Show`/`Reflect` protocol impls are provided through
arity 8; larger tuples still exist as structural values, but should be modeled as
records or lists when they need protocol-backed display or reflection. You rarely
need to call a conversion by hand. To print one value, use
`console.print("${x}")`, or `show.say(console, x)` for any `Show` value (the
built-in scalars and your own types). The **`Show` trait**
(`fn show(self) -> String`) is the trait-method route: implement it to give a
type a custom rendering.

## 2. Types

Builtins: `Int`, `Float`, `Bool`, `String`, `Duration`, `Nil` (the unit type),
`List(a)`, `Dict(k, v)`, tuples `(a, b, ...)`, function types
`fn(Int, String) -> Bool`, and the capability types (`Console`, `Clock`, `Env`,
`Dir[...]`, `File[...]`, `Net[...]`, `Exec`, `SecretStore`, `Secret` — see
[capabilities.md](capabilities.md)).

**Algebraic data types.** One `type` declaration covers enums, tagged unions,
and records:

```witchy
type Color:
    Red
    Green
    Blue

type Shape:
    Circle(Int)
    Square(Int)

type Account:
    name: String
    balance: Int

fn main(console: Console):
    console.print("${Red == Red}")
    let acc = Account("ada", 100)
    let named = Account(name: "bob", balance: 5)
    console.print(acc.name)
    let richer = Account(balance: acc.balance + 1, ..acc)
    console.print("${richer.balance}")
    console.print("${named.balance}")
```

Records construct positionally (`Account("ada", 100)`) or by name; field access
is `account.name`; the spread form `Account(balance: ..., ..acc)` makes a fresh
record (overrides first, then `..base`).

`Option(a)` (`Some(x)` / `None`) and `Result(a, e)` (`Ok(x)` / `Err(e)`) come
from `import option` / `import result`.

**Type aliases.** `type X = …` names a shape without creating a new type —
`type Id = Int` makes `Id` and `Int` fully interchangeable, and the alias may
be generic (`type Pair(a) = (a, a)`; `Pair(Int)` is `(Int, Int)`). The `=` vs
`:` distinction is the rule to remember: **`type X = …` names a shape and
never mints a type; `type X: …` mints a nominal type with constructors** —
only the latter can be sealed, carry `impl`s, or hold an invariant. Alias
cycles are a link-time error.

**Structural records and anonymous unions.** The structural tier is the family
of types that are named by shape instead of by declaration: tuples `(A, B)`,
function types `fn(A) -> B`, anonymous records `.{field: Type}`, and anonymous
tagged unions `.[Tag | Tag(Payload)]`.

Anonymous records are exact-shape records. You can write their type in
parameters, returns, fields, aliases, and generic arguments; field order does
not affect identity, but there is no width subtyping. `.{x: Int, y: Int}` and
`.{y: Int, x: Int}` are the same shape; `.{x: Int}` is not a smaller subtype.
Use spread (`.{x: ..., ..base}`) to make updated values.

Anonymous tagged unions are closed tag sets. A value is injected with a leading
dot (`.Missing`, `.BadPort(70000)`) and must have an expected union type from a
return annotation, `let` annotation, call argument, or enclosing constructor.
The only implicit width rule is union widening: `.[A | B]` may flow into
`.[A | B | C]` at argument, return/tail, and `?` propagation sites. Records do
not widen.

Capabilities cannot appear anywhere inside anonymous record fields or anonymous
union payloads. Structural types also cannot receive user `impl`s, even through
an alias; behavior and invariants belong on nominal `type X:` declarations.

```witchy
type Point = .{x: Int, y: Int}
type ParseErr = .[BadPort(Int) | Missing(String)]
type LoadErr = .[NotFound | BadPort(Int) | Missing(String)]

fn move_right(p: Point) -> .{y: Int, x: Int}:
    .{x: p.x + 1, ..p}

fn parse_port(kind: Int) -> Result(Int, ParseErr):
    if kind == 0:
        Ok(8080)
    else if kind == 1:
        Err(.BadPort(70000))
    else:
        Err(.Missing("port"))

fn load(kind: Int) -> Result(Int, LoadErr):
    let port = parse_port(kind)? // widens ParseErr into LoadErr
    Ok(port)

fn describe(r: Result(Int, LoadErr)) -> String:
    match r:
        Ok(port) -> "ok:${port}"
        Err(.NotFound) -> "not found"
        Err(.BadPort(p)) -> "bad:${p}"
        Err(.Missing(k)) -> "missing:" + k

fn main(console: Console):
    console.print("${move_right(.{y: 2, x: 1})}")
    console.print(describe(load(0)))
    console.print(describe(load(1)))
    console.print(describe(load(2)))
```

**Sealed types.** Prefixing a declaration with `sealed` makes construction the
private business of the defining module: outside code cannot call the data
constructor and must go through the module's public functions — so those
"smart constructors" are the one place an invariant is established, and a
value of the type is *proof* the invariant holds. Sealing restricts
**construction only**: field reads and `match` work from anywhere.

```witchy
sealed type Percent:
    value: Int

// The one choke point: every Percent in the program came through here,
// so `0 <= value <= 100` holds everywhere without re-checking.
pub fn percent(n: Int) -> Percent:
    if n < 0:
        Percent(0)
    else if n > 100:
        Percent(100)
    else:
        Percent(n)

fn main(console: Console):
    let p = percent(140)
    console.print("${p.value}")
```

This is the same mechanism that makes capabilities unforgeable (only the host
mints a `Net`); `sealed type` opens it to your own types, and the standard
library uses it widely — `Set` (distinct members), `semver.Version`
(non-negative components), `time.DateTime` (a real calendar date). See
[capabilities.md](capabilities.md) for the capability-specific form
(`capability X from U`).

## 3. Bindings and assignment

```witchy
fn bounds(xs: List(Int)) -> (Int, Int):
    (xs.at(0), xs.at(xs.length() - 1))

fn main(console: Console):
    let x = 1
    var count = 0
    count = count + 1
    let (lo, hi) = bounds([3, 5, 9])
    console.print("${x + count}")
    console.print("${lo}..${hi}")
```

`let x: Type = e` ascribes the binding: the annotation is a unification
constraint, so it pins type variables the value leaves open (`let xs:
List(Int) = []`, a return-position type variable) and a disagreeing value
errors at the binding line. Locals stay inferred by default — ascribe for
ambiguous literals, checked documentation, and catching a wrong assumption
where it is made.

Top-level `let` declares a module constant (inlined at compile time).
Assigning to a `let` is a check-time error. A closure also cannot assign to a
variable it captured: closures capture **by value**, so return the new value or
use a `var` parameter when a closure-like helper should write through.
`let _ = expr` evaluates and discards — the same meaning as the bare
expression statement, which is the form `fmt` prints.

**Assigning to a place.** Beyond a bare variable, the left of `=` may be a
subscript or a field — `xs[i] = v`, `d[k] = v`, `acct.balance = b` (the binding
must be a `var`). Each is sugar for reassigning the variable
(`xs = xs.set_at(i, v)`, `acct = Account(balance: b, ..acct)`), so it keeps value
semantics while reading like in-place mutation — and the uniqueness analysis makes
it an in-place update. Compound forms (`xs[i] += v`, `d[k] += v`) work too.

```witchy
type Account:
    name: String
    balance: Int

fn main(console: Console):
    var xs = [1, 2, 3]
    xs[0] = 9
    xs[1] += 5
    var acct = Account("ada", 100)
    acct.balance += 50
    console.print("${xs}")
    console.print("${acct.balance}")
```

A **method call used as a statement** on a `var` place belongs to the same
family, and it writes back by **declaration**: a function is a *mutator* when
its first parameter is a `var` receiver whose type it also returns
(`fn push(var xs: List(a), x: a) -> List(a)`). A statement-position call to a
mutator on a `var` place writes its result back, so `xs.push(v)` *is*
`xs = list.push(xs, v)` and `d.insert(k, v)` mutates `d` in place; the signature
in [the stdlib reference](stdlib.md) shows the `var` receiver, so which functions
mutate is documented where it is declared. A call that is *not* a mutator and
whose result is thrown away is a **compile error** — bind it, reassign it, or
discard it explicitly with `let _ =` — which catches the mistake of calling a
value-returning method (`xs.length()`, `xs.map(f)`) and forgetting its result.
A mutator statement on a `let` place is likewise an error (declare it `var`, or
bind the result). In expression position a method call is an ordinary value call
— `let ys = xs.push(4)` builds a new list and leaves `xs` alone.

```witchy
fn main(console: Console):
    var xs = []
    xs.push(1)
    xs.push(2)
    let _ = xs.length() // explicit discard — `length` is not a mutator
    console.print("${xs}") // [1, 2]
```

## 4. Expressions and operators

Everything is an expression; a block's value is its final expression.

| Operators | Meaning |
|---|---|
| `+ - * / %` | arithmetic (`Int` wraps; `/ 0` and `Int.MIN / -1` are runtime errors on every backend); `+` on two Strings concatenates — never coerces |

| `== !=` | equality through `PartialEq`, at **every depth** — the derived/default impl is deep structural equality (lists, tuples, records, enums, `Option`, `Dict` insertion-order-sensitive); a **custom `impl PartialEq`** is honored inside containers too (a `List(P)`/`Option(P)`/tuple/`Dict` value of a type with a hand impl compares by that impl). Function and capability types do **not** compare — `==` on them is a compile-time error |
| `< <= > >=` | ordering on `Int`/`Float`/`String`/`Duration` only; ordering a NaN is a runtime error; compounds don't order |
| `&&` | short-circuit boolean **and** (Bool operands) |
| `\|\|` | short-circuit boolean **or** (Bool operands only — for a fallback value use `??`) |
| `??` | the **fallback** operator: `Option(T) ?? T -> T` unwraps `Some` or yields the fallback on `None`; `Result(T, e) ?? T -> T` unwraps `Ok` or yields the fallback on `Err` (the error is discarded — reach for `?` / `match` when it matters). The fallback is evaluated **lazily** (only on `None`/`Err`). Right-associative and the loosest binary operator, so `d.get(k1) ?? d.get(k2) ?? 0` chains and `d.get(k) ?? n + 1` is `d.get(k) ?? (n + 1)`. There is no truthiness: `""` and `[]` are values, not absences — default them with an explicit test (`if name.is_empty(): "anon" else: name`) |
| `!` | negation |
| `& \| ^ ~ << >>` | bitwise on `Int` (shifts mask the count to 6 bits) |
| `xs[i]`, `d[k]` | strict indexing, sugar for `list.at(xs, i)` / `dict.at(d, k)`; out of bounds or missing-key reads are runtime errors on every backend |
| `lo..hi` | a half-open range (for-loop iteration; never materialized) |
| `x.f(args)` | a method call: an `impl`/trait method on `x`, **or** the stdlib UFCS form `module.f(x, args)` for a built-in type (so `xs.map(f)` *is* `list.map(xs, f)`) |
| `e?` | unwrap `Ok`/`Some` or return the `Err`/`None` from the enclosing function |
| `e? "msg"` | like `e?` with context: a `Result` `Err` gets `"msg: "` prepended; an `Option` `None` becomes `Err("msg")` |
| `cap as Dir[Read]` | capability narrowing (drop rights; never widen) |

```witchy
fn double(n: Int) -> Int:
    n * 2

fn main(console: Console):
    console.print("${7 % 3}")
    console.print("a" + "b")
    console.print("${[1, 2] == [1, 2]}")
    console.print("${2.5 < 3.0}")
    let xs = [10, 20, 30]
    console.print("${xs[1]}")
    console.print("${list.head(xs) ?? 0}")
```

Float notes: `0.0 / 0.0` is NaN; `1.0 / 0.0` is infinity; NaN `==` anything is
`false` (IEEE), while NaN *ordering* errors. Conversions: `math.to_float`,
`math.to_int` (saturating truncation), `string.to_int` (strict; ABORTS on
junk or overflow — `string.parse_int` is the `Option`-returning version), `math.sqrt`, and `${...}` for rendering to strings.

## 5. Control flow

```witchy
fn main(console: Console):
    let n = 5
    if n > 10:
        console.print("big")
    else if n > 3:
        console.print("medium")
    else:
        console.print("small")

    var total = 0
    for x in [1, 2, 3, 4]:
        if x == 2:
            continue
        if x > 3:
            break
        total = total + x
    console.print("${total}")

    var i = 0
    while i < 3:
        i = i + 1
    console.print("${i}")
```

`for var x in xs:` binds each element of a list variable **mutably** and writes it
back, so you update elements in place without index bookkeeping (the loop form of
mutable value semantics) — a mutation of `x` lands in `xs`, in place when the
uniqueness analysis proves it unaliased. The v1 form takes a single loop variable
over a plain list variable and rejects a `break`/`continue`/`return`/`?` that
belongs to the loop (it would skip the write-back); a plain `for x in xs:` binds
each element read-only.

```witchy
fn main(console: Console):
    var xs = [1, 2, 3, 4]
    for var x in xs:
        x = x * 10
    console.print("${xs}") // [10, 20, 30, 40]
```

`if let PAT = e:` binds and runs only on a match (with an optional `else`);
`while let PAT = e:` loops as long as the scrutinee keeps matching. `return e`
exits early, and works in functions with a `var` parameter (the written-back
parameters are still delivered). `return e if cond` is a postfix form of
`if cond: return e`, for one-line early returns like `return Ok(true) if ok`.

To deny a capability to a region of code, give that work its own function and
don't pass the capability — a function that never receives a capability cannot
use it, alias it, or forge it. That structural boundary (capture-as-DI) is
witchy's firewall; see `spec/capabilities.md`.

A `region:` block (optionally `region -> T:`) is a user-controlled allocation
scope: everything allocated inside is reclaimed at the block's end, and the
block's VALUE is what escapes — on the compiled backend it is deep-copied out,
except sub-values from outside the region, which are shared rather than
copied. Assigning a non-scalar variable declared outside the region is a type
error (the value is the only pointer escape; scalar assignments are fine), and
`yield` is rejected. A region never changes observable behavior — only when
memory is reclaimed — so the interpreter runs it as a plain block. The
optional `-> T` ascribes the value's type, guaranteeing the copy-out shape
when inference cannot see it. See `rfcs/regions.md`.

```witchy
import option

fn first_even(xs: List(Int)) -> Option(Int):
    for x in xs:
        if x % 2 == 0:
            return Some(x)
    None

fn main(console: Console):
    if let Some(v) = first_even([1, 3, 4, 5]):
        console.print("first even: ${v}")
    else:
        console.print("none")
```

## 6. Pattern matching

There is **one pattern grammar**, used in every binding position — `match` arms,
`if let` / `while let`, `let`, `for`, and comprehensions. A pattern is one of:

| Pattern | Example |
|---|---|
| wildcard | `_` |
| variable | `x` (binds the value) |
| literal | `0`, `-1`, `"hi"`, `true`, `1s` (a Duration) |
| integer range | `0..10` (half-open), `0..=10` (inclusive) |
| tuple | `(a, b)`, nested `((a, b), c)` |
| constructor | `Circle(r)`, `Some(x)`, a record `Point(x, y)` |
| list shape | `[]`, `[a, b]`, `[first, ..rest]`, `[a, ..]` |
| or-pattern | `1 \| 2 \| 3`, and nested `Some(1 \| 2)`, at any depth |

Or-patterns and ranges are ordinary sub-patterns: they nest anywhere a pattern
is allowed (`Some(1 | 2)`, `(0..10, _)`, `[1 | 2, ..rest]`). Every alternative of
an or-pattern must bind the **same names at the same types** (checked). `Float`
literals **cannot** be matched — exact float equality is a precision trap
(bind and guard instead: `x if math.float_abs(x - 1.5) < eps ->`); a `Float`
*scrutinee* bound to a variable is fine (`match f: x -> …`). `Duration` literals
**can** be matched — a Duration is an exact millisecond count, and `-1s` is a
negative duration literal in both expression and pattern position.

**Contexts differ only by refutability.** `match` / `if let` / `while let`
accept any pattern. `let` / `for` / comprehensions require an **irrefutable**
pattern — one the checker proves always matches: `_`, a variable, a tuple of
irrefutable patterns (any nesting), and a single-variant constructor/record
whose fields are irrefutable. A refutable pattern there (a literal, a range, an
or-pattern, a list shape, or a multi-variant constructor) is a check-time error
pointing at `if let`:

```sh
let Circle(r) = shape
# error: `let Circle(r) = …` — `Circle` is one of 2 variants of `Shape`, so
#        this pattern can fail. Use `if let Circle(r) = …:` (with an else), or `match`.
```

`match` is exhaustiveness-checked (missing variants are named in the error) and
unreachable arms are rejected; an or-pattern covers the union of its
alternatives, while a range is treated as refutable (a range-only match still
needs a final `_`/binding arm — witchy does no numeric-coverage analysis).
Guards (`PAT if cond ->`) don't count toward exhaustiveness. An arm body is an
expression, a single inline statement (`0 -> return Err("zero")`,
`Some(v) -> total = total + v`, `_ -> break`), or an indented block of
statements on the lines after the `->`.

```witchy
type Shape:
    Circle(Int)
    Square(Int)

fn describe(s: Shape) -> String:
    match s:
        Circle(r) if r > 100 -> "big circle"
        Circle(r) | Square(r) -> "small " + "${r}"

fn size(n: Int) -> String:
    match n:
        0 -> "none"
        1..10 -> "few"
        10..=100 -> "many"
        _ -> "lots"

fn head(xs: List(Int)) -> String:
    match xs:
        [] -> "empty"
        [first, ..rest] -> "first " + "${first}" + ", " + "${list.length(rest)}" + " more"

fn main(console: Console):
    console.print(describe(Circle(2)))
    console.print(size(5))
    console.print(size(50))
    console.print(head([10, 20, 30]))
    // Irrefutable destructuring shares the same grammar:
    let ((a, b), c) = ((1, 2), 3)
    console.print("${a} ${b} ${c}")
    for k, v in [(1, 2), (3, 4)]:
        console.print("${k}=${v}")
```

## 7. Functions

```witchy
type Shape:
    Circle(Int)
    Square(Int)

pub fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    console.print("${area(Circle(2))}")
    console.print("${area(Square(3))}")
```

Parameter annotations are required; the return type may be inferred for
non-`pub` functions. Locals are inferred (Hindley-Milner-style unification with
an occurs check).

**Parameter conventions** (Hylo-style value semantics):

| Convention | Meaning |
|---|---|
| (default) | owned, observably immutable value — the callee may read but the caller sees no change |
| `let` | immutable **borrow**; may not escape — returning a `let`-borrowed parameter is a type error |
| `var` | a write-back parameter, in one of two shapes by the return type: a **procedure channel** (returns `Nil`) mutates the parameter and writes it back to the caller's variable — even on early `return`/`?`; a **mutator receiver** (first parameter, returns that parameter's type — `fn push(var xs: List(a), …) -> List(a)`) declares that the function's statement form writes back (§3), while its expression form is an ordinary value call |
| `own` | ownership transfer; the **callee** consumes the argument, so using the source afterwards is a check-time error |
| `move e` | use-site ownership transfer; the **caller** consumes the source binding (see below), idiomatically paired with `own` |

```witchy
fn bump(var n: Int):
    n = n + 1

fn main(console: Console):
    var counter = 41
    bump(counter)
    console.print("${counter}")
```

`bump` above is a **procedure channel**: a `var` parameter and a `Nil` return, so
the call site must pass a mutable `var` (`bump(counter)`) and the mutation is
written back through the parameter. A `var` **first** parameter whose type the
function *returns* is instead a **mutator receiver** (`fn push(var xs: List(a),
x: a) -> List(a)`): its expression form is a plain value call that accepts any
receiver argument, and it is its *statement* form that writes back (§3). The two
readings are disjoint by return shape, so a reader never guesses which applies.

`own` and `move` are two independent ways to end a binding's life, meeting in the
middle. `own` consumes from the **callee** side: passing any variable to an `own`
parameter marks it moved, so a later use is a check-time error
(*use of `x` after it was moved*). `move x` consumes from the **caller** side: it
ends `x` *whatever the callee's convention is* — into a default, `let`, or `own`
parameter alike — so a later use of `x` is the same check-time error even when the
parameter only took an ordinary copy. The two compose: `f(move x)` into an `own`
parameter is a hand-off both sides spell out, and on the compiled backend it is a
guaranteed no-copy move. `move` is **not** accepted into a **procedure-channel**
`var` parameter — that argument must be a plain mutable variable, since the
callee writes it back (a mutator receiver's expression form is an ordinary value
call, so it accepts `move` like any value parameter). On both backends `move` is
value-neutral (value semantics copy already); it changes only *when* a copy is
elided, never any result.

**Closures.** `fn(n: Int): n + by` captures by value; you call through a
`fn(...)` -typed value or parameter. Closures cannot assign to captured
variables (check-time error). A closure may declare its return type —
`fn(n: Int) -> Bool: n > 0` — which also makes it a `?` boundary: a `?` inside
the closure propagates to the closure's own `Result`/`Option`, not the enclosing
function's, so closures can short-circuit on errors just like named functions.

```witchy
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn adder(by: Int) -> fn(Int) -> Int:
    fn(n: Int): n + by

fn main(console: Console):
    let add10 = adder(10)
    console.print("${apply(add10, 5)}")
```

**Keyword arguments and default parameters.** A direct call to a free or
module-qualified function may pass arguments by parameter **name**: a positional
prefix followed by labeled arguments (`connect(host: "x", port: 443)`). Labels
bind to the declaration's parameter names and may appear in any order, but every
argument still evaluates in **source order** (left to right as written, not in
parameter order). A suffix parameter may declare a **closed-constant default**
(`port: Int = 443` — a literal or other compile-time-constant expression); a call
that omits it splices the default in. Defaults live at the declaration site: they
do **not** attach to a function *value*, and labels and defaults are erased before
either backend runs, so they cost nothing at runtime. Two limits hold: method
calls and calls through a function value are positional-only (so
`string.substring(s, start: 1, end: 3)` accepts labels but `s.substring(start: 1)`
does not), and a `var` (write-back) parameter cannot have a default.

```witchy
fn connect(host: String, port: Int = 443, tls: Bool = true) -> String:
    let scheme = if tls: "tls" else: "tcp"
    "${scheme}://${host}:${port}"

fn main(console: Console):
    console.print(connect(port: 8080, host: "localhost", tls: false))
    console.print(connect("example.com"))
    console.print(connect("db.internal", port: 5432))
```

**Where a mutation reaches.** A `var` is a *local* mutable binding, nothing more.
witchy has value semantics: every boundary that carries a value out of a scope —
a default call argument, a closure capture, a task message — carries a **copy**,
so a mutation is never observed through that copy and there is no shared mutable
state to reason about. The one mechanism that writes back to a caller is a `var`
*parameter* (above), and even that is a single handed-over variable with no
aliasing. Concretely:

- A **closure** captures by value and so cannot assign to a captured variable
  (check-time error); produce the new value and return it, or take a `var`
  parameter.
- **`await`** lowers an `async fn` into state-machine segments (§14). Live locals
  are threaded through segment parameters, so a `var` declared before an
  `await` may be mutated after it when the await appears in a supported
  position. `await` is supported in loop bodies, including `while` bodies and
  `for await` folds; it remains unsupported in branch conditions, loop
  conditions, and match scrutinees.
- A **`gen fn`** is the exception: a `var` *may* be freely mutated across a
  `yield` (§11), because a generator re-runs its body to the next yield rather
  than capturing a continuation.

**Ownership/immutability qualifiers** (`frozen`, `unique`, `local unique`) are
compile-time *contracts* on a type — distinct from the calling conventions above,
they live on the type and propagate through it. They have no runtime
representation (both backends lower `frozen T`/`unique T` to `T`), so they never
change observable behavior; they only let the checker enforce, and a library
*promise*, an ownership fact:

| qualifier | meaning |
|---|---|
| `frozen T` | deeply immutable — sharing is safe; declaring it mutable (`var`/`own`) is a check-time error |
| `unique T` | the sole reference — may be mutated in place and returned as `unique` |
| `local unique T` | unique within this call only — may be mutated but **may not escape** (returning it is a check-time error) |

```witchy
import show

fn total(xs: frozen List(Int)) -> Int:
    var sum = 0
    for x in xs:
        sum = sum + x
    sum

fn main(console: Console):
    let table: frozen List(Int) = [10, 20, 30]
    show.say(console, "${total(table)}")
```

These restate, as enforced contracts, guarantees witchy's value semantics already
provide (a shared value is never mutated in place; the uniqueness pass reuses
buffers it proves unaliased), so they carry no separate performance cost or
benefit — see [performance.md](performance.md).

## 8. Generics and traits

```witchy
import cmp

fn largest(xs: List(a)) -> a where a: Ord:
    var best = xs.at(0)
    for x in xs:
        if x > best:
            best = x
    best

fn main(console: Console):
    console.print("${largest([3, 9, 2, 7])}")
    console.print(largest(["apple", "pear", "fig"]))
```

Generic functions are checked once and monomorphized per concrete use for the
compiled backends (both `where`-bounded and unbounded generics). `Self` in an
impl refers to the implementing type. Trait method calls inside a
`where a: Trait` function resolve on any expression whose type the checker
knows — parameters, loop variables, constructor-pattern bindings, destructured
tuple slots, and the results of calls — so an intermediate expression rarely
needs a `let` binding to dispatch.

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
    console.print(Dog.greet())
```

The std comparison hierarchy `PartialEq` → `Eq` → `PartialOrd` → `Ord` (in
`import cmp`, mirroring Rust's `std::cmp`) backs the `== != < > <= >=` operators
and provides scalar helpers such as `cmp.max_of`, `cmp.min_of`, and `cmp.clamp`;
bounded collection algorithms live in `list` (`list.contains`, `list.index_of`,
`list.sort`, ...). The prelude `Show` protocol renders.

**`impl Trait` arguments.** When a parameter is generic only so it can carry a
trait bound, `impl Trait` says so directly — `x: impl Loud` is sugar for a fresh
type variable plus a `where` bound:

```witchy
trait Loud:
    fn shout(self) -> String

type Dog:
    Dog

impl Loud for Dog:
    fn shout(self) -> String:
        "WOOF"

fn announce(console: Console, x: impl Loud):    // == `x: a` ... `where a: Loud`
    console.print(shout(x))

fn main(console: Console):
    announce(console, Dog)
```

Each `impl Trait` parameter introduces its own type variable, so two of them are
two independent types. It is argument-position only (not a return type), and it
composes with an explicit `where` clause. The std library uses it for
`show.say(console, x: impl Show)` — a `Show`-accepting `print`. The `show`
module is preluded; `from show import say` is needed only when you want the bare
`say(...)` spelling.

### Deriving the standard traits

`derive(...)` generates trait impls for a type. The generated code is appended to
the module before type-checking, so both backends and the footprint analysis treat
it like handwritten code. The supported derives are `Show`, `PartialEq`, `Eq`,
`PartialOrd`, `Ord`, `Reflect`, and `Deserialize`. `Reflect` needs
`import reflect` and makes a user type reflectable (scalars and the built-in
containers already are); it is what lets
`json.stringify` / `json.from_value` encode the type with no per-type code.
`Deserialize` generates `from_json(j) -> Result(Self, String)` for scalars,
lists, options, and nested records, and — because the generated body names them
like handwritten code — needs `import json`. `Result`/`Ok`/`Err` and
`Option`/`Some`/`None` are prelude names, so generated deserialize code can use
them without redundant imports. There is no `Serialize` derive,
because reflection already encodes any value (`json.from_value`, `json.stringify`,
`Into(Json)`); only decoding has to be generated per type.

```witchy
import show
import cmp

type Point derive(Show, PartialEq, Eq, PartialOrd, Ord):
    x: Int
    y: Int

fn main(console: Console):
    show.say(console, Point(1, 2))
    console.print("${Point(1, 2) < Point(1, 3)}")
```

`Show` renders a value as a string; derived `Show` is structural, and
interpolation uses it for values with a relevant impl once `show` is linked.
`PartialEq`/`Eq` are structural equality (backing `==`/`!=`), and
`PartialOrd`/`Ord` compare record fields in order (records only) and back
`<` `>` `<=` `>=`.
Derives also work on a generic type. `type Box(a) derive(Reflect)` generates an impl
that carries the type parameters and their bounds and specializes per type argument.

User-defined derives are local compile-time generator functions named
`derive_<lowercase-name>`. The legacy form returns source text, while a
`comptime fn` generator may return `ItemSyntax` or `List(ItemSyntax)` to append
typed generated items through the RFC-0080 channel. The generated declarations
are still ordinary module items after expansion; they must type-check and pass
footprint analysis like handwritten code.

### Reflection and anonymous structs

`reflect(x)` returns a value's structure as a `Mirror`, which lets one function
handle a value of any type. `List`, `Option`, tuples through arity 8, and generic records implement
`Reflect` through ordinary generic impls, so `json.stringify(x)` and
`reflect.debug(x)` work on a list, an option, a supported tuple, or a nested record without a
per-type impl.

An anonymous struct, `.{ field: expr, ... }`, is a record with no declared type. It
reflects like any record, so you can build JSON from plain values without declaring
a type or constructing `Json` by hand:

```witchy
import json
import reflect

fn main(console: Console):
    let files = [("a.txt", "hi")]
    console.print(json.stringify(.{files: files}))
```

### Conversion: `From` and `Into`

`std/convert` provides `From` and `Into`, following Rust. Implementing `From` for a
type also gives it `Into`:

```witchy
import convert

type Celsius:
    deg: Int

impl From(Int) for Celsius:
    fn from(value: Int) -> Celsius:
        Celsius(value)

fn main(console: Console):
    let c: Celsius = 5.into()
    console.print("${c.deg}")
```

A single blanket impl, `impl Into(b) for a where b: From(a)`, supplies the `into` for
every `From`. Its body calls the static `from` on the target type, `b.from(self)`,
which the `where` bound resolves when the call is monomorphized. `std/json` adds
`impl From(a) for Json where a: Reflect`, so any reflectable value converts to JSON
with `x.into()` or `Json.from(x)`, and `server.send(code, value)` encodes a response
of any reflectable type.

### `comptime:` — compile-time item generation

A top-level `comptime:` block runs **at compile time** with no capabilities
reachable (there is no parameter list to receive one), making it
deterministic by construction. Legacy `emit(line)` output, and direct
`console.print(line)` for compatibility, are parsed as witchy source and
**appended** to the module before type checking and footprint analysis.
`emit_item(item)` is the typed RFC-0080 migration channel for
`meta.ItemSyntax`. A single `comptime:` block may use the legacy source
channel or the typed item channel, but not both.
Compiler syntax values such as `meta.ItemSyntax`, `meta.TypeSyntax`,
`meta.ExprSyntax`, `meta.PatternSyntax`, `meta.StmtSyntax`, `meta.BlockSyntax`,
`meta.MatchArmSyntax`, and `meta.Ident` are compile-time-only: runtime
functions, fields, aliases, and expressions cannot store or return them.
Top-level `comptime fn` declarations are helpers for this expansion phase. They
may mention compile-time-only syntax types, may be called from `comptime:`,
custom-derive, or tagged-literal expansion, and are stripped before the runtime
module is linked and type-checked. Runtime code cannot call them.
`std/meta` also exposes source-backed syntax builders such as `ident`,
`type_named`, `expr_call`, `pattern_anon_ctor`, `match_arm`, `stmt_let`,
`block`, `param`, and `function_block`; they make generated item structure
typed at the API boundary and validate identifier spelling while full quotation
and hygienic identifier origins remain future work.
`quote expr:`, `quote type:`, `quote pattern:`, `quote stmt:`, `quote block:`,
and `quote item:` are the first quotation forms. They parse the indented
expression, type, pattern, statement, block, or single item immediately and
produce `meta.ExprSyntax`, `meta.TypeSyntax`, `meta.PatternSyntax`,
`meta.StmtSyntax`, `meta.BlockSyntax`, or `meta.ItemSyntax` through the same
sealed source-backed channel as the `std/meta` builders. Inside `quote expr:`,
`${hole}` splices a `meta.ExprSyntax`; inside `quote type:` and
`quote pattern:`, `${hole}` splices a `meta.TypeSyntax` or `meta.PatternSyntax`;
inside `quote stmt:` and `quote block:`, `${hole}` splices a `meta.ExprSyntax`
in expression positions. Holes are typed by the surrounding `comptime`/tag
generator, not by runtime interpolation. Type and pattern holes inside
statement/block quotation remain future work.
`quote type:` covers named/generic, module-qualified, tuple, function,
ownership-qualified, and capability-right types; anonymous structural type
quotation and hygiene remain future work.

Generated code is analyzed exactly like handwritten code, and nothing existing
can be rewritten, so a comptime block cannot launder authority out of a
signature.

```witchy
comptime:
    var i = 0
    while i < 3:
        emit("pub fn lucky_${i}() -> Int:")
        emit("    ${i * 7}")
        emit("")
        i = i + 1

fn main(console: Console):
    console.print("${lucky_0()} ${lucky_1()} ${lucky_2()}")
```

### Tagged literals — compile-time `tag"…"`

A string literal written **immediately after an identifier**, `tag"a${x}b"`, is a
*tagged literal*. It is expanded **at compile time**, like `comptime:`, but in
**expression** position: the lexer splits the literal into its static fragments
and its `${…}` hole sources, and the compiler calls the `tag` function

```text
fn tag(parts: List(String), holes: List(String)) -> String
```

or the typed RFC-0080 form

```text
comptime fn tag(parts: List(String), holes: List(String)) -> meta.ExprSyntax
```

with `parts` = the static fragments and `holes` = an **opaque marker** per hole —
a token the tag *places* where that hole's value belongs (the tag does not read
the hole's source). A legacy string tag returns witchy **expression source**; a
typed tag returns `meta.ExprSyntax`, whose source payload remains sealed behind
the compiler boundary. The compiler parses that generated expression and
**substitutes** the real hole expression — parsed once at the call site, carrying
its source position — at each marker, then splices the result over the literal
before type checking. So both backends compile the same AST, the tag runs once in
the compiler, and a hole's marker may be placed zero, once, or many times. The tag
is local or imported; only the items reachable from it run at expansion time.

Because a tag emits *code*, interpolation holes are typed **by position** (the
substituted expression is type-checked normally) and there is no runtime string
parser. Hole expressions resolve at the **call site** (hygiene), and a type error
in a hole points back **into the literal** at that `${…}`, not at generated code.
The `html` tag in the `glamour` rune uses this: a `${userInput}` in text position
becomes a `text(…)` **node**, never markup, so it is XSS-immune by construction.

```witchy
import list

// A tag receives the static parts and an opaque MARKER per hole; it places each
// marker where the hole's value goes, then returns witchy expression source. The
// compiler substitutes the real hole expression (here `name`, resolved at the
// call site) at the marker.
fn greet(parts: List(String), holes: List(String)) -> String:
    "\"Hello, \" + " + holes.at(0)

fn main(console: Console):
    let name = "witch"
    console.print(greet"hi ${name}")
```

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
    let first = checked_div(a, b)?
    checked_div(first, c)

fn show(r: Result(Int, String)) -> String:
    match r:
        Ok(v) -> "ok: " + "${v}"
        Err(e) -> "err: " + e

fn main(console: Console):
    console.print(show(ratio(100, 5, 2)))
    console.print(show(ratio(100, 0, 2)))
```

Bare `e?` propagates `Option(T)` or `Result(T, e)` unchanged. The contextual
form `e? "msg"` is the string-error convenience: it accepts `Option(T)` or
`Result(T, String)` and yields `Result(T, String)`. On a `Result`, it prepends
`"msg: "` to a propagated `String` error; on an `Option`, a propagated `None`
becomes `Err("msg")`. The enclosing function therefore propagates a `String`
error. The message may interpolate. Richer typed-error context wrapping is
tracked by RFC-0054.

For typed errors, use either a named error enum or a local anonymous union.
Named enums are the contract surface for libraries and packages: they can
derive or implement protocols and can absorb lower-level errors through
`From`. Anonymous unions are the local structural tier: `Result(T, .[A | B])`
can propagate through `?` into `Result(U, .[A | B | C])` with no wrapper.
The context form `e? "msg"` intentionally stays a `String`-error tool; on a
union-error `Result`, match and rewrap or add an explicit payload tag such as
`.Context(String)`.

```witchy
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn ratio(a: Int, b: Int) -> Result(Int, String):
    let q = checked_div(a, b) ? "computing ${a}/${b}"
    Ok(q + 1)

fn main(console: Console):
    match ratio(10, 0):
        Ok(v) -> console.print("${v}")
        Err(e) -> console.print(e)
```

Unexpected failure is **loud on every backend**: out-of-bounds indexing,
division by zero, unparseable `string.to_int`, NaN ordering, and the `fail(msg)`
primitive all abort (a runtime error interpreted, a trap compiled). The parity
invariant covers these too — a program that errors on one backend errors on
both.

**Unwrapping with `??`.** For a quick value-or-default, `Option(T) ?? T` unwraps
to a bare `T` (§4): `Some(x) ?? d` is `x`, `None ?? d` is `d` (with `d` evaluated
only when absent). `Result(T, e) ?? T` unwraps `Ok` likewise, discarding the
error. It is `unwrap_or` with operator syntax — handy on the `Option`-returning
lookups (`dict.get`, `list.head`, …).

```witchy
fn main(console: Console):
    let ages = dict.new().insert("ada", 36)
    console.print("${dict.get(ages, "ada") ?? 0}")
    console.print("${dict.get(ages, "bob") ?? 0}")
```

## 10. Comprehensions

`[elem for x in iter]`, optionally filtered with `if cond`, builds a list:

```witchy
import list
import string

fn show(xs: List(Int)) -> String:
    xs.map(fn(n: Int): "${n}").join(" ")

fn main(console: Console):
    let squares = [n * n for n in 1..6]
    console.print(show(squares))
    let evens = [n for n in 1..11 if n % 2 == 0]
    console.print(show(evens))
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
    console.print(list.join(list.map(first8, fn(n: Int): "${n}"), " "))

// 0 1 1 2 3 5 8 13
```

## 12. Modules and the standard library

```witchy
import list
import string

fn main(console: Console):
    let shouted = ["a", "b", "c"].map(fn(s: String): s.to_upper())
    console.print(shouted.join("-")) // A-B-C
```

The core data modules — `list`, `string`, `dict`, `math`, `option`, `result`,
`policy`, and `show` — are **the prelude**: always available, no import line needed
(`list.push(xs, 1)` works anywhere). Pure data operations live ONLY in
modules; the global namespace is capability operations (`print`, `read`,
`send`, `now`, …) and `fail` — authority is loud and unprefixed, everything
else says where it came from. (Rendering needs no function at all: `${...}`
interpolation is the rendering.) For other modules,
`import name` brings the module in under its name; **function** calls are
module-qualified (`list.map(xs, f)`) — or, for a built-in type's own operations,
the equivalent method form (`xs.map(f)`, see §4), which is the idiom for the data
libraries. A module's `pub` **types and their constructors** are module-scoped
the same way: after `import json` you name them qualified (`json.Json`,
`json.JsonInt(1)`, `json.JsonObject([...])`). To use a type and its constructors
*unqualified*, name it explicitly with `from json import Json` — a from-imported
type brings its variant constructors into scope bare, so `JsonInt(1)` and
`JsonObject([...])` then work directly. (In a `match` whose scrutinee type is
known, bare variant names always resolve against that type, so match arms need
no qualifier either.) Two unqualified bindings of the same name collide at the
import line, not at first use. Bundled standard-library module names are
reserved and always resolve to their canonical compiler-shipped source; a local
module must use another name. Other imports resolve a sibling `name.witchy`
file or a package dependency. `pub` items are importable; everything else is
module-private. Package dependencies ("runes")
come from the manifest — see [package-manager.md](../rfcs/package-manager.md).

## 13. Entry point

The program's root authority. `main` may take any number of **capability**
parameters plus an optional `args: List(String)` (the command-line arguments),
and may return `Nil` or `Int` (the process exit code):

```witchy
fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:
    console.print("running with ${args.length()} arg(s)")
    0
```

The host mints exactly these capabilities and nothing else. (This block
type-checks but isn't run by the doc harness, since it needs `Dir` — run it
with `witchy sandbox --dir <root> prog.witchy a b c`.)

`main` may ask for any of the host capabilities — `Console`, `Clock`, `Env`,
`Dir[...]`, `File[...]`, `Net[...]`, `Exec`, `SecretStore` — and the launch grant
backs each: `--dir <root>` a `Dir`, `--file <path>` a `File` (the i-th `File`
parameter ← the i-th `--file`), `--net <host:port>` a `Net` allowlist entry,
`--secret`/`--signing-key` a `SecretStore`. A `File[Read]` lets a single-file
program ask for exactly one file instead of a whole `Dir`. A **grant document**
(`--grants app.grants.toml`) enumerates the whole grant as reviewable TOML and is
cross-checked against the computed footprint — see
[capabilities.md](capabilities.md) and
[0013-capability-grant-documents.md](../rfcs/0013-capability-grant-documents.md).

### 13.1 The build entrypoint

A rune may ship a **build step**: a top-level `fn build` whose first parameter is
a build capability. It is the root of *build-time* authority, exactly as `main`
is the root of runtime authority — and the two capability sets never mix: `build`
may take **only** build capabilities, and `main` may take none of them.

```witchy
fn build(out: BuildOut, schema: BuildRead, cc: BuildExec):
    let proto = schema.read_build("api.proto")
    out.write_out("api.witchy", cc.run_tool("protoc", proto))
```

| Capability | Grants | Operations |
|---|---|---|
| `BuildOut` | write generated source into this rune's confined output sandbox (needs no naming once the consumer accepts the build step — execution itself is default-deny) | `out.write_out(name, contents)` |
| `BuildRead` | read project files, confined to a granted subtree | `r.read_build(name) -> String` |
| `BuildEnv` | read env vars — only keys *named* in the grant, never the whole environment | `e.get_build_env(key) -> Option(String)` |
| `BuildNet` | HTTP-fetch from hosts on an allow-list (`host:port`, exact) | `n.fetch_build(host, path) -> String` |
| `BuildExec` | invoke a *named* external tool on an allow-list | `x.run_tool(tool, stdin) -> String` |

The types are kind-only — the specific directory/key/host/tool is the consuming
project's *grant*, not the type. `witchy caps` reports the build footprint on its
own axis, `witchy caps-diff` fails on a build-axis widening, and
`witchy build-step <file> [--out <dir>] [--read <dir>] [--env K]... [--exec tool]...`
runs a build step under those confined grants. See
[build-time-execution-plan.md](../rfcs/build-time-execution-plan.md) for status and
[package-manager.md](../rfcs/package-manager.md) §7.1 for the full model.

### 13.2 User-definable capabilities

A library declares its own capability by **refining** the host's, with
`capability X from U`. `X` is a *sealed brand*: a single-variant wrapper over the
underlying capability `U` (or several — `from (A, B)`), with one rule — `X` may be
**constructed or destructured only inside the module that declares it**. Any other
module may hold, pass, and return a value of `X`, but cannot mint or unwrap one, so
`X` is un-forgeable exactly like a host capability.

```witchy
capability Redis from Net[Connect, Tcp]

// The ONLY way to obtain a `Redis` — its constructor is sealed to this module.
pub fn open(net: Net[Connect, Tcp]) -> Redis:
    Redis(net)

pub fn ping(r: Redis) -> Int:
    match r:
        Redis(net) -> 1
```

- **Minting consumes authority.** A `Redis` can only be made by handing a real
  `Net` to `open`; a library can never conjure authority from nothing.
- **Attenuation is by facet** — declare a narrower capability refining the first
  (`capability ReadOnly from Postgres`) that exposes fewer operations; ordinary
  type-checking enforces it.
- **The footprint sees through.** `witchy caps` reports a user capability as the
  host authority it refines — `ping` audits as `Net[Connect, Tcp] (refined: Redis)`
  — so a library cannot launder `Net` behind a friendly name.

A second form lets a capability **carry state beside** the authority it wraps — a
sealed *record* mixing one or more host capabilities with ordinary policy data:

```witchy
capability Postgres:
    net: Net[Connect, Tcp]
    table: String

// Sealed constructor — only this module can mint, refine, or destructure one.
pub fn open_db(net: Net[Connect, Tcp], table: String) -> Postgres:
    Postgres(net, table)

pub fn scope(p: Postgres) -> String:
    match p:
        Postgres(_, table) -> table
```

The fields are private — reached with `match`, never `.field` — so the underlying
`Net` can never leak past the policy. `witchy caps` sums the record's
capability-typed fields, so `Postgres` audits as exactly `Net`: carried policy with
nothing hidden (the hard, runtime-enforced `Net` plus a soft, library-enforced
`table` filter, in one unforgeable value). See
[0002-user-definable-capabilities.md](../rfcs/0002-user-definable-capabilities.md).

### 13.3 Narrowing a `Net`'s reach: `only` / `deny`

Rights (§13.1) narrow which *verbs* a `Net` permits; to narrow which *hosts* it may
dial, confine its **address-set** with typed policy values built on the capability
itself (`Net.tcp(…)`).
`net.only(policy)` intersects the carried set with `policy` (an endpoint survives
only if already admitted); `net.deny(policy)` subtracts a slice. Both are monotone —
refinement only ever shrinks — and host-enforced **at the syscall** on both
backends, the address analog of `dir.subtree` for `Dir`.

```witchy
fn main(console: Console, net: Net):
    let db = net.only(Net.tcp("10.0.0.5", 6379))
    let safe = net.deny(Net.cidr_any("10.0.0.0/8")).only(Net.tcp("192.168.1.1", 80))
    console.print("net confined")
```

The policy constructors are `Net.tcp(host, port)`, `Net.any_port(host)`,
`Net.cidr(block, port)`, `Net.cidr_any(block)`, `Net.union(a, b)`, and `Net.private()` — the
internal IP ranges (loopback, RFC-1918, link-local incl. the `169.254.169.254`
metadata IP, CGNAT) for the one-line SSRF/rebinding guard
`net.deny(Net.private())`. A CIDR/IP policy is
checked against the *resolved* IP, so it is rebinding-safe. TLS is not a right or a
policy scheme but a connect-time `tls:` prefix on the address you dial
(`net.connect("tls:host:443")`); see
[0003-network-address-scoping.md](../rfcs/0003-network-address-scoping.md) and
[0009-https-tls-client.md](../rfcs/0009-https-tls-client.md).

A `Dir` likewise carries an **entry policy** narrowing which entries it may touch:
`dir.only(Dir.ext(".txt"))` confines it so `read`/`write`/`open` admit only
matching files (a non-matching name is refused at the access check; a subtree
inherits the policy) — the filesystem analog of `net.only`. See
[0011-capability-refinement.md](../rfcs/0011-capability-refinement.md).

## 14. Concurrency: async, spawn, and channels

Concurrency is **cooperative async tasks** that communicate over **channels**. A
function marked `async` may `await`; calling it yields a task that does nothing
until driven. `chan.spawn` starts a task concurrently, and a `chan.channel` is a
first-class value you create and pass to whichever tasks share it — spawning and
channels are *independent* (no task has an implicit mailbox). Tasks share no
memory, so there are no locks or data races, and the round-robin schedule is
deterministic — identical output on the interpreter and the compiled WebAssembly.

```witchy
from chan import Sender

async fn producer(tx: Sender(Int)) -> Nil:
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.spawn(producer(tx)).await
    chan.consume(rx, fn(n): chan.done(console.print("got ${n}"))).await
```

`chan.channel(cap)` is a bounded channel — the sender blocks when it is full
while the executor can make progress; pass `0`, or use `chan.unbounded()`, for no
backpressure. If every live task parks with no progress, the executor runs its
quiescence close pass: parked receives/selects resume as `None`, parked sends are
released, and parked joins resume. That is the close condition
`chan.recv(rx).await` and `chan.consume` observe; witchy does not refcount sender
values, so "closed" does not mean no `Sender` value can ever be used again. A
channel can be shared by many receivers (a worker pool) or many senders. Each
channel is typed independently — a program may use channels of many different
message types (the executor carries messages erased and each endpoint recovers
its own type). A spawned task returns `Nil`, reporting results over a channel.
See the book's *Concurrency* chapter and `std/chan` for the full model.

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
- Equality goes through `PartialEq` at every depth: the derived/default impl is
  deep structural equality (`Dict` insertion-order-sensitive), and a custom impl
  is honored inside containers too. `==`/`!=` on function or capability types is a
  compile-time error (no meaningful, stable equality — this replaces a former
  backend divergence). `Dict` keys and `Set` members require `Eq`, so a `Float`
  key/member is a compile-time error (closing the NaN-key hole).
- String operations are byte-precise across backends; `trim`/case-mapping are
  ASCII-scoped by design.
- Out-of-bounds, overflow-on-parse, and `fail` abort on every backend.
- Capability confinement (`Dir` path resolution, `Net` allowlists) uses the
  same rules in the interpreter and the sandbox.

Anything a backend cannot run identically is a **loud compile or runtime error,
never a silently different answer**.
