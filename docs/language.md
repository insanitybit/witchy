# The witchy language reference

This is the reference for witchy's syntax and semantics. The behavioral
contract is enforced by differential testing: the tree-walking interpreter is
the reference semantics, and the compiled backends (WebAssembly, native) must
produce identical results — `witchy parity <file>` checks any program, and the
test suite holds the backends to **zero silent divergence**, including error
paths.

Companion documents: [capabilities.md](capabilities.md) (the security model),
[stdlib.md](stdlib.md) (the module-by-module API), [architecture.md](architecture.md)
(how the compiler is built).

## 1. Lexical structure

**Layout.** Blocks are indentation-delimited (the off-side rule), opened by a
trailing `:`. Four spaces per level is canonical (`witchy fmt` enforces it).

```
fn classify(n: Int) -> String:
    match n:
        0 -> "zero"
        _ -> if n > 0: "positive" else: "negative"
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

## 2. Types

Builtins: `Int`, `Float`, `Bool`, `String`, `Duration`, `Nil` (the unit type),
`List(a)`, `Dict(k, v)`, tuples `(a, b, ...)`, function types
`fn(Int, String) -> Bool`, and the capability types (`Console`, `Clock`, `Env`,
`Dir[...]`, `Net[...]`, `SigningKey` — see [capabilities.md](capabilities.md)).

**Algebraic data types.** One `type` declaration covers enums, tagged unions,
and records:

```
type Color:                 // enum: nullary variants
    Red
    Green

type Shape:                 // sum type: variants with positional fields
    Circle(Int)
    Square(Int)

type Account:               // record: a single variant with named fields
    name: String
    balance: Int
```

Records construct positionally (`Account("ada", 100)`) or by name
(`Account(name: "ada", balance: 100)`), read fields with `account.name`, and
update functionally with the spread form — a fresh record, overrides first:

```
let richer = Account(balance: a.balance + 1, ..a)
```

`Option(a)` (`Some(x)` / `None`) and `Result(a, e)` (`Ok(x)` / `Err(e)`) come
from `import option` / `import result`.

**Type aliases.** `type Id = Int` names a type without creating a new one.

## 3. Bindings and assignment

```
let x = 1          // immutable binding
var count = 0      // mutable binding
count = count + 1  // assignment (only to `var`)
let (lo, hi) = bounds(xs)   // tuple destructuring
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

Float notes: `0.0 / 0.0` is NaN; `1.0 / 0.0` is infinity; NaN `==` anything is
`false` (IEEE), while NaN *ordering* errors. Conversions: `int_to_float`,
`float_to_int` (saturating truncation), `string_to_int` (strict; errors on
junk or overflow), `sqrt`, `to_string`.

## 5. Control flow

```
if cond:
    ...
else if other:
    ...
else:
    ...

for x in [1, 2, 3]:        // lists, ranges (0..n), and dict views
    if x == 2: continue
    if x > 2: break

while cond:
    ...

if let Some(v) = lookup(d, k):      // bind on match, else fallback
    use(v)
else:
    ...

while let Some(job) = pop(queue):   // loop while the pattern matches
    work(job)

return e   // early return (works in `inout` functions too)
```

## 6. Pattern matching

`match` is exhaustiveness-checked (missing variants are named in the error)
and unreachable arms are rejected. Patterns: literals, `_`, variables,
constructors with nested patterns, tuples, list shapes (`[]`,
`[first, ..rest]`), and guards (`PAT if cond ->`, which don't count toward
exhaustiveness).

```
match shape:
    Circle(r) if r > 100 -> "big circle"
    Circle(r) -> "circle " <> int_to_string(r)
    Square(w) -> "square " <> int_to_string(w)
```

## 7. Functions

```
pub fn area(s: Shape) -> Int:      // `pub` exports from the module
    ...
```

Parameter annotations are required; the return type may be inferred for
non-`pub` functions. Locals are inferred (Hindley-Milner-style unification
with an occurs check).

**Parameter conventions** (Hylo-style value semantics):

| Convention | Meaning |
|---|---|
| (default / `let`) | immutable view; the native backend passes a borrow (no clone) |
| `inout` | the callee mutates and the caller's `var` is **written back** — even on early `return`/`?` |
| `sink` / `own` | ownership transfer; using the source afterwards is a check-time error |
| `move e` | explicitly transfer a binding at a call site |

**Closures.** `fn(n: Int): n + by` — capture by value; calling through a
`fn(...)` -typed value or parameter. Closures cannot assign to captured
variables (check-time error).

## 8. Generics and traits

```
fn largest(xs: List(a)) -> a where a: Ord:
    ...

trait Show:
    fn show(self) -> String

impl Show for Shape:
    fn show(self) -> String:
        ...
```

Generic functions are checked once and monomorphized per concrete use for the
compiled backends (both `where`-bounded and unbounded generics). `Self` in an
impl refers to the implementing type. Trait method calls inside a
`where a: Trait` function resolve on parameters and loop variables; an
intermediate expression may need a `let` first.

The std `Eq`/`Ord`/`Show` traits (`import eq`, ...) provide bounded generic
algorithms (`eq.member`, `ord.max`, ...).

## 9. Errors, `Option`/`Result`, and failure

witchy has no exceptions. Expected failure is a value:

```
fn parse_port(s: String) -> Result(Int, String):
    ...

fn config(dir: Dir[Read]) -> Result(Int, String):
    let raw = read(dir, "port.txt")
    let port = parse_port(raw)?      // Err propagates to the caller
    Ok(port)
```

Unexpected failure is **loud on every backend**: out-of-bounds indexing,
division by zero, unparseable `string_to_int`, NaN ordering, and the `fail(msg)`
primitive all abort (a runtime error interpreted, a trap compiled). The parity
invariant covers these too — a program that errors on one backend errors on
both.

## 10. Modules and the standard library

```
import list
import string

fn main(console: Console):
    print(console, string.join(list.map(["a", "b"], to_upper), "-"))
```

`import name` brings a module in under its name; calls are
module-qualified (`list.map`). Resolution order: a sibling `name.witchy` file,
then the bundled standard library (30+ modules — see [stdlib.md](stdlib.md)).
`pub` items are importable; everything else is module-private. Package
dependencies ("runes") come from the manifest — see
[package-manager.md](package-manager.md).

## 11. Entry point

```
fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:
    ...
```

`main` may take any number of **capability** parameters plus an optional
`args: List(String)` (the command-line arguments), and may return `Nil` or
`Int` (the process exit code). This signature is the program's root authority:
the host mints exactly these capabilities and nothing else.

## 12. Actors

```
actor Logger:
    console: Console            // immutable state field
    var count: Int = 0          // mutable state field

    on Log(line: String):
        count = count + 1
        print(console, line)

fn main(console: Console):
    let logger = spawn Logger(console)
    send(logger, Log("hello"))
```

`spawn` creates an actor with its declared state (capabilities are granted
explicitly at spawn — attenuated, if you choose) and returns a `Subject`.
`send(subject, Msg(...))` is validated at check time against the declared
handlers (unknown messages, wrong arity, and wrong field types are errors).
Messages are copied; actors share nothing. Compiled actors run one VM per
actor (own memory, own grant), preemptible by the scheduler at loop
back-edges.

## 13. In-language tests

```
import testing

fn test_doubling():
    testing.assert_int_eq(double(21), 42)
```

`witchy test <file|dir>` runs every zero-parameter `test_*` function; a test
fails by aborting (the `testing` assertions abort with a message). Tests take
no capabilities, so a suite provably has no effects.

## 14. Semantics guarantees (the parity contract)

The full behavioral surface below is identical on the interpreter and the
compiled backends, verified by differential tests:

- Integer arithmetic wraps; division/modulo by zero errors; shifts mask.
- Float formatting is shortest-round-trip everywhere; NaN/±infinity behave
  identically; NaN ordering errors.
- Equality is structural and deep for every comparable type; `Dict` equality
  is insertion-order-sensitive; multi-parameter generic payloads (`Result`)
  are the one compile-time-rejected comparison.
- String operations are byte-precise across backends; `trim`/case-mapping are
  ASCII-scoped by design.
- Out-of-bounds, overflow-on-parse, and `fail` abort on every backend.
- Capability confinement (`Dir` path resolution, `Net` allowlists) uses the
  same rules in the interpreter and the sandbox.

Anything a backend cannot run identically is a **loud compile or runtime
error, never a silently different answer**.
