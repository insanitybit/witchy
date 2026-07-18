---
rfc: 0021
title: "`||` unwraps an Option to its inner type (Option(T) || T -> T)"
status: implemented
created: 2026-06-28
superseded-by:
tracking:
---

# RFC-0021: `||` unwraps an `Option` to its inner type

This proposal is superseded by [RFC-0048](0048-fallback-operator.md), which
replaced the overloaded `||` behavior with the dedicated `??` fallback and
restored `||`/`&&` to Bool-only logic.

> Code blocks here are intentionally **not** tagged `witchy` (per RFC-0002's
> convention): they are illustrative sketches, not complete programs, and must
> not be executed by the doc-test harness. This matters more than usual here —
> the proposed form does **not** compile today.

## Summary

Extend the `||` truthy-fallback operator so that `Option(T) || T` evaluates to a
bare `T`:

- `Some(x) || d` is `x`
- `None || d` is `d` (and `d` is evaluated **only** when the left is `None`)

This is `option.unwrap_or` with short-circuit evaluation, given operator syntax —
the Ruby/JS `x || default` and Swift/Kotlin `x ?? default` / `x ?: default`
ergonomics, for the many stdlib functions that return `Option` (`dict.get`,
`list.first`/`last`, `string.parse_int`, …). Today `||` requires both operands to
share a type, so `d.get(k) || 0` is a **type error**; you must write
`d.get(k).unwrap_or(0)` or `d.get_or(k, 0)`.

## Motivation

`||` is already witchy's *truthy fallback* (not just boolean-or): for same-typed
operands it returns the left when truthy, else the right, where falsy is `""` /
`None` / `[]`. So `name || "anon"` (String) and `cfg || fallback` (both `Option`)
work. The natural next expectation — coming from Ruby, JS, Swift — is:

```
let port = config.get("port") || 8080      // Int, defaulting to 8080
let who  = req.headers.get("user") || "anon" // String
```

But `config.get("port")` is `Option(Int)` and `8080` is `Int`, so `||`'s
"operands share a type" rule rejects it. The workaround,
`config.get("port").unwrap_or(8080)`, is fine but the `|| default` form is what
people reach for, repeatedly. Since `||` *already means* "use the right when the
left is falsy," and `None` is already a falsy value, having it also unwrap the
surviving `Some` is the intuitive completion of the feature.

## Current behavior

- Typecheck ([`crates/witchy-types/src/typeck.rs`](../crates/witchy-types/src/typeck.rs), the `Or` arm): `unify(lt, rt)` —
  forces both operands to one type — then requires that type be
  `Bool` / `String` / `List` / `Option`. Result type = that operand type.
- Lowering: `||` stays a runtime `BinOp::Or`. The interpreter checks
  `value_truthy` (falsy = `""` / `[]` / `None`) and returns the left value if
  truthy, else the right. The compiled backend matches.

So `||` is **type-erased at runtime**: it inspects the value, never the static
type.

## Proposed semantics

Add one typing case on top of the existing homogeneous rule:

- If `lt` resolves to `Option(T)` and `rt` unifies with `T` (and `rt` is **not**
  itself `Option(T)`), then `lt || rt : T`.
- Every existing case is unchanged: `Bool||Bool`, `String||String` (with `""`
  falsy), `List||List` (with `[]` falsy), and `Option(T)||Option(T) -> Option(T)`.

**Disambiguation is by the RHS type**, which is unambiguous once known:

| Expression | Result |
|---|---|
| `opt \|\| 0` (RHS `Int`) | `Int` — unwrap |
| `opt \|\| Some(0)` (RHS `Option(Int)`) | `Option(Int)` — homogeneous, no unwrap |
| `opt \|\| None` | `Option(Int)` — homogeneous |

**Falsiness for the unwrap case is `None` only.** `Some(x)` is always present, so
`Some("") || "x"` is `""`, not `"x"` — *absence* (`None`) and *emptiness* (`""`)
stay distinct. This matches `unwrap_or` exactly and avoids the JS
`0 || fallback` / `"" || fallback` footgun where a legitimate empty value is
silently replaced. (The homogeneous `"" || "x"` still yields `"x"`, because there
the value genuinely *is* the empty string, not a present-but-empty `Some`.)

The RHS stays **lazy**: evaluated only when the left is `None`.

### Examples (illustrative, not executed)

```
let port = config.get("port") || 8080        // Int
let first = xs.first() || -1                  // Int (list.first -> Option)
let who   = headers.get("user") || "anon"     // String

// unchanged behavior:
"" || "default"                               // String -> "default"
cfg || fallback                               // Option(T) || Option(T) -> Option(T)
Some("") || "x"                               // String -> ""   (present wins)
```

## Implementation

This is a **type-directed desugar**, not a runtime tweak, because the runtime
`||` cannot distinguish `Option(T)||Option(T)` (keep the Option) from
`Option(T)||T` (unwrap it) — both have an `Option` value on the left.

1. **Typecheck** (`typeck.rs`, `Or` arm): before the homogeneous `unify(lt, rt)`,
   check whether `lt` resolves to `Option(inner)` and `rt` unifies with `inner`
   (but not with `Option(inner)`). If so, the result is `inner` and the node is
   marked as the *unwrap* form. Otherwise fall through to today's homogeneous
   rule.
2. **Desugar** (a typed elaboration step): rewrite an unwrap-form `a || b` to
   `match a: Some(x) -> x; None -> b`. This preserves laziness and needs **no new
   runtime op** — both backends already compile `match`. The homogeneous form
   keeps lowering to `BinOp::Or` as today.
3. **Inference caveat**: when `rt` is an unresolved type variable, the homogeneous
   and unwrap rules are ambiguous. Resolve conservatively — take the unwrap branch
   only when `lt` is concretely `Option(_)` **and** `rt` is concretely not an
   `Option` of the same inner; otherwise default to the homogeneous rule (today's
   behavior). Document that a rare ambiguous case may need an ascription.
4. **Parity test** (`crates/.../example_tests.rs`): differential coverage for
   `Some(x) || d`, `None || d`, `Some("") || d`, plus the homogeneous
   `Option||Option` case, so the interpreter and compiled backend agree.
5. **Docs**: update [`spec/language.md`](../spec/language.md) §4 (the `||` row + the falsy-fallback
   paragraph) and the relevant book chapter once implemented.

## Alternatives considered

- **Status quo + `unwrap_or` / `get_or`.** What exists today. Works, but the
  `|| default` form is what users reach for, and `||` already reads as a falsy
  fallback — so the gap is ergonomic friction, not a missing capability.
- **A separate `??` operator (JS/Swift nullish).** Rejected: witchy already
  overloads `||` as the truthy fallback; adding `??` would split one concept into
  two operators. Reusing `||` keeps the surface small.
- **Deep falsiness — `Some("")` falls back to the default.** Rejected: it
  conflates *absence* with *emptiness* and reintroduces the JS footgun. `None`-only
  falsiness for the unwrap case is the safe choice.

## Drawbacks

- `||` becomes the one operator whose **result type can differ from its operand
  types** (`Option(T) -> T`). That is a small irregularity, justified by the
  ergonomic win and the RHS-directed disambiguation.
- The inference ambiguity edge case (unresolved RHS) needs the conservative rule
  above and a documented ascription escape hatch.
- Adds a typed desugar step for one operator.

## Rollout

`proposed`. Land **after** the compiler workspace refactor ([RFC-0018](./0018-compiler-architecture.md)) settles, to
avoid editing `typeck.rs` / both lowerings while they are in motion. Sibling
ergonomic proposal: index-assignment sugar (`d[k] = v`, `xs[i] = v`) over the same
value-semantic, in-place-optimized collections — to be captured in its own RFC.
