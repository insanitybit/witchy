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
    print(console, classify(-3))
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
    (xs.at(0), xs.at(xs.length() - 1))

fn main(console: Console):
    let x = 1
    var count = 0
    count = count + 1
    let (lo, hi) = bounds([3, 5, 9])
    print(console, "${x + count}")
    print(console, "${lo}..${hi}")
```

`let x: Type = e` ascribes the binding: the annotation is a unification
constraint, so it pins type variables the value leaves open (`let xs:
List(Int) = []`, a return-position type variable) and a disagreeing value
errors at the binding line. Locals stay inferred by default — ascribe for
ambiguous literals, checked documentation, and catching a wrong assumption
where it is made.

Top-level `let` declares a module constant (inlined at compile time).
Assigning to a `let`, or to a variable captured by a closure, is a check-time
error (closures capture **by value**; return the new value or use `var`).
`let _ = expr` evaluates and discards — the same meaning as the bare
expression statement, which is the form `fmt` prints.

**Assigning to a place.** Beyond a bare variable, the left of `=` may be a
subscript or a field — `xs[i] = v`, `d[k] = v`, `acct.balance = b` (the binding
must be a `var`). Each is sugar for reassigning the variable
(`xs = xs.set_at(i, v)`, `acct = Account(balance: b, ..acct)`), so it keeps value
semantics while reading like in-place mutation — and the uniqueness analysis makes
it an in-place update. Compound forms (`xs[i] += v`) work too.

```witchy
type Account:
    name: String
    balance: Int

fn main(console: Console):
    var xs = [1, 2, 3]
    xs[0] = 9
    xs[1] += 5
    var acct = Account("ada", 100)
    acct.balance = acct.balance + 50
    print(console, "${xs}")
    print(console, "${acct.balance}")
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
    let _ = xs.length()                  // explicit discard — `length` is not a mutator
    print(console, "${xs}")              // [1, 2]
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
| `xs[i]` | list indexing, sugar for `list.at(xs, i)`; out of bounds is a runtime error on every backend |
| `lo..hi` | a half-open range (for-loop iteration; never materialized) |
| `x.f(args)` | a method call: an `impl`/trait method on `x`, **or** the stdlib UFCS form `module.f(x, args)` for a built-in type (so `xs.map(f)` *is* `list.map(xs, f)`) |
| `e?` | unwrap `Ok`/`Some` or return the `Err`/`None` from the enclosing function |
| `e? "msg"` | like `e?` with context: a `Result` `Err` gets `"msg: "` prepended; an `Option` `None` becomes `Err("msg")` |
| `cap as Dir[Read]` | capability narrowing (drop rights; never widen) |

```witchy
fn double(n: Int) -> Int:
    n * 2

fn main(console: Console):
    print(console, "${7 % 3}")
    print(console, "a" + "b")
    print(console, "${[1, 2] == [1, 2]}")
    print(console, "${2.5 < 3.0}")
    let xs = [10, 20, 30]
    print(console, "${xs[1]}")
    print(console, "${list.head(xs) ?? 0}")
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
    print(console, "${xs}")              // [10, 20, 30, 40]
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
        print(console, "first even: ${v}")
    else:
        print(console, "none")
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
    print(console, describe(Circle(2)))
    print(console, size(5))
    print(console, size(50))
    print(console, head([10, 20, 30]))
    // Irrefutable destructuring shares the same grammar:
    let ((a, b), c) = ((1, 2), 3)
    print(console, "${a} ${b} ${c}")
    for (k, v) in [(1, 2), (3, 4)]:
        print(console, "${k}=${v}")
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
    print(console, "${counter}")
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
    print(console, "${apply(add10, 5)}")
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
- **`await`** lowers the rest of the function after the seam into a continuation
  closure (§14), so a `var` declared before an `await` cannot be mutated after it
  — carrying mutable state across an `await` hits the same closure rule. Recurse
  with an `async fn`, or thread the state through a channel (`chan.serve`), instead.
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
    say(console, "${total(table)}")
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
    print(console, "${largest([3, 9, 2, 7])}")
    print(console, largest(["apple", "pear", "fig"]))
```

Generic functions are checked once and monomorphized per concrete use for the
compiled backends (both `where`-bounded and unbounded generics). `Self` in an
impl refers to the implementing type. Trait method calls inside a
`where a: Trait` function resolve on any expression whose type the checker
knows — parameters, loop variables, and the results of calls — so an
intermediate expression rarely needs a `let` binding to dispatch.

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

The std comparison hierarchy `PartialEq` → `Eq` → `PartialOrd` → `Ord` (in
`import cmp`, mirroring Rust's `std::cmp`) backs the `== != < > <= >=` operators
and provides bounded generic algorithms (`cmp.member`, `cmp.max_of`, `cmp.sort`,
...); `Show` (`import show`) renders.

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

`derive(...)` generates trait impls for a type. The generated code is appended to
the module before type-checking, so both backends and the footprint analysis treat
it like handwritten code. The supported derives are `Show`, `Eq`, `Ord`, `Reflect`,
and `Deserialize`. `Reflect` needs `import reflect` and makes a user type
reflectable (scalars and the built-in containers already are); it is what lets
`json.stringify` / `json.value_of` encode the type with no per-type code.
`Deserialize` generates `from_json(j) -> Result(Self, String)` for scalars,
lists, options, and nested records, and — because the generated body names them
like handwritten code — needs `import json` **and `import result`** (plus
`import option` when any field is an `Option`). There is no `Serialize` derive,
because reflection already encodes any value (`json.value_of`, `json.stringify`,
`Into(Json)`); only decoding has to be generated per type.

```witchy
import show
import cmp

type Point derive(Show, PartialEq, Eq, PartialOrd, Ord):
    x: Int
    y: Int

fn main(console: Console):
    say(console, Point(1, 2))
    print(console, "${Point(1, 2) < Point(1, 3)}")
```

`Show` renders a value structurally (the same form `${...}` uses); `PartialEq`/`Eq`
are structural equality (backing `==`/`!=`), and `PartialOrd`/`Ord` compare record
fields in order (records only) and back `<` `>` `<=` `>=`.
Derives also work on a generic type. `type Box(a) derive(Reflect)` generates an impl
that carries the type parameters and their bounds and specializes per type argument.

### Reflection and anonymous structs

`reflect(x)` returns a value's structure as a `Mirror`, which lets one function
handle a value of any type. `List`, `Option`, tuples, and generic records implement
`Reflect` through ordinary generic impls, so `json.stringify(x)` and
`reflect.debug(x)` work on a list, an option, a tuple, or a nested record without a
per-type impl.

An anonymous struct, `.{ field: expr, ... }`, is a record with no declared type. It
reflects like any record, so you can build JSON from plain values without declaring
a type or constructing `Json` by hand:

```witchy
import json

fn main(console: Console):
    let files = [("a.txt", "hi")]
    print(console, json.stringify(.{files: files}))
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
    let c: Celsius = (5).into()
    print(console, "${c.deg}")
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

### Tagged literals — compile-time `tag"…"`

A string literal written **immediately after an identifier**, `tag"a${x}b"`, is a
*tagged literal*. It is expanded **at compile time**, like `comptime:`, but in
**expression** position: the lexer splits the literal into its static fragments
and its `${…}` hole sources, and the compiler calls the `tag` function

```text
fn tag(parts: List(String), holes: List(String)) -> String
```

with `parts` = the static fragments and `holes` = an **opaque marker** per hole —
a token the tag *places* where that hole's value belongs (the tag does not read
the hole's source). The tag returns witchy **expression source**; the compiler
parses it and **substitutes** the real hole expression — parsed once at the call
site, carrying its source position — at each marker, then splices the result over
the literal before type checking. So both backends compile the same AST, the tag
runs once in the compiler, and a hole's marker may be placed zero, once, or many
times. The tag is an ordinary function (local or imported); only the items
reachable from it run at expansion time.

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
    print(console, greet"hi ${name}")
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
    print(console, show(ratio(100, 5, 2)))
    print(console, show(ratio(100, 0, 2)))
```

`?` optionally takes a context message — `e? "msg"` — and works wherever bare `?`
does. On a `Result`, it prepends `"msg: "` to a propagated `String` error; on an
`Option`, a propagated `None` becomes `Err("msg")`. Either way the enclosing
function propagates a `String` error. The message may interpolate. Bare `e?`
propagates unchanged.

```witchy
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    if b == 0:
        Err("division by zero")
    else:
        Ok(a / b)

fn ratio(a: Int, b: Int) -> Result(Int, String):
    let q = checked_div(a, b)? "computing ${a}/${b}"
    Ok(q + 1)

fn main(console: Console):
    match ratio(10, 0):
        Ok(v) -> print(console, "${v}")
        Err(e) -> print(console, e)
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
    print(console, "${dict.get(ages, "ada") ?? 0}")
    print(console, "${dict.get(ages, "bob") ?? 0}")
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
    print(console, list.join(list.map(first8, fn(n: Int): "${n}"), " "))

// 0 1 1 2 3 5 8 13
```

## 12. Modules and the standard library

```witchy
import list
import string

fn main(console: Console):
    let shouted = ["a", "b", "c"].map(fn(s: String): s.to_upper())
    print(console, shouted.join("-"))   // A-B-C
```

The core data modules — `list`, `string`, `dict`, `math`, `option`,
`result` — are **the prelude**: always available, no import line needed
(`list.push(xs, 1)` works anywhere). Pure data operations live ONLY in
modules; the global namespace is capability operations (`print`, `read`,
`send`, `now`, …) and `fail` — authority is loud and unprefixed, everything
else says where it came from. (Rendering needs no function at all: `${...}`
interpolation is the rendering.) For other modules,
`import name` brings the module in under its name; **function** calls are
module-qualified (`list.map(xs, f)`) — or, for a built-in type's own operations,
the equivalent method form (`xs.map(f)`, see §4), which is the idiom for the data
libraries. A module's `pub` **types and their constructors**,
however, come into scope *unqualified* — after `import json` you write
`JsonInt(1)` and `JsonObject([...])`, not `json.JsonInt(1)`. Resolution order: a
sibling `name.witchy` file, then the bundled standard library (30+ modules — see
[stdlib.md](stdlib.md)). `pub` items are importable; everything else is
module-private. Package dependencies ("runes")
come from the manifest — see [package-manager.md](../rfcs/package-manager.md).

## 13. Entry point

The program's root authority. `main` may take any number of **capability**
parameters plus an optional `args: List(String)` (the command-line arguments),
and may return `Nil` or `Int` (the process exit code):

```witchy
fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:
    print(console, "running with ${args.length()} arg(s)")
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
dial, confine its **address-set** with typed policy values from `std/confine`.
`net.only(policy)` intersects the carried set with `policy` (an endpoint survives
only if already admitted); `net.deny(policy)` subtracts a slice. Both are monotone —
refinement only ever shrinks — and host-enforced **at the syscall** on both
backends, the address analog of `dir.subtree` for `Dir`.

```witchy
import confine

fn main(console: Console, net: Net):
    let db = net.only(confine.tcp("10.0.0.5", 6379))
    let safe = net.deny(confine.cidr_any("10.0.0.0/8")).only(confine.tcp("192.168.1.1", 80))
    print(console, "net confined")
```

The policy constructors are `confine.tcp(host, port)`, `any_port(host)`,
`cidr(block, port)`, `cidr_any(block)`, `union(a, b)`, and `private()` — the
internal IP ranges (loopback, RFC-1918, link-local incl. the `169.254.169.254`
metadata IP, CGNAT) for the one-line SSRF/rebinding guard
`net.deny(confine.private())`. A CIDR/IP policy is
checked against the *resolved* IP, so it is rebinding-safe. TLS is not a right or a
policy scheme but a connect-time `tls:` prefix on the address you dial
(`connect(net, "tls:host:443")`); see
[0003-network-address-scoping.md](../rfcs/0003-network-address-scoping.md) and
[0009-https-tls-client.md](../rfcs/0009-https-tls-client.md).

A `Dir` likewise carries an **entry policy** narrowing which entries it may touch:
`dir.only(confine.ext(".txt"))` confines it so `read`/`write`/`open` admit only
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
import chan

async fn producer(tx: Sender(Int)) -> Nil:
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.spawn(producer(tx)).await
    chan.consume(rx, fn(n): chan.done(print(console, "got ${n}"))).await
```

`chan.channel(cap)` is a bounded channel — the sender blocks when it is full;
pass `0`, or use `chan.unbounded()`, for no backpressure. `chan.recv(rx).await`
yields the next message or `None` once the channel closes (no task can send to it
anymore). `chan.consume`/`chan.serve` write the receive-loop for you (`serve`
threads state through each message). A channel can be shared by many receivers
(a worker pool) or many senders. Each channel is typed independently — a program
may use channels of many different message types (the executor carries messages
erased and each endpoint recovers its own type). A spawned task returns `Nil`,
reporting results over a channel. See the book's *Concurrency* chapter and
`std/chan` for the full model.

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
