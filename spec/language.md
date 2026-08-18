# The witchy language reference

This is the reference for witchy's syntax and semantics. The behavioral
contract is enforced by differential testing: the tree-walking interpreter is
the reference semantics, and the compiled backend (WebAssembly) must produce
identical results. The test suite and a CI sweep (the project's
`witchy parity` harness) hold the backends to **zero silent divergence**,
including error paths.

Every ` ```witchy ` block below is a complete program that the test suite
type-checks - and, when it needs nothing beyond `Console`, runs on both
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
| `"sum: ${a + b}"` | `String` | interpolation - `${expr}` renders data values (see below); inner strings may be bare (`"${f("x")}"`) or escaped (`"${f(\"x\")}"`) |
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
ordinary data values - scalars, record fields, lists, tuples, records, sum types,
dicts, and their supported nesting - identically on both backends. Function
values aren't data rendering targets and are rejected before runtime. `Show`
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
`Rand`, `Dir[...]`, `File[...]`, `Net[...]`, `Fetch`, `Exec`, `SecretStore`,
`Secret` - see [capabilities.md](capabilities.md)).

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

### Declared packed layouts

`type P packed:` is a checked physical-layout contract for closed fixed-size
data. Its first shipped class contains `Int`, `Float`, `Bool`, `Duration`, nested
packed records or tuples, and fixed-layout packed sums; a reference,
capability, open type variable, existential, String, or dynamically sized list
cannot be an inline packed field. Logical behavior is unchanged: reflection and
type identity don't expose offsets, padding, headers, or destination state.

With the optimized compiled layout enabled, each closed packed shape has one
versioned canonical `LayoutId`. Packed records, `List(P)`, tuples containing a
packed component, and fixed-layout packed sums retain that descriptor through
local construction and direct named calls, including calls linked across user
modules. Lists use one descriptor-strided buffer; sums use the descriptor's tag
width and aligned payload band. The source signature doesn't mention the
physical pointer ABI.

The current boundary matrix is exact and fail-closed:

| Use | Shipped behavior |
|---|---|
| Direct named or linked user-module parameter/result | Exact packed record, list, packed-containing tuple, or fixed-sum layout is preserved. |
| Closed direct generic use | Specializes by logical type, access envelope, parameter/result `LayoutId`s, and optimization schema. Packed construction, indexed traversal, mutation, direct/recursive helper calls, and return keep that physical instance; open or unsupported uses reject. |
| Function value, lambda/closure capture, trait method, or existential witness | Rejects when the call or environment would need a specialized physical signature. |
| Host call | Rejects today. Generated ABI metadata accepts no specialized layout until a real exact or explicitly counted marshal adapter exists. |
| Whole-value equality | Derived/default structural `==` and `!=` on a fixed-layout packed sum use its descriptor-selected tag width, variant child layouts, and physical payload offsets. Custom `PartialEq`, other specialized equality, and rendering reject. |
| `region:` result, worker/channel transport, or another unsupported dynamic operation | Rejects rather than boxing, reshaping, or copying through the ordinary slot representation. |

A direct function returning an exact `unique` packed record may have a hidden
destination only when every successful return path constructs that result and
the checked ownership envelope has no incompatible `own`/`var` capacity state.
The caller may forward compatible dead storage; fixed packed sums additionally
support a proven nonescaping immediate-consumer scratch. An escaping old value,
layout mismatch, nested allocating payload, or incomplete constructor path keeps
the allocating call. The counters `destination_candidates_forwarded`,
`packed_alloc_calls`, and `packed_alloc_bytes` expose which path ran.

Header-free packed lists are narrower still. In `mode opt`, a nonempty immutable
local `List(Packed)` receives an elided-header descriptor only after a
whole-module proof excludes signatures, whole-value boundary calls/returns,
aliases, mutation, nested scopes, dynamic wrappers, and loans for that exact
type. All other packed lists retain the RC header. Generated modules expose exact
emitted/elided header counters for differential tests.

`Option(a)` (`Some(x)` / `None`) and `Result(a, e)` (`Ok(x)` / `Err(e)`) are
prelude names: the types and constructors need no import. `import option` /
`import result` only brings in the qualified helpers (`option.map`,
`result.map_ok`).

**Type aliases.** `type X = …` names a shape without creating a new type -
`type Id = Int` makes `Id` and `Int` fully interchangeable, and the alias may
be generic (`type Pair(a) = (a, a)`; `Pair(Int)` is `(Int, Int)`). The `=` vs
`:` distinction is the rule to remember: **`type X = …` names a shape and
never mints a type; `type X: …` mints a nominal type with constructors** -
only the latter can be sealed, carry `impl`s, or hold an invariant. Alias
cycles are a link-time error.

**Structural records and anonymous unions.** The structural tier is the family
of types that are named by shape instead of by declaration: tuples `(A, B)`,
function types `fn(A) -> B`, anonymous records `.{field: Type}`, and anonymous
tagged unions `.[Tag | Tag(Payload)]`.

Anonymous-record identity remains exact. You can write the type in parameters,
returns, fields, aliases, and generic arguments, and field order doesn't affect
identity: `.{x: Int, y: Int}` and `.{y: Int, x: Int}` are the same type.

A richer anonymous record may flow to a poorer expected anonymous-record type
when every required field has exactly the required type. This is a directed
projection at an explicit expected-type site - an annotation, assignment,
argument, return/tail, typed aggregate slot, or `as` - not general subtyping. It
evaluates the source once and constructs the exact target shape, so rendering,
reflection, JSON, equality, and runtime type information cannot observe the
discarded fields. Unconstrained inference, branch joins, containers, function
types, and equality remain exact. A `var` argument is invariant and never uses
record projection because write-back could not reconstruct omitted fields.

Compose a new exact record shape with one type spread:
`type Located = .{..Point, line: Int}`. The base must resolve to an anonymous
record. Identical duplicate fields collapse; conflicting duplicates and
non-record bases are errors. This type-level form is distinct from value update
spread (`.{x: ..., ..base}`), which updates one already-exact value shape.

Anonymous tagged unions are closed tag sets. A value is injected with a leading
dot (`.Missing`, `.BadPort(70000)`) and must have an expected union type from a
return annotation, `let` annotation, call argument, or enclosing constructor.
Anonymous unions have their own representation-preserving widening rule:
`.[A | B]` may flow into `.[A | B | C]` at argument, return/tail, and `?`
propagation sites. Record conformance instead constructs a new exact value.

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
constructor and must go through the module's public functions - so those
"smart constructors" are the one place an invariant is established, and a
value of the type is *proof* the invariant holds. Sealing restricts
**construction only**: field reads and `match` work from anywhere. A record
update, including `value.field = replacement`, constructs a new whole value and
therefore remains confined to the defining module.

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
library uses it widely - `Set` (distinct members), `semver.Version`
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
errors at the binding line. Locals stay inferred by default - ascribe for
ambiguous literals, checked documentation, and catching a wrong assumption
where it's made.

Top-level `let` declares a module constant (inlined at compile time).
Assigning to a `let` is a check-time error. A closure also cannot assign to a
variable it captured: closures capture **by value**, so return the new value or
use a `var` parameter when a closure-like helper should write through.
`let _ = expr` evaluates and discards - the same meaning as the bare
expression statement, which is the form `fmt` prints.

**Assigning to a place.** Beyond a bare variable, the left of `=` may be a
subscript or a field - `xs[i] = v`, `d[k] = v`, `acct.balance = b` (the binding
must be a `var`). The destination base and projection coordinates are captured
once, then the right-hand side is evaluated, and finally the updated aggregate is
written back to the root. This keeps value semantics while reading like in-place
mutation, and uniqueness analysis may perform the store in place. Compound forms
(`xs[i] += v`, `d[k] += v`) work too.

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

A parameter declared `var` is move-in/move-out in every call position. The
argument must be a mutable place; the callee receives its current value and every
structured return writes the callee's final parameter value back to that place.
The ordinary return value is independent, so `pop(var xs) -> Option(a)` both
updates `xs` and produces an element. Free, method, trait, closure, and indirect
calls use the same rule; statement position merely permits discarding an
independent result from a call that has a resolved `var` effect.

A non-`var`, non-`Nil` call whose result is thrown away is a **compile error**:
bind it or discard it explicitly with `let _ =`. A `var` call on a `let` binding
or temporary is also an error. To derive a changed copy, create a mutable copy
explicitly (`var ys = xs; ys.push(4)`).

```witchy
fn main(console: Console):
    var xs = []
    xs.push(1)
    xs.push(2)
    let _ = xs.length() // explicit discard - `length` is not a mutator
    console.print("${xs}") // [1, 2]
```

## 4. Expressions and operators

Everything is an expression; a block's value is its final expression.

| Operators | Meaning |
|---|---|
| `+ - * / %` | arithmetic (`Int` wraps; `/ 0` and `Int.MIN / -1` are runtime errors on every backend); `+` on two Strings concatenates - never coerces |

| `== !=` | equality through `PartialEq`, at **every depth** - the derived/default impl is deep structural equality (lists, tuples, records, enums, `Option`, `Dict` insertion-order-sensitive); a **custom `impl PartialEq`** is honored inside containers too (a `List(P)`/`Option(P)`/tuple/`Dict` value of a type with a hand impl compares by that impl). Function and capability types do **not** compare - `==` on them is a compile-time error |
| `< <= > >=` | ordering on `Int`/`Float`/`String`/`Duration` only; ordering a NaN is a runtime error; compounds don't order |
| `&&` | short-circuit boolean **and** (Bool operands) |
| `\|\|` | short-circuit boolean **or** (Bool operands only - for a fallback value use `??`) |
| `??` | the **fallback** operator: `Option(T) ?? T -> T` unwraps `Some` or yields the fallback on `None`; `Result(T, e) ?? T -> T` unwraps `Ok` or yields the fallback on `Err` (the error is discarded - reach for `?` / `match` when it matters). The fallback is evaluated **lazily** (only on `None`/`Err`). Right-associative and the loosest binary operator, so `d.get(k1) ?? d.get(k2) ?? 0` chains and `d.get(k) ?? n + 1` is `d.get(k) ?? (n + 1)`. There is no truthiness: `""` and `[]` are values, not absences - default them with an explicit test (`if name.is_empty(): "anon" else: name`) |
| `!` | negation |
| `& \| ^ ~ << >>` | bitwise on `Int` (shifts mask the count to 6 bits) |
| `xs[i]`, `d[k]` | strict indexing, sugar for `xs.at(i)` / `d.at(k)`; out of bounds or missing-key reads are runtime errors on every backend |

### Evaluation order

Expressions evaluate in deterministic source order. A method receiver precedes
its explicit arguments; call arguments evaluate left to right as written (also
for labels); an index evaluates its base before its coordinate; unary operands
evaluate first and binary operands evaluate left before right. `&&`, `||`, and
`??` short-circuit. Tuple/list elements, constructor and record fields, and
interpolation holes evaluate left to right. Comprehension generators nest left to
right; each filter precedes the element expression for that iteration. `if`
evaluates its condition before the selected branch, and `match`/`if let` evaluate
their scrutinee before selection. Assignment captures destination coordinates,
then evaluates the right-hand side, then stores.

A `var` argument captures and reads its place at its argument position. Its
write-back commits after the callee returns and before the call yields its ordinary
result. Earlier `var` arguments remain reserved until that commit: later arguments
may read snapshots, but may not write back to an overlapping place. Two `var`
arguments with the same root are accepted only when their projections are proven
disjoint (for example `swap(xs[0], xs[1])`); dynamic or prefix-overlapping places
are rejected.
| `lo..hi` | a half-open range (for-loop iteration; never materialized) |
| `x.f(args)` | a method call: an `impl`/trait method on `x` - the primary form for the standard data types; every public method also has an equivalent module-qualified alias (`list.map(xs, f)` is `xs.map(f)`) |
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
junk or overflow - `string.parse_int` is the `Option`-returning version), `math.sqrt`, and `${...}` for rendering to strings.

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
mutable value semantics) - a mutation of `x` lands in `xs`, in place when the
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
don't pass the capability - a function that never receives a capability cannot
use it, alias it, or forge it. That structural boundary (capture-as-DI) is
witchy's firewall; see [the capability reference](capabilities.md).

A `region:` block (optionally `region -> T:`) is a user-controlled allocation
scope: everything allocated inside is reclaimed at the block's end, and the
block's VALUE is what escapes - on the compiled backend it's deep-copied out,
except sub-values from outside the region, which are shared rather than
copied. Assigning a non-scalar variable declared outside the region is a type
error (the value is the only pointer escape; scalar assignments are fine), and
`yield` is rejected. A region never changes observable behavior - only when
memory is reclaimed - so the interpreter runs it as a plain block. The
optional `-> T` ascribes the value's type, guaranteeing the copy-out shape
when inference cannot see it. See [RFC-0034](../rfcs/regions.md).

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

There is **one pattern grammar**, used in every binding position - `match` arms,
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
literals **cannot** be matched - exact float equality is a precision trap
(bind and guard instead: `x if math.float_abs(x - 1.5) < eps ->`); a `Float`
*scrutinee* bound to a variable is fine (`match f: x -> …`). `Duration` literals
**can** be matched - a Duration is an exact millisecond count, and `-1s` is a
negative duration literal in both expression and pattern position.

**Contexts differ only by refutability.** `match` / `if let` / `while let`
accept any pattern. `let` / `for` / comprehensions require an **irrefutable**
pattern - one the checker proves always matches: `_`, a variable, a tuple of
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
needs a final `_`/binding arm - witchy does no numeric-coverage analysis).
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
        [first, ..rest] -> "first " + "${first}" + ", " + "${rest.length()}" + " more"

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

**Target availability.** An unannotated function is shared: every selected
target may include it, so it may reference only other shared functions.
`@browser`, `@server`, and `@static` mark a function as available only in that
target. A specialized function may reference shared functions and functions
with the same annotation. Direct calls and first-class function references are
both checked; conflicting annotations are rejected. Availability is checked
when a named function value is introduced, so a shared higher-order helper may
apply a callback after the caller has proved that callback is available in its
own target.

This is composition checking, not conditional compilation. The compiler rejects
a shared or browser function that captures a server-only function instead of
silently changing the program for one build.

```witchy
fn normalize(name: String) -> String:
    name.trim()

@browser
fn browser_title(name: String) -> String:
    normalize(name)

@server
fn server_title(name: String) -> String:
    normalize(name)

@static
fn static_title(name: String) -> String:
    normalize(name)

fn main(console: Console):
    console.print(normalize(" Witchy "))
```

**Recursive proper tail calls use constant control stack.** A call qualifies
when its complete result becomes the current function's result directly, with no
caller calculation, write-back, drop, conversion, loan cleanup, or error
inspection remaining. This includes calls in a function's final expression,
`return`, and the selected tail branches of `if`, `match`, blocks, and `??`. It
excludes calls inside operators, constructors, `?`, guards, conditions, and
arguments. Self recursion lowers to a loop; mutually recursive direct functions
lower to a typed state machine that preserves each function's public ABI.
Monomorphized generic calls, resolved trait calls, and devirtualized closure calls
participate as direct edges after specialization. A genuinely indirect scalar
closure edge uses its exact function-table signature and runtime table index to
select a typed state-machine bank; mixed scalar result kinds use the closure
slot representation inside the component and recover their declared kind at its
public boundary. Direct self and mutual edges with reference-valued results keep
their exact `externref` or GC-reference result kind through the loop or dispatcher.
A genuinely indirect closure edge keeps its exact scalar, `externref`, or GC
reference signature through the typed closure table and dispatcher. Reference
parameters and results are never boxed into an integer slot.

No tail-call keyword or sigil is required. Arguments still evaluate left to
right and are rebound simultaneously (`f(b, a)` swaps correctly). A textually
final `var` call is proper only when its complete move-out envelope can be
forwarded unchanged; ordinary caller-place reconstruction is real residual
work. The guarantee bounds stack, not heap allocation or total running time.

**Functional-in-place state kernels.** In `mode opt`, a directly self-recursive
function with exactly one `own state: unique T` parameter and result `unique T`
opts into the FIP contract when it recurses. `T` must be a record with scalar
stored fields, and every auxiliary parameter must be scalar. Every non-recursive
exit must return `state` directly. Base cases use explicit returns; the recursive
edge must be the function's final expression and pass `state` directly in the
same parameter slot. The body may perform scalar work, read and update fields
rooted at `state`, and branch for base-case exits. Aggregate/closure construction,
other calls, owner escape, `?`, async or generator suspension, loops, ranges,
indexed access, and regions are rejected.

The compiled tier forwards the complete `(value, ownership-token)` result into
one loop, so recursive depth adds no allocation, deallocation, free-list reuse,
arena rewind, or control-stack growth. This is a checked `mode opt` resource
contract, not a semantic restriction in normal mode and not a promise for every
function returning `unique`. No `fip` keyword or attribute exists; the ownership
signature, direct recursion, and `mode opt` state the contract.

**Parameter conventions** (Hylo-style value semantics):

| Convention | Meaning |
|---|---|
| (default) | owned, observably immutable value - the callee may read but the caller sees no change |
| `let` | immutable **borrow**; may not escape - returning a `let`-borrowed parameter is a type error |
| `var` | move-in/move-out write-back: requires a mutable caller place and commits the parameter's final value on every structured return, independently of parameter position or ordinary return type |
| `own` | ownership transfer; the **callee** consumes the argument, so using the source afterwards is a check-time error |
| `move e` | use-site ownership transfer; the **caller** consumes the source binding (see below), idiomatically paired with `own` |

```witchy
fn bump(var n: Int) -> Int:
    n = n + 1
    n

fn main(console: Console):
    var counter = 41
    let result = bump(counter)
    console.print("${counter} ${result}")
```

`bump` writes `42` back to `counter` and independently returns `42`. The same
convention appears in function types (`fn(var Int) -> Int`) and is part of type
identity: `fn(Int) -> Int` isn't interchangeable with it. A `var` parameter may
not have a default, and async/generator functions may not declare one because a
suspension could outlive the caller's place.

`own` and `move` are two independent ways to end a binding's life, meeting in the
middle. `own` consumes from the **callee** side: passing any variable to an `own`
parameter marks it moved, so a later use is a check-time error
(*use of `x` after it was moved*). `move x` consumes from the **caller** side: it
ends `x` *whatever the callee's convention is* - into a default, `let`, or `own`
parameter alike - so a later use of `x` is the same check-time error even when the
parameter only took an ordinary copy. The two compose: `f(move x)` into an `own`
parameter is a hand-off both sides spell out, and on the compiled backend it's a
guaranteed no-copy move. `move` is **not** accepted into a **procedure-channel**
`var` parameter - that argument must be a live mutable place, since the callee
writes it back. On both backends `move` is
value-neutral (value semantics copy already); it changes only *when* a copy is
elided, never any result.

**Closures.** `fn(n: Int): n + by` captures by value; you call through a
`fn(...)` -typed value or parameter. Closures cannot assign to captured
variables (check-time error). A closure may declare its return type -
`fn(n: Int) -> Bool: n > 0` - which also makes it a `?` boundary: a `?` inside
the closure propagates to the closure's own `Result`/`Option`, not the enclosing
function's, so closures can short-circuit on errors just like named functions.
Failure propagation rebuilds the enclosing return type's `None` or `Err`
representation. The success payload type of the operand may differ from the
function's success type, including across scalar, capability-reference, and
typed-GC layouts; only the propagated error contract must be compatible.

Function values preserve the concrete runtime kinds in their signatures. A
direct capability reference or concrete GC aggregate may therefore cross an
indirect call as a parameter, result, or `var` write-back without being boxed
into an integer slot. This applies to named functions, annotated lambdas, and
lambdas whose concrete signature is inferred at their use site. A named
polymorphic function is monomorphized from each concrete use-site function type
before compiled closure lowering. Aliases, higher-order arguments, assignments,
control-flow joins, and pattern bindings all use that rule; parameter
conventions and generic bounds are preserved. Result-only type variables can be
fixed by the expected function type, and a generic function may return another
generic or bounded function value; specialization follows those references to a
fixpoint. Unannotated parameters that don't carry a type variable don't block
specialization. Scalar-only parameters and results retain the universal scalar
ABI, but the function value itself always uses the uniform GC closure wrapper.
A boxed lambda's captures live in a per-lambda typed GC struct, so direct
capabilities, other closures, and fixed-layout GC aggregates remain references
throughout creation, aliasing, and indirect invocation.

Function values may be fields of concrete tuples and closed nominal instances,
including generic instances and recursive sums. `Option(reference)` uses a
nullable reference; closed `Result` values use typed GC sums. Every
reference-bearing `List(T)` uses a typed GC array of its exact element kind and
supports literals, persistent push/set/concat, `pop`, length, indexed access,
list-pattern destructuring, and iteration, including nested lists and lists of
reference-bearing aggregates.
`Dict` reference payloads and open generic calls instantiated with
capability-bearing values remain check-time errors. Capability-typed callbacks
crossing an isolated worker adapter remain rejected because workers receive
only the scalar cross-instance callback ABI.

```witchy
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn adder(by: Int) -> fn(Int) -> Int:
    fn(n: Int): n + by

fn identity(x: a) -> a:
    x

fn main(console: Console):
    let add10 = adder(10)
    console.print("${apply(add10, 5)}")
    let same_int = identity
    console.print("${same_int(5000000000)}")
```

**Keyword arguments and default parameters.** A direct call to a free, module-qualified,
or UFCS method call may pass arguments by parameter **name**: a positional
prefix followed by labeled arguments (`connect(host: "x", port: 443)`). Labels
bind to the declaration's parameter names and may appear in any order, but every
argument still evaluates in **source order** (left to right as written, not in
parameter order). A suffix parameter may declare a **closed-constant default**
(`port: Int = 443` - a literal or other compile-time-constant expression); a call
that omits it splices the default in. Defaults live at the declaration site: they
do **not** attach to a function *value*, and labels and defaults are erased before
either backend runs, so they cost nothing at runtime. Direct calls through a function
value remain positional-only; declarations on receiver-less function values still do
not receive labels/defaults from call sites. A `var` (write-back) parameter cannot
have a default.

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
witchy has value semantics: every boundary that carries a value out of a scope -
a default call argument, a closure capture, a task message - carries a **copy**,
so a mutation is never observed through that copy and there's no shared mutable
state to reason about. The one mechanism that writes back to a caller is a `var`
*parameter* (above), and even that's a single handed-over variable with no
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

**Ownership/immutability qualifiers** (`frozen`, `unique`, `local unique`, and
borrowed `View`) are
compile-time *contracts* on a type - distinct from the calling conventions above,
they live on the type and propagate through it. They have no runtime
representation (both backends lower `frozen T`/`unique T` to `T`), so they never
change observable behavior; they only let the checker enforce, and a library
*promise*, an ownership fact:

| qualifier | meaning |
|---|---|
| `frozen T` | deeply immutable - sharing is safe; declaring it mutable (`var`/`own`) is a check-time error |
| `unique T` | the sole reference - may be mutated in place and returned as `unique` |
| `local unique T` | unique within this call only - may be mutated but **may not escape** (returning it's a check-time error) |
| `View(T, 'a)` | a read-only view whose lifetime is tied to an input `let('a) T`; available in `mode opt` |

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

The qualifiers don't change values or representation, but they can strengthen
the performance contract. For a `var` parameter typed `unique` or `local unique`,
`mode opt` rejects a missing ownership proof and names the alias, move, or loan
that invalidated it. Normal mode remains value-correct through the collection's
copy-on-write fallback when storage is shared. `List.pop`, `Dict.insert`, and
`Dict.remove` use this rule for their no-container-copy guarantee. See
[performance.md](performance.md).

A callable that returns a `unique List` or `unique Dict` carries the producer's
hidden capacity state into the receiving binding. The same logical envelope is
used by direct calls, typed function values, typed lambdas, and existential
trait witnesses: the ordinary result, each `var` write-back, and their ownership
state cross the call together. Each reachable return must establish the state
from fresh list/dict storage or another typed `unique` collection result. A
proper self-tail accepted by the ownership-envelope lowering forwards that
complete envelope through its loop. Callable type identity also preserves
parameter conventions, uniqueness qualifiers, write-back ownership outputs,
and borrowed-view owner relations; an ascription that erases those contracts is
rejected.

### Borrowed views

A `mode opt` function may return a read-only view tied to one or more borrowed
inputs. Every returned lifetime must be bound by an input of the same name:

```witchy
mode opt

fn inspect(let xs: let('a) List(Int)) -> View(List(Int), 'a):
    xs
```

The relation is part of the function type. Direct calls, resolved trait calls,
and indirect function values preserve it; assigning a function value to a type
that erases or changes the owner positions or parameter conventions is a
check-time error.

#### Borrowed nominal declarations

A nominal type declared in a `mode opt` module may bind lifetime parameters in
the same parameter list as ordinary type parameters. Lifetime parameters carry
a leading apostrophe and have a distinct kind from ordinary type parameters,
even when their names otherwise have the same spelling:

```witchy
mode opt

type Parser(a, 'input):
    value: View(a, 'input)
    offset: Int
```

Each declared lifetime must be unique and used by at least one field, and each
lifetime used by a field must appear in the nominal type's parameter list. This
form is available for records and single-variant positional types; a
lifetime-parameterized multi-variant sum isn't supported. Applying the type
requires a type argument for each ordinary parameter and a lifetime argument
for each lifetime parameter in the declared order.

Compile-time reflection exposes the normalized declaration through
`module_types`. `meta.TypeInfo.params` preserves explicit type and lifetime
parameters in source order, including the apostrophe on lifetime parameters;
inferred ordinary parameters follow in first-use order. A `View(a, 'input)`
field is represented structurally as `meta.TBorrowed(meta.TNamed("a", []),
"input")`, so generators can inspect the owner relation without parsing source
text.

Borrowed nominal values may be constructed, copied, projected, and returned
when their declared lifetime relations preserve the checked owner roots. A
mutable borrowed record shell may update an owned scalar field; the write-back
keeps the shell's hidden owner-root companions unchanged for a scalar update.
Replacing a declared borrowed field is also permitted when the replacement
preserves its declared lifetime relation: lowering closes the retired root set
after reading the shell and opens the replacement set after the write-back.
Replacing an owned aggregate field is outside the current scalar-mutation
slice. Calls or storage that erase the declared lifetime relation remain
rejected.

A `List(B('a))`, where `B` is a direct lifetime-parameterized nominal, may be
constructed, read with `list.at`, and traversed with `for`. The compiler keeps
hidden owner-root companions for the list and transfers the selected companions
to a value read from `list.at`; a dynamic index conservatively retains every
possible element root. A loop binder is a read-only borrowed shell and keeps
the list companions live for the iterator expression. Overwrite, drop, and nested
containers are now supported; relation-erasing boundaries remain rejected unless
explicitly materialized through a typed owned-companion conversion.

Binding a returned view loans the corresponding owner until the view's final
use. During that interval the owner may be read, but it may not be moved,
reassigned, mutated, or passed to a `var`/`own` parameter, and the view may not
escape through a closure, task, channel, mutable binding, or owned aggregate.
Forwarding a bound view preserves its original owner loan. Persisting a view of
a temporary is rejected because there's no stable owner; immediately calling
`.owned()` on that result is allowed. A projection of an already-bound view
must likewise be materialized before it's persisted, so the new alias cannot
lose the projected storage layout. A live view may not cross `await`, `break`,
or `continue`, and lambda bodies are checked as independent scopes. Last-use
precision is statement-level within a straight-line block; an enclosing loan is
conservatively live throughout a nested branch or loop body. Calling `.owned()`
from `import borrow` produces an owned snapshot and is such a final use.

Views have the same logical value representation as their inner type. On the
compiled backend, lowering retains a hidden root for refcounted linear-memory
owners until the checked final use and releases it on normal, explicit-return,
and `?` paths. Opening the loan also invalidates the owner's uniqueness token,
so later update/extract operations copy and re-own rather than mutating storage
still observed by the materialized value. Host-backed views and capability
leases require a capability-specific API and aren't introduced by this rule.

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
knows - parameters, loop variables, constructor-pattern bindings, destructured
tuple slots, and the results of calls - so an intermediate expression rarely
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
trait bound, `impl Trait` says so directly - `x: impl Loud` is sugar for a fresh
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
two independent types. It's argument-position only (not a return type), and it
composes with an explicit `where` clause. The std library uses it for
`show.say(console, x: impl Show)` - a `Show`-accepting `print`. The `show`
module is preluded; `from show import say` is needed only when you want the bare
`say(...)` spelling.

### Existential trait types - `dyn Trait` ([RFC-0081](../rfcs/0081-existential-trait-values.md))

`dyn Render` / `dyn Convert(Int)` is an existential trait type: a value whose
concrete type is hidden behind a trait's callable surface. It's an owned value:
the interpreter carries the concrete value plus an authenticated witness, while
compiled Wasm uses a typed payload box plus an immutable witness-table index.
Both backends select witnesses from the same closed linked program and agree on
dispatch, write-back, failures, and supertrait upcasts.

- `dyn` is contextual and only in type position: it must be followed by an
  uppercase trait name or a lowercase module plus uppercase trait name, so a
  bare `dyn` remains an ordinary type variable. Both `dyn Render` and
  `dyn render.Render` parse in every type position - parameters, returns,
  aliases, generic arguments, tuples, function types, `let` annotations, and
  the explicit `value as dyn Trait` cast. A concrete value erases implicitly
  only where an expected type is already existential: annotations, arguments,
  returns and tail expressions, assignments, typed collection and tuple slots,
  and constructor fields. Every other construction requires `as dyn Trait`;
  unconstrained inference never replaces a concrete type with an existential.
  A `var dyn Trait` argument is invariant: its caller place must already have
  that existential type, because callee write-back may contain a different
  concrete witness. Read-only and `own` arguments retain directed erasure.
  `??` doesn't map an `Option`/`Result` payload implicitly: its payload and
  fallback still have one concrete type. Convert both paths explicitly, or use
  an existential payload type, when their original concrete types differ.
- Identity is the resolved trait declaration plus its fully substituted
  arguments. Non-ambient trait declarations, supertraits, impl heads, bounds,
  and `dyn` heads resolve to `module.Trait` before modules merge. Aliases and
  imports that name the same declaration normalize to one identity. Unrelated
  same-spelled traits in different modules remain distinct; a bare reference
  imported from both modules is an error that names the `module.Trait`
  alternatives. The comparison traits and preluded `Show` retain their ambient
  bare identities.
- A trait must be existential-safe to be used as `dyn Trait`: every method has
  a receiver, introduces no method-local type parameters, doesn't return bare
  `Self`, mentions `Self` nowhere but the receiver, and doesn't return a
  result borrowed from the hidden receiver; every trait type parameter must be
  fixed by concrete arguments. One unsafe method blocks the trait, and the
  diagnostic names every blocking method and rule (`dyn PartialEq` is rejected:
  its second `Self` parameter violates the `Self`-position rule).
- Capability-carrying payloads are rejected transitively (records, sums,
  tuples, containers): a directed or explicit conversion whose concrete type
  contains a `Dir`/`File`/`Net`/… fails at check time. Borrowed existentials
  (`View(dyn T, 'a)`) are excluded from v1.
- Bare, `let`, `var`, and `own` receivers keep their ordinary conventions.
  `var self` and explicit `var` arguments move in and write back together on a
  structured return; nested places are rebuilt only after success, overlapping
  caller places are rejected, and a trap exposes no partial write-back. `own`
  consumes the existential and later use is a check-time error.
- `dyn Sub` converts to `dyn Super` only when `Super` is a declared transitive
  supertrait. The conversion projects an authenticated witness while retaining
  the same hidden payload; unrelated conversions and forged witness selection
  fail before execution.
- Existential values have no automatic equality, ordering, hashing, reflection,
  serialization, type-name/address/witness inspection, or downcast. Only methods
  explicitly declared on the existential-safe trait are callable. A domain that
  needs comparison or a stable key declares that operation as a trait method.
- Constructing an owned existential may allocate its payload box. `mode opt`
  preserves exactly the same values and traps but doesn't promise allocation
  removal or devirtualization; either optimization is optional and unobservable.

Heterogeneous values dispatch through their own witnesses:

```witchy
trait Render:
    fn render(let self) -> String

type Number:
    Number(Int)

type Label:
    Label(String)

impl Render for Number:
    fn render(let self) -> String:
        match self:
            Number(value) -> "number=${value}"

impl Render for Label:
    fn render(let self) -> String:
        match self:
            Label(value) -> "label=${value}"

fn main(console: Console):
    let parts: List(dyn Render) = [Number(7), Label("ready")]
    for part in parts:
        console.print(part.render())
```

### Deriving the standard traits

`derive(...)` generates trait impls for a type. The generated code is appended to
the module before type-checking, so both backends and the footprint analysis treat
it like handwritten code. The supported derives are `Show`, `PartialEq`, `Eq`,
`PartialOrd`, `Ord`, `Reflect`, `Deserialize`, and `PublicState`. `Reflect` needs
`import reflect` and makes a user type reflectable (scalars and the built-in
containers already are); it's what lets
`json.stringify` / `json.from_value` encode the type with no per-type code.
`Deserialize` generates `from_json(j) -> Result(Self, String)` for scalars,
lists, options, and nested records, and - because the generated body names them
like handwritten code - needs `import json`. `Result`/`Ok`/`Err` and
`Option`/`Some`/`None` are prelude names, so generated deserialize code can use
them without redundant imports. There is no `Serialize` derive,
because reflection already encodes any value (`json.from_value`, `json.stringify`,
`Into(Json)`); only decoding has to be generated per type.

`derive(PublicState)` recursively proves that every field may cross a public or
resumable web boundary. Scalars, `List`, `Option`, `Result`, and nested nominal
types compose when their contained values also satisfy `PublicState`.
Capabilities, functions, `Bytes`, secrets, and host handles deliberately have
no implementation, so a rejected nested field fails during type checking. The
trait is a sealed compiler boundary: handwritten and user-generated impls are
rejected; only the canonical standard foundations and the authenticated built-in
derive may produce a proof.
`public_state.to_json` additionally requires `Reflect`; public-state eligibility
doesn't by itself define a serialization format.

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

### `comptime:` - compile-time item generation

A top-level `comptime:` block runs **at compile time** with no capabilities
reachable (there's no parameter list to receive one), making it
deterministic by construction. Legacy `emit(line)` output, and direct
`console.print(line)` for compatibility, are parsed as witchy source and
**appended** to the module before type checking and footprint analysis.
`emit_item(item)` is the typed RFC-0080 migration channel for
`meta.ItemSyntax`. `quote item:` and a literal whole-item
`meta.item("...")` are stored as compiler-owned item AST and appended without
formatting or reparsing. A hole-bearing item keeps that AST and replaces its
exact expression, type, and pattern placeholder nodes with the typed hole
payloads. Dynamic `meta.item(source)` input is parsed exactly once at its
constructor boundary and then remains compiler-owned. A
single `comptime:` block may use the legacy source channel or the typed item
channel, but not both.
Compiler syntax values such as `meta.ItemSyntax`, `meta.TypeSyntax`,
`meta.ExprSyntax`, `meta.PatternSyntax`, `meta.StmtSyntax`, `meta.BlockSyntax`,
`meta.MatchArmSyntax`, and `meta.Ident` are compile-time-only: runtime
functions, fields, aliases, and expressions cannot store or return them.
`quote expr:` retains a compiler-owned expression AST plus a canonical source
projection. When the quote contains holes, the compiler stores an expression
template and structurally replaces its exact hole nodes. A compiler-owned hole
transfers its AST directly; a source-backed compatibility hole parses only its
own payload, never the enclosing expression. Literal `meta.expr_raw("...")`
plans are promoted to the same owned representation when parsed; dynamic input
is parsed exactly once at the constructor boundary. Direct item holes and typed
tagged-literal output consume the resulting AST without reparsing. Structural
`meta.expr_*` builders retain their owned child nodes.
`quote type:` values likewise retain a compiler-owned type AST. A hole-bearing
quote stores a type template and structurally replaces its exact hole nodes. A
compiler-owned hole transfers its AST directly; a source-backed compatibility
hole parses only its own payload. Literal `meta.type_join(parts, holes)` plans
are promoted to the same owned representation when parsed. Direct item holes
consume the result without reparsing, including anonymous record/union types and
borrowed views. Other `meta.type_*` builders project canonical source and remain
the compatibility construction path.
`quote pattern:` values retain a compiler-owned pattern AST as well. A
hole-bearing quote stores a pattern template and structurally replaces its exact
hole nodes. Compiler-owned holes transfer their AST directly; source-backed
compatibility holes parse only their payload. Literal
`meta.pattern_join(parts, holes)` plans are promoted to the same owned
representation. Direct item holes consume the result without reparsing; other
`meta.pattern_*` builders remain the compatibility construction path.
`quote stmt:` and `quote block:` values retain compiler-owned body AST. Their
expression, type, and pattern holes are exact template nodes and are substituted
structurally through the mixed `meta.SyntaxHole` envelope. Literal
`meta.stmt_raw`, `meta.block_raw`, `meta.stmt_join_syntax`, and
`meta.block_join_syntax` plans are promoted back to owned syntax when they
contain one parser-valid body. Existing statement/block builders may project
canonical source for compatibility when constructing an item.
Top-level `comptime fn` declarations are helpers for this expansion phase. They
may mention compile-time-only syntax types, may be called from `comptime:`,
custom-derive, or tagged-literal expansion, and are stripped before the runtime
module is linked and type-checked. Imported tags remain available to sibling
expansion even after their defining module has been expanded. Runtime code cannot
call them.
`std/meta` also exposes compatibility source-backed syntax builders such as `ident`,
`type_named`, `expr_call`, `pattern_anon_ctor`, `match_arm`, `stmt_let`,
`block`, `param`, and `function_block`; they make generated item structure
typed at the API boundary and validate identifier spelling. `meta.fresh(hint)`
returns a deterministic compiler-owned `Ident` for generated bindings. Repeated
calls and separate comptime blocks or tagged-literal invocations produce distinct
names in the source-reserved `__` namespace, so a handwritten binding cannot
capture them. When a typed tag returns a compiler-owned expression, direct
function calls, function references, type names, constructor expressions, and
constructor patterns written in that expression resolve in the tag's defining
module, including through that module's imports; generated lexical bindings
still shadow normally.
`meta.call_site(name)` is the explicit escape for an identifier that must resolve
in the syntax consumer's scope. Its consumer fixes the category:
`meta.expr_name` creates a value/function or constructor expression,
`meta.type_named` creates a type with structural arguments, and
`meta.pattern_ctor` creates a constructor pattern with structural subpatterns.
Structural quotation and typed-tag transport preserve those invocation-site
origins without a forgeable source spelling. Qualified-name composition and
source-projecting compatibility builders still consume only validated spelling.
A call-site type alias expands in the consuming module's alias environment, not
the generator's. General field and item origin channels remain future work.
`quote expr:`, `quote type:`, `quote pattern:`, `quote stmt:`, `quote block:`,
and `quote item:` are the first quotation forms. They parse the indented
expression, type, pattern, statement, block, or single item immediately and
produce `meta.ExprSyntax`, `meta.TypeSyntax`, `meta.PatternSyntax`,
`meta.StmtSyntax`, `meta.BlockSyntax`, or `meta.ItemSyntax`. Every hole-free
quote category uses the compiler-owned channels described above. Hole-bearing
quotations in every category are structural; only general compatibility-builder
composition may still project and parse canonical source.
Inside `quote expr:`,
`${hole}` splices a `meta.ExprSyntax`; inside `quote type:` and
`quote pattern:`, `${hole}` splices a `meta.TypeSyntax` or `meta.PatternSyntax`;
inside `quote stmt:` and `quote block:`, `${hole}` splices a
`meta.ExprSyntax`, `meta.TypeSyntax`, or `meta.PatternSyntax` in expression,
type, or pattern positions. `quote item:` accepts the same expression, type,
and pattern holes anywhere those grammar positions occur inside the quoted
item. Holes are typed by the surrounding `comptime`/tag generator, not by
runtime interpolation. Item-hole placement is structural. Directly supplied
compiler-owned expression, type, and pattern values retain their AST. An
Every quote retains its enclosing template AST across hole substitution; values
composed by other compatibility builders remain source-backed until their
structural slices land.
`quote type:` covers named, generic, module-qualified, tuple, function,
ownership-qualified, capability-right, anonymous structural, and borrowed-view
types.
`witchy expand <file.witchy>` prints the entry module after `comptime:` item
generation and tagged-literal expansion, rendered as canonical witchy source.
It's an inspection tool: it doesn't type-check, compile, or dump bundled
standard-library implementation modules.

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

### Tagged literals - compile-time `tag"…"`

A string literal written **immediately after an identifier**, `tag"a${x}b"`, is a
*tagged literal*. It's expanded **at compile time**, like `comptime:`, but in
**expression** position: the lexer splits the literal into its static fragments
and its `${…}` hole sources, and the compiler calls the `tag` function

```text
comptime fn tag(parts: List(String), holes: List(String)) -> meta.ExprSyntax
```

A tag may accept a third `String` parameter. The compiler supplies
`module:line` invocation metadata for diagnostics:

```text
comptime fn tag(parts: List(String), holes: List(String), origin: String) -> meta.ExprSyntax
```

The metadata isn't a stable semantic identity. A tag derives stable IDs from
its normalized static parts and slot structure, keeping source location separate.

with `parts` = the static fragments and `holes` = an **opaque marker** per hole -
a token the tag *places* where that hole's value belongs (the tag doesn't read
the hole's source). A tag returns `meta.ExprSyntax`; String-returning tags are
rejected. A compiler-owned expression transfers its AST through the expansion
event directly. A tag that must construct source dynamically uses the explicit
`meta.expr_raw` bridge, which parses once when the syntax value is constructed.
Reachable tag evaluator items may not contain tagged literals in their bodies or
initializers; nested tags are returned as expression syntax and expanded after the outer tag
returns. Generated dynamic source resolves against the definition module's
direct imports, and call-site holes resolve against the invocation module's
direct imports. Transitive imports never become implicit qualifier scope.
Direct function, type, and constructor
references in compiler-owned output resolve in the tag's definition module,
including private function helpers and directly imported public declarations; an
invocation-site declaration with the same spelling cannot capture them.
Definition-site identity doesn't bypass sealed-type construction rules. The
compiler then **substitutes** the real hole
expression - parsed once at the call site, carrying its source position - at each
marker and splices the result over the literal before type checking. So both
backends compile the same AST, the tag runs once in the compiler, and a hole's
marker may be placed zero, once, or many times. The tag is local or imported;
only its module-qualified reachable closure runs at expansion time, including
reachable comptime helpers, constants, constructors, traits, and implementations
in directly imported modules. Two direct imports exporting the same tag name are
ambiguous and rejected. Bundled standard-library modules don't export tag entry
points; library tags live in ordinary imported runes.

Because a tag emits *code*, interpolation holes are typed **by position** (the
substituted expression is type-checked normally) and there's no runtime string
parser. Hole expressions resolve at the **call site** (hygiene), while direct
functions, types, constructors, and constructor patterns written in
compiler-owned typed output resolve at the **definition site**. A generator can
opt an expression, type, or constructor pattern back into invocation-site
resolution by passing `meta.call_site("name")` to `meta.expr_name`,
`meta.type_named`, or `meta.pattern_ctor`. A type error
in a hole points back **into the literal** at that `${…}`, not at generated code.
The `html` tag in the `glamour` rune uses this: a `${userInput}` in text position
becomes a `text(…)` **node**, never markup, so it's XSS-immune by construction.

```witchy
import meta

// A tag receives the static parts and an opaque MARKER per hole; it places each
// marker where the hole's value goes, then constructs one typed expression. The
// compiler substitutes the real hole expression (here `name`, resolved at the
// call site) at the marker.
comptime fn greet(parts: List(String), holes: List(String)) -> meta.ExprSyntax:
    meta.expr_raw("\"Hello, \" + " + holes.at(0))

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
invariant covers these too - a program that errors on one backend errors on
both. An abort is terminal for that VM instance: it cannot be resumed after a
partially unwound call. Structured `return` and `?` paths aren't aborts and run
their ordinary ownership cleanup.

**Unwrapping with `??`.** For a quick value-or-default, `Option(T) ?? T` unwraps
to a bare `T` (§4): `Some(x) ?? d` is `x`, `None ?? d` is `d` (with `d` evaluated
only when absent). `Result(T, e) ?? T` unwraps `Ok` likewise, discarding the
error. It's `unwrap_or` with operator syntax - handy on the `Option`-returning
lookups (`d.get(key)`, `list.head`, …).

```witchy
fn main(console: Console):
    let ages = dict.from_pairs([("ada", 36)])
    console.print("${ages.get("ada") ?? 0}")
    console.print("${ages.get("bob") ?? 0}")
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
    let first8: List(Int) = iter.collect(fibs().take(8))
    console.print(first8.map(fn(n: Int): "${n}").join(" "))

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

The core data modules - `list`, `string`, `dict`, `math`, `option`, `result`,
`policy`, and `show` - are **the prelude**: always available, no import line needed
(`xs.push(1)` and `dict.new()` work anywhere). Pure data operations belong to
their data type or its module - never bare globals; capability operations are
**methods on the capability that carries the
authority** (`console.print(msg)`, `dir.read(path)`, `clock.now()`) - the
authority is loud because it names the capability, and `fail` is the one bare
global. (Rendering needs no function at all: `${...}` interpolation is the
rendering.) For other modules,
`import name` brings the module in under its name. A standard data type's own
operations are **methods** (`xs.map(f)`, see §4) - the primary form for the data
libraries. Every public inherent method also has an equivalent module-qualified
alias (`list.map(xs, f)`), so the qualified spelling always works too; the
remaining module-level functions - constructors such as `iter.range` and
`dict.from_pairs`, and helpers whose argument isn't the module's own type, such
as `json.stringify(x)` - are called module-qualified. A module's `pub`
**types and their constructors** are module-scoped
the same way: after `import json` you name them qualified (`json.Json`,
`json.JsonInt(1)`, `json.JsonObject([...])`). To use a type and its constructors
*unqualified*, name it explicitly with `from json import Json` - a from-imported
type brings its variant constructors into scope bare, so `JsonInt(1)` and
`JsonObject([...])` then work directly. (In a `match` whose scrutinee type is
known, bare variant names always resolve against that type, so match arms need
no qualifier either.) Two unqualified bindings of the same name collide at the
import line, not at first use. Bundled standard-library module names are
reserved and always resolve to their canonical compiler-shipped source; a local
module must use another name. Other imports resolve a sibling `name.witchy`
file or a package dependency. `pub` items are importable; everything else is
module-private. Package dependencies ("runes")
come from the manifest - see [package-manager.md](../rfcs/package-manager.md).

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
type-checks but isn't run by the doc harness, since it needs `Dir` - run it
with `witchy sandbox --dir <root> prog.witchy a b c`.)

`main` may ask for any of the host capabilities - `Console`, `Clock`, `Env`,
`Rand`, `Console[...]`, `Dir[...]`, `File[...]`, `Net[...]`, `Fetch`, `Exec`,
`SecretStore` - and the launch grant backs each: `--dir <root>` a `Dir`,
`--file <path>` a `File` (the i-th `File` parameter ← the i-th `--file`),
`--net <host:port>` a `Net` allowlist entry, `--fetch <scheme://host:port>` a
`Fetch` origin, `--secret`/`--signing-key` a `SecretStore`; grant-document
`[env]`, `[exec]`, and `[fetch]` entries carry the corresponding allowlists. A `File[Read]` lets a single-file
program ask for exactly one file instead of a whole `Dir`. A **grant document**
(`--grants app.grants.toml`) enumerates the whole grant as reviewable TOML and is
cross-checked against the computed footprint - see
[capabilities.md](capabilities.md) and
[0013-capability-grant-documents.md](../rfcs/0013-capability-grant-documents.md).

`main` may also receive a library-defined capability when its declaration is
explicitly `grantable`. The capability must be **bare**: none of its fields may
carry transitive host authority such as `Net`, `Dir`, or `SecretStore`. The host
mints each value from the `[user_caps]` entry whose key matches the parameter
name:

```text
grantable capability UiRoot:
    policy: String

fn main(console: Console, ui: UiRoot):
    console.print("UI policy granted")
```

```toml
# app.grants.toml
[user_caps]
ui = { type = "UiRoot", policy = "coven-web" }
```

Grantable capabilities are policy authority, not aliases for hidden host
authority. `witchy caps` therefore reports them on a separate `user caps` axis,
and adding one to an entrypoint is a footprint widening. The full minting,
bareness, and exported-root contract is in the **Grantable capabilities** section
of [capabilities.md](capabilities.md) and
[RFC-0038](../rfcs/0038-grantable-user-capabilities.md).

### 13.1 The build entrypoint

A rune may ship a **build step**: a top-level `fn build` whose first parameter is
a build capability. It's the root of *build-time* authority, exactly as `main`
is the root of runtime authority - and the two capability sets never mix: `build`
may take **only** build capabilities, and `main` may take none of them.

```witchy
fn build(out: BuildOut, schema: BuildRead, cc: BuildExec):
    let proto = schema.read_build("api.proto")
    out.write_out("api.witchy", cc.run_tool("protoc", proto))
```

| Capability | Grants | Operations |
|---|---|---|
| `BuildOut` | write generated source into this rune's confined output sandbox (needs no naming once the consumer accepts the build step - execution itself is default-deny) | `out.write_out(name, contents)` |
| `BuildRead` | read project files, confined to a granted subtree | `r.read_build(name) -> String` |
| `BuildEnv` | read env vars - only keys *named* in the grant, never the whole environment | `e.get_build_env(key) -> Option(String)` |
| `BuildNet` | HTTP-fetch from hosts on an allow-list (`host:port`, exact) | `n.fetch_build(host, path) -> String` |
| `BuildExec` | invoke a *named* external tool on an allow-list | `x.run_tool(tool, stdin) -> String` |

The types are kind-only - the specific directory/key/host/tool is the consuming
project's *grant*, not the type. `witchy caps` reports the build footprint on its
own axis, `witchy caps-diff` fails on a build-axis widening, and
`witchy build-step <file> [--out <dir>] [--read <dir>] [--env K]... [--exec tool]...`
runs a build step under those confined grants. See
[build-time-execution-plan.md](../rfcs/build-time-execution-plan.md) for status and
[package-manager.md](../rfcs/package-manager.md) §7.1 for the full model.

### 13.2 User-definable capabilities

A library declares its own capability by **refining** the host's, with
`capability X from U`. `X` is a *sealed brand*: a single-variant wrapper over the
underlying capability `U` (or several - `from (A, B)`), with one rule - `X` may be
**constructed or destructured only inside the module that declares it**. Any other
module may hold, pass, and return a value of `X`, but cannot mint or unwrap one, so
`X` is un-forgeable exactly like a host capability.

```witchy
capability Redis from Net[Connect, Tcp]

// The ONLY way to obtain a `Redis` - its constructor is sealed to this module.
pub fn open(net: Net[Connect, Tcp]) -> Redis:
    Redis(net)

pub fn ping(r: Redis) -> Int:
    match r:
        Redis(net) -> 1
```

- **Minting consumes authority.** A `Redis` can only be made by handing a real
  `Net` to `open`; a library can never conjure authority from nothing.
- **Narrowing is by facet** - declare a narrower capability refining the first
  (`capability ReadOnly from Postgres`) that exposes fewer operations; ordinary
  type-checking enforces it.
- **The footprint sees through.** `witchy caps` reports a user capability as the
  host authority it refines - `ping` audits as `Net[Connect, Tcp] (refined: Redis)` -
  so a library cannot launder `Net` behind a friendly name.

A second form lets a capability **carry state beside** the authority it wraps - a
sealed *record* mixing one or more host capabilities with ordinary policy data:

```witchy
capability Postgres:
    net: Net[Connect, Tcp]
    table: String

// Sealed constructor - only this module can mint, refine, or destructure one.
pub fn open_db(net: Net[Connect, Tcp], table: String) -> Postgres:
    Postgres(net, table)

pub fn scope(p: Postgres) -> String:
    match p:
        Postgres(_, table) -> table
```

The fields are private - reached with `match`, never `.field` - so the underlying
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
only if already admitted); `net.deny(policy)` subtracts a slice. Both are monotone -
refinement only ever shrinks - and host-enforced **at the syscall** on both
backends, the address analog of `dir.subtree` for `Dir`.

```witchy
fn main(console: Console, net: Net):
    let db = net.only(Net.tcp("10.0.0.5", 6379))
    let safe = net.deny(Net.cidr_any("10.0.0.0/8")).only(Net.tcp("192.168.1.1", 80))
    console.print("net confined")
```

The policy constructors are `Net.tcp(host, port)`, `Net.any_port(host)`,
`Net.cidr(block, port)`, `Net.cidr_any(block)`, `Net.union(a, b)`, and `Net.private()` - the
internal IP ranges (loopback, RFC-1918, link-local incl. the `169.254.169.254`
metadata IP, CGNAT) for the one-line SSRF/rebinding guard
`net.deny(Net.private())`. A CIDR/IP policy is
checked against the *resolved* IP, so it's rebinding-safe. TLS isn't a right or a
policy scheme but a connect-time `tls:` prefix on the address you dial
(`net.connect("tls:host:443")`); see
[0003-network-address-scoping.md](../rfcs/0003-network-address-scoping.md) and
[0009-https-tls-client.md](../rfcs/0009-https-tls-client.md).

A `Dir` likewise carries an **entry policy** narrowing which entries it may touch:
`dir.only(Dir.ext(".txt"))` confines it so `read`/`write`/`open` admit only
matching files (a non-matching name is denied at the access check; a subtree
inherits the policy) - the filesystem analog of `net.only`. See
[0011-capability-refinement.md](../rfcs/0011-capability-refinement.md).

## 14. Concurrency: async, spawn, and channels

Concurrency is **cooperative async tasks** that communicate over **channels**. A
function marked `async` may `await`; calling it yields a task that does nothing
until driven. `chan.spawn` starts a task concurrently, and a `chan.channel` is a
first-class value you create and pass to whichever tasks share it - spawning and
channels are *independent* (no task has an implicit mailbox). Tasks share no
memory, so there are no locks or data races, and the round-robin schedule is
deterministic - identical output on the interpreter and the compiled WebAssembly.

```witchy
from chan import Sender

async fn producer(tx: Sender(Int)):
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.spawn(producer(tx)).await
    chan.consume(rx, fn(n): chan.done(console.print("got ${n}"))).await
```

`chan.channel(cap)` is a bounded channel - the sender blocks when it's full
while the executor can make progress; pass `0`, or use `chan.unbounded()`, for no
backpressure. If every live task parks with no progress, the executor runs its
quiescence close pass: parked receives/selects resume as `None`, parked sends are
released, and parked joins resume. That is the close condition
`chan.recv(rx).await` and `chan.consume` observe; witchy doesn't refcount sender
values, so "closed" doesn't mean no `Sender` value can ever be used again. A
channel can be shared by many receivers (a worker pool) or many senders. Each
channel is typed independently - a program may use channels of many different
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

`witchy test <file|dir>` runs every selected `test_*` function; a test fails by
aborting (the `testing` assertions abort with a message). Plain tests receive no
real authority. Capability-parameterized tests use either validated inert
fixture plans (`--fixtures`, deterministic and transcripted) or explicit owned
integration grants (`--integration`); dependency tests never inherit real
grants or the ability to construct fixture records.

## 16. Semantics guarantees (the parity contract)

The full behavioral surface below is identical on the interpreter and the
compiled backends, verified by differential tests:

- Integer arithmetic wraps; division/modulo by zero errors; shifts mask.
- Float formatting is shortest-round-trip everywhere; NaN/±infinity behave
  identically; NaN ordering errors.
- Equality goes through `PartialEq` at every depth: the derived/default impl is
  deep structural equality (`Dict` insertion-order-sensitive), and a custom impl
  is honored inside containers too. `==`/`!=` on function or capability types is a
  compile-time error (no meaningful, stable equality - this replaces a former
  backend divergence). `Dict` keys and `Set` members require `Eq`, so a `Float`
  key/member is a compile-time error (closing the NaN-key hole).
- String operations are byte-precise across backends; `trim`/case-mapping are
  ASCII-scoped by design.
- Out-of-bounds, overflow-on-parse, and `fail` abort on every backend.
- Capability confinement (`Dir` path resolution, `Net` allowlists) uses the
  same rules in the interpreter and the sandbox.

Anything a backend cannot run identically is a **loud compile or runtime error,
never a silently different answer**.
