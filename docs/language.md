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
| `3.5`, `0.5` | `Float` | IEEE-754 double; `${...}` renders the shortest round-trip form |
| `true` / `false` | `Bool` | |
| `"hi\n"` | `String` | UTF-8; escapes `\n \t \r \0 \\ \" \$` |
| `"sum: ${a + b}"` | `String` | interpolation — `${expr}` renders *any* value (see below); inner strings may be bare (`"${f("x")}"`) or escaped (`"${f(\"x\")}"`) |
| `30s`, `250ms`, `5m`, `2h`/`2hr`, `1d`, `1w` | `Duration` | a distinct type carried as milliseconds; not mixable with bare `Int`; `${d}` prints the raw milliseconds — `duration.human`/`clock` (or `say`) for display |
| `[1, 2, 3]` | `List(Int)` | immutable |
| `(1, "a")` | tuple | fixed arity, mixed types; elements read by position (`pair.0`, `grid.0.1`) or destructured (`let (n, s) = pair`) |

```witchy
fn main(console: Console):
    let a = 6
    let b = 7
    print(console, "sum: ${a + b}")          // string interpolation
    print(console, "${1500ms < 2s}")         // durations are a distinct type
    let pair = (1, "a")                       // a tuple
    let (n, s) = pair
    print(console, "${n}${s}")
```

**Rendering values to strings.** Reach for interpolation first: `"${x}"` renders
*any* value — scalars, record fields, lists, tuples, records, sum types, dicts,
and any nesting — identically on both backends. You rarely need to call
a conversion by hand. To print
one value, `print(console, "${x}")`, or `say(console, x)` — the `Show`-accepting
`print` from `import show`, for any `Show` value (the built-in scalars and your
own types). The **`Show` trait** (`fn show(self) -> String`) is the trait-method
route: implement it to give a type a *custom* rendering (interpolation already
gives every value a structural default like `Point(1, 2)`).

## 2. Types

Builtins: `Int`, `Float`, `Bool`, `String`, `Duration`, `Nil` (the unit type),
`List(a)`, `Dict(k, v)`, tuples `(a, b, ...)`, function types
`fn(Int, String) -> Bool`, and the capability types (`Console`, `Clock`, `Env`,
`Dir[...]`, `Net[...]`, `Secret` — see [capabilities.md](capabilities.md)).

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
    print(console, "${Red == Red}")
    let acc = Account("ada", 100)
    let named = Account(name: "bob", balance: 5)
    print(console, acc.name)
    let richer = Account(balance: acc.balance + 1, ..acc)
    print(console, "${richer.balance}")
    print(console, "${named.balance}")
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
    (list.at(xs, 0), list.at(xs, list.length(xs) - 1))

fn main(console: Console):
    let x = 1
    var count = 0
    count = count + 1
    let (lo, hi) = bounds([3, 5, 9])
    print(console, "${x + count}")
    print(console, "${lo}..${hi}")
```

Top-level `let` declares a module constant (inlined at compile time).
Assigning to a `let`, or to a variable captured by a closure, is a check-time
error (closures capture **by value**; return the new value or use `inout`).
`let _ = expr` evaluates and discards — the same meaning as the bare
expression statement, which is the form `fmt` prints.

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
| `xs[i]` | list indexing, sugar for `list.at(xs, i)`; out of bounds is a runtime error on every backend |
| `lo..hi` | a half-open range (for-loop iteration; never materialized) |
| `x.f(args)` | a METHOD call: resolves to `impl` methods / trait dispatch for `x`'s type |
| `e?` | unwrap `Ok`/`Some` or return the `Err`/`None` from the enclosing function |
| `cap as Dir[Read]` | capability narrowing (drop rights; never widen) |

```witchy
fn double(n: Int) -> Int:
    n * 2

fn main(console: Console):
    print(console, "${7 % 3}")
    print(console, "a" <> "b")
    print(console, "${[1, 2] == [1, 2]}")
    print(console, "${2.5 < 3.0}")
    let xs = [10, 20, 30]
    print(console, "${xs[1]}")
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
        print(console, "big")
    else if n > 3:
        print(console, "medium")
    else:
        print(console, "small")

    var total = 0
    for x in [1, 2, 3, 4]:
        if x == 2:
            continue
        if x > 3:
            break
        total = total + x
    print(console, "${total}")

    var i = 0
    while i < 3:
        i = i + 1
    print(console, "${i}")
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

A `region:` block (optionally `region -> T:`) is a user-controlled allocation
scope: everything allocated inside is reclaimed at the block's end, and the
block's VALUE is what escapes — on the compiled backend it is deep-copied out,
except sub-values from outside the region, which are shared rather than
copied. Assigning a non-scalar variable declared outside the region is a type
error (the value is the only pointer escape; scalar assignments are fine), and
`yield` is rejected. A region never changes observable behavior — only when
memory is reclaimed — so the interpreter runs it as a plain block. The
optional `-> T` ascribes the value's type, guaranteeing the copy-out shape
when inference cannot see it. See `docs/regions.md`.

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

An arm body is an expression, a single inline statement (`0 -> return
Err("zero")`, `Some(v) -> total = total + v`, `_ -> break`), or an indented
block of statements on the lines after the `->`.

```witchy
type Shape:
    Circle(Int)
    Square(Int)

fn describe(s: Shape) -> String:
    match s:
        Circle(r) if r > 100 -> "big circle"
        Circle(r) -> "circle " <> "${r}"
        Square(w) -> "square " <> "${w}"

fn head(xs: List(Int)) -> String:
    match xs:
        [] -> "empty"
        [first, ..rest] -> "first " <> "${first}" <> ", " <> "${list.length(rest)}" <> " more"

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

pub fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Square(w) -> w * w

fn main(console: Console):
    print(console, "${area(Circle(2))}")
    print(console, "${area(Square(3))}")
```

Parameter annotations are required; the return type may be inferred for
non-`pub` functions. Locals are inferred (Hindley-Milner-style unification with
an occurs check).

**Parameter conventions** (Hylo-style value semantics):

| Convention | Meaning |
|---|---|
| (default) | owned, observably immutable value |
| `let` | immutable **borrow**; may not escape — returning a `let`-borrowed parameter is a type error |
| `inout` | the callee mutates and the caller's `var` is **written back** — even on early `return`/`?` |
| `sink` / `own` | ownership transfer; using the source afterwards is a check-time error |
| `move e` | explicitly transfer a binding at a call site; pairs with `sink`/`own` |

```witchy
fn bump(var n: Int):
    n = n + 1

fn main(console: Console):
    var counter = 41
    bump(counter)
    print(console, "${counter}")
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
    print(console, "${apply(add10, 5)}")
```

## 8. Generics and traits

```witchy
import ord

fn largest(xs: List(a)) -> a where a: Ord:
    var best = list.at(xs, 0)
    for x in xs:
        if greater(x, best):
            best = x
    best

fn main(console: Console):
    print(console, "${largest([3, 9, 2, 7])}")
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
    print(console, shout(x))

fn main(console: Console):
    announce(console, Dog)
```

Each `impl Trait` parameter introduces its own type variable, so two of them are
two independent types. It is argument-position only (not a return type), and it
composes with an explicit `where` clause. The std library uses it for
`show.say(console, x: impl Show)` — a `Show`-accepting `print`, so you write
`say(console, value)` instead of converting by hand.

### Deriving the standard traits

`derive(...)` on a type generates the impls you would write by hand —
appended to the module before checking, so the footprint and both backends
see ordinary code. Supported: `Show`, `Eq`, `Ord`, and `Json` (which needs
`import json` and generates `to_json(self) -> Json` over scalars, lists,
options, and nested derived records):

```witchy
import show
import ord

type Point derive(Show, Eq, Ord):
    x: Int
    y: Int

fn main(console: Console):
    say(console, Point(1, 2))
    print(console, "${less(Point(1, 2), Point(1, 3))}")
```

`Show` renders structurally (the `${...}` form), `Eq` is structural equality, and
`Ord` compares record fields lexicographically (records only).

### `comptime:` — compile-time item generation

A top-level `comptime:` block runs **at compile time** with no capabilities
reachable (there is no parameter list to receive one), making it
deterministic by construction. `emit(line)` is its only channel: the output
is parsed as witchy source and **appended** to the module before type
checking and footprint analysis — generated code is analyzed exactly like
handwritten code, and nothing existing can be rewritten, so a comptime
block cannot launder authority out of a signature.

```witchy
comptime:
    var i = 0
    while i < 3:
        emit("pub fn lucky_${i}() -> Int:")
        emit("    ${i * 7}")
        emit("")
        i = i + 1

fn main(console: Console):
    print(console, "${lucky_0()} ${lucky_1()} ${lucky_2()}")
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
        Ok(v) -> "ok: " <> "${v}"
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
    string.join(list.map(xs, fn(n: Int): "${n}"), " ")

fn main(console: Console):
    let squares = [n * n for n in 1..6]
    print(console, show(squares))
    let evens = [n for n in 1..11 if n % 2 == 0]
    print(console, show(evens))
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
    print(console, string.join(list.map(first8, fn(n: Int): "${n}"), " "))

// 0 1 1 2 3 5 8 13
```

## 12. Modules and the standard library

```witchy
import list
import string

fn main(console: Console):
    let shouted = list.map(["a", "b", "c"], fn(s: String): string.to_upper(s))
    print(console, string.join(shouted, "-"))   // A-B-C
```

The core data modules — `list`, `string`, `dict`, `math`, `option`,
`result` — are **the prelude**: always available, no import line needed
(`list.push(xs, 1)` works anywhere). Pure data operations live ONLY in
modules; the global namespace is capability operations (`print`, `read`,
`send`, `now`, …) and `fail` — authority is loud and unprefixed, everything
else says where it came from. (Rendering needs no function at all: `${...}`
interpolation is the rendering.) For other modules,
`import name` brings the module in under its name; **function** calls are
module-qualified (`list.map`). A module's `pub` **types and their constructors**,
however, come into scope *unqualified* — after `import json` you write
`JsonInt(1)` and `JsonObject([...])`, not `json.JsonInt(1)`. Resolution order: a
sibling `name.witchy` file, then the bundled standard library (30+ modules — see
[stdlib.md](stdlib.md)). `pub` items are importable; everything else is
module-private. Package dependencies ("runes")
come from the manifest — see [package-manager.md](package-manager.md).

## 13. Entry point

The program's root authority. `main` may take any number of **capability**
parameters plus an optional `args: List(String)` (the command-line arguments),
and may return `Nil` or `Int` (the process exit code):

```witchy
fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:
    print(console, "running with ${list.length(args)} arg(s)")
    0
```

The host mints exactly these capabilities and nothing else. (This block
type-checks but isn't run by the doc harness, since it needs `Dir` — run it
with `witchy sandbox --dir <root> prog.witchy a b c`.)

### 13.1 The build entrypoint

A rune may ship a **build step**: a top-level `fn build` whose first parameter is
a build capability. It is the root of *build-time* authority, exactly as `main`
is the root of runtime authority — and the two capability sets never mix: `build`
may take **only** build capabilities, and `main` may take none of them.

```witchy
fn build(out: BuildOut, schema: BuildRead, cc: BuildExec):
    let proto = read_build(schema, "api.proto")
    write_out(out, "api.witchy", run_tool(cc, "protoc", proto))
```

| Capability | Grants | Operations |
|---|---|---|
| `BuildOut` | write generated source into this rune's confined output sandbox (needs no naming once the consumer accepts the build step — execution itself is default-deny) | `write_out(out, name, contents)` |
| `BuildRead` | read project files, confined to a granted subtree | `read_build(r, name) -> String` |
| `BuildEnv` | read env vars — only keys *named* in the grant, never the whole environment | `get_build_env(e, key) -> Option(String)` |
| `BuildNet` | HTTP-fetch from hosts on an allow-list (`host:port`, exact) | `fetch_build(n, host, path) -> String` |
| `BuildExec` | invoke a *named* external tool on an allow-list | `run_tool(x, tool, stdin) -> String` |

The types are kind-only — the specific directory/key/host/tool is the consuming
project's *grant*, not the type. `witchy caps` reports the build footprint on its
own axis, `witchy caps-diff` fails on a build-axis widening, and
`witchy build-step <file> [--out <dir>] [--read <dir>] [--env K]... [--exec tool]...`
runs a build step under those confined grants. See
[build-time-execution-plan.md](build-time-execution-plan.md) for status and
[package-manager.md](package-manager.md) §7.1 for the full model.

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
Messages are copied; actors share nothing. Compiled actors run one VM per
actor — own memory, own grant: the VM links only the host-import families
the actor's capability fields entitle it to, and `Dir`/`Net` grants transfer
at `spawn` by handle translation, so this works on both backends and in the
sandbox:

```witchy
actor Confined:
    console: Console
    dir: Dir                       // this actor's whole filesystem view

    on Read(name: String):
        print(console, read(dir, name))

fn main(console: Console, dir: Dir):
    let worker = spawn Confined(console, subdir(dir, "data"))
    send(worker, Read("notes.txt"))      // resolves inside data/ only
```

Capabilities travel at `spawn`, not in messages — a capability-typed message
parameter is a compile error on the compiled backend.

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
