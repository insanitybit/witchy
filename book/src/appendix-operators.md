# Appendix: Operators and Keywords

A quick reference. The [language reference](https://github.com/insanitybit/witchy/blob/master/docs/language.md)
has the precise semantics; this is the cheat sheet.

## Operators

| Operator | Meaning |
|---|---|
| `+ - * / %` | arithmetic; `Int` wraps on overflow; `/0` and `%0` are runtime errors |
| `<>` | string concatenation |
| `== !=` | structural (deep) equality |
| `< <= > >=` | ordering — `Int`/`Float`/`String`/`Duration` only |
| `&& \|\|` | short-circuit boolean and/or |
| `!` | boolean not |
| `& \| ^ ~ << >>` | bitwise on `Int` (shift counts masked to 6 bits) |
| `xs[i]` | list indexing (sugar for `at(xs, i)`); out of bounds errors |
| `lo..hi` | half-open range, for iteration only |
| `x.f(a)` | method-call sugar for `f(x, a)` |
| `${expr}` | string interpolation (sugar for `to_string(expr)`) |
| `e?` | unwrap `Ok`/`Some`, or early-return the `Err`/`None` |
| `cap as T` | capability narrowing (drop rights; never widen) |
| `..base` | record spread / list rest-pattern |

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
| `trait` / `impl` | declare / implement an interface |
| `where` | a trait bound on a generic (`where a: Ord`) |
| `pub` | export an item from its module |
| `import` | bring a module into scope |
| `inout` | a parameter whose final value is written back to the caller |
| `sink` / `own` / `move` | ownership transfer of a parameter / at a call |
| `actor` / `on` / `spawn` | concurrency: actor type, message handler, spawn |
| `gen` / `yield` | a generator function that yields a lazy `Iter` |
| `as` | capability rights narrowing |
| `true` / `false` | boolean literals |

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
