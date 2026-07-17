# Appendix: Operators and Keywords

A quick reference. The [language reference](https://github.com/insanitybit/witchy/blob/master/spec/language.md)
has the precise semantics; this is the cheat sheet.

## Operators

| Operator | Meaning |
|---|---|
| `+ - * / %` | arithmetic; `+` on two Strings concatenates; `Int` wraps on overflow; `/0` and `%0` are runtime errors |
| `== !=` | equality — structural for built-ins, else the type's `PartialEq` impl (`eq`/`ne`) |
| `< <= > >=` | ordering via the type's `PartialOrd` impl (built in for `Int`/`Float`/`String`/`Duration`; derive or implement it for your own) |
| `&&` | short-circuit boolean and |
| `\|\|` | short-circuit boolean or (Bool operands only) |
| `??` | fallback: `Option(T) ?? T` / `Result(T, e) ?? T` **unwraps** (`Some(x)`/`Ok(x)` is `x`, else the right side, evaluated lazily) — right-associative, so `d.get(k1) ?? d.get(k2) ?? 0` chains |
| `!` | boolean not |
| `& \| ^ ~ << >>` | bitwise on `Int` (shift counts masked to 6 bits) |
| `xs[i]`, `d[k]` | strict indexing (sugar for `list.at(xs, i)` / `dict.at(d, k)`); out of bounds or missing-key reads error |
| `xs[i] = v`, `d[k] = v`, `x.f = v` | assign to a place — sugar for a value update (`set_at` / record spread); the binding must be `var`. Compound `+=` etc. work |
| `lo..hi` | half-open range, for iteration only |
| `lo..=hi` | inclusive integer range in a pattern |
| `x.f(a)` | a method call: an `impl`/trait method; standard data modules also keep equivalent module-qualified calls or compiler aliases such as `list.map(xs, f)` |
| `${expr}` | string interpolation — renders *any* value into the string |
| `e?` | unwrap `Ok`/`Some`, or early-return the `Err`/`None` |
| `e? "context"` | propagate with context: prefix a String `Err`, or turn `None` into `Err("context")` |
| `cap as T` | capability narrowing (drop rights; never widen) |
| `..base` | record spread / list rest-pattern |

## Precedence

Tightest to loosest — an operator higher in the table binds before one below it,
so `1 + 2 * 3` is `1 + (2 * 3)` and `a & b == c` is `(a & b) == c`. Postfix
operators bind tighter than everything: `f(x)?` is `(f(x))?` and `p.x + 1` is
`(p.x) + 1`. When in doubt, parenthesize.

| Level | Operators | Associativity |
|---|---|---|
| tightest | `x.f` `x.f(a)` `xs[i]` `e?` (call / index / field / propagate) | left |
| | `- x` `! x` `~ x` (prefix negate / not / bitwise-not) | — |
| | `* / %` | left |
| | `+ -` | left |
| | `<< >>` (bit shift) | left |
| | `&` (bitwise and) | left |
| | `^` (bitwise xor) | left |
| | `\|` (bitwise or) | left |
| | `== != < <= > >=` (comparison) | left |
| | `&&` | left |
| | `\|\|` | left |
| | `??` (unwrap-or) | **right** — `a ?? b ?? c` is `a ?? (b ?? c)` |
| loosest | `lo..hi` `lo..=hi` (range) | — |

Comparison does not chain: write `0 <= x && x < n`, not `0 <= x < n`.

## Keywords

| Keyword | Use |
|---|---|
| `fn` | function definition |
| `let` / `var` | immutable / mutable binding |
| `if` / `else` | conditional (an expression) |
| `match` | pattern match (exhaustive) |
| `for` / `in` / `while` | loops |
| `break` / `continue` / `return` | loop and function control flow |
| `type` | define a record, enum, or sum type (or a `=` alias) |
| `sealed` | restrict construction of a nominal type to its defining module |
| `trait` / `impl` | declare / implement an interface |
| `where` | a trait bound on a generic (`where a: Ord`) |
| `pub` | export an item from its module |
| `import` / `from ... import` | bring a module, or one type and its constructors, into scope |
| `var` | a parameter whose final value is written back to the caller |
| `own` / `move` | ownership transfer of a parameter / at a call |
| `async` / `await` | concurrency: declare an async function / suspend on a future (`spawn` and channels are stdlib functions in `std/task` and `std/chan`, not keywords) |
| `gen` / `yield` | a generator function that yields a lazy `Iter` |
| `as` | capability rights narrowing |
| `comptime` | a compile-time item-generation block (`comptime:`) |
| `capability` | declare a user-defined capability (`capability X from U`) |
| `grantable` | allow a bare user capability to be supplied to an entry point |
| `region` | a scoped temporary-allocation region (`region:` / `region -> T:`) |
| `true` / `false` | boolean literals |

Several declaration and type modifiers are contextual words rather than general
expression keywords: `packed` selects a flat record layout; `frozen`, `unique`,
and `local unique` state ownership contracts; `mode opt` enables checked
performance contracts; and `quote item|expr|type|pattern|stmt|block` constructs
typed syntax inside compile-time code.

## Literals

| Form | Type |
|---|---|
| `42`, `-7` | `Int` (64-bit) |
| `3.5`, `0.5` | `Float` |
| `true`, `false` | `Bool` |
| `"hi\n"` | `String` (escapes: `\n \t \r \0 \\ \" \$`) |
| `30s` `250ms` `5m` `2h`/`2hr` `1d` `1w` | `Duration` |
| `[1, 2, 3]` | `List(Int)` |
| `(1, "a")` | tuple |
| `[e for x in it if c]` | list comprehension |
