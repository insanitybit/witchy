---
rfc: 0048
title: "A dedicated ?? fallback; || returns to Bool"
status: proposed
created: 2026-07-03
predecessors:
  - "0021 (Option(T) || T unwrap — the role ?? takes over)"
tracking:
---

# RFC-0048: A dedicated `??` fallback; `||` returns to Bool

> Provisional syntax throughout. Code blocks are intentionally **not** tagged
> `witchy` so the doc-examples sweep does not compile pre-implementation
> snippets.

## Summary

`||` is currently three operators in one spelling: short-circuit logical-or on
Bool, a same-typed *truthy fallback* on String/Option/List (falsy = `""` /
`None` / `[]`), and the RFC-0021 `Option(T) || T` unwrap
(spec/language.md:196). This RFC splits the roles: `||` becomes **Bool-only
logical-or**, and a new `??` operator is **the** fallback —
`Option(T) ?? T -> T` (None-coalescing) and `Result(T, e) ?? T -> T`
(Err-coalescing). Truthiness leaves the language entirely: an empty string or
empty list is data, not falsehood. Chaining (`a ?? b ?? c`) works
right-associatively, with fallbacks evaluated lazily.

## Motivation

All claims re-probed against the shipped binary, 2026-07-03.

**One spelling, three meanings, and a real ambiguity.** With nested Options
the truthy-fallback reading and the unwrap reading *both* plausibly apply, and
the user gets neither an answer nor a useful error:

```
let x: Option(Option(Int)) = Some(None)
let y = x || Some(1)
// truthy reading:  x is Some(_) = truthy → y = Some(None) : Option(Option(Int))
// unwrap reading:  Some(None) || Some(1) → None : Option(Int)
// actual: type error: `main`, line 3: expected `Option(Int)`, found `Int`
```

The error names types the user never wrote, because the unwrap rewrite
(traits.rs:739, which fires only when the left is "concretely Option" and the
right "concretely not") and the homogeneous truthy rule (typeck.rs:2733) are
fighting over the same token. An operator whose meaning depends on a
best-effort type recovery is a diagnostic dead end by design.

**The truthy set is arbitrary.** Bool, String, Option, and List are falsy-able;
`Dict` is excluded (probed: ``` `||` needs Bool, String, Option, or List
operands ``` on a `Dict` — the error's own list is the arbitrariness). No rule
generates this set; it's an allowlist someone must memorize.

**The language's two sum types diverge on their most idiomatic operator.**
`Option(T) || T` unwraps; `Result(T, e) || T` is a type error (probed:
`expected Result(Int, String), found Int`). Every "value or default" call site
over a Result must detour through `result.unwrap_or` while the Option
spelling is an operator — for the identical concept.

**Truthiness is the least witchy-shaped idea in the surface.** It was imported
from neither parent — Rust has no truthiness, and Go has none either; witchy's
own doctrines (mixed Int/Float arithmetic banned, no implicit coercions
anywhere, absence-is-Option) all point the same direction: *emptiness is not
falsehood*. `"" || "anon"` treats a legitimate value as a missing one; the
JavaScript ecosystem spent a decade rediscovering why that's a bug class
(`0 || default`, `"" || default`) and added `??` specifically to escape it. We
would be pre-empting the exact failure JS had to patch.

## Design

### 1. `||` is Bool-only logical-or

`a || b` requires both operands `Bool`, short-circuits (rhs unevaluated when
lhs is `true`), yields `Bool`. Identical to today's Bool case; every other
operand type is a compile error whose message points at `??`:

```
error: `||` is logical-or on Bool. For a fallback value use `??`:
       `name ?? "anon"` (Option), `parse(s) ?? 0` (Result).
```

`&&` is untouched. The truthy-fallback machinery — `value_truthy`
(crates/witchy-interp/src/interpreter.rs:2604), the compiled truthy lowering
(crates/witchy-lower/src/codegen/mod.rs:4395), the typeck allowlist
(crates/witchy-types/src/typeck.rs:2733–2748), and the RFC-0021 rewrite
(crates/witchy-types/src/traits.rs:739–780) — is **deleted**, not gated.

### 2. `??` — the fallback operator

```
opt ?? default          // Option(T) ?? T  -> T      None-coalescing
res ?? default          // Result(T,e) ?? T -> T     Err-coalescing (error discarded)
```

- **Typing rule**: lhs must be `Option(T)` or `Result(T, e)`; rhs is `T`; the
  expression is `T`. Nothing else is admissible — no truthiness, no
  same-typed String/List fallback, no `Option ?? Option` (that concept is
  `option.or`, which already exists; keeping `??` strictly *unwrapping* means
  its result type is never ambiguous, killing the `Some(None)` confusion
  class by construction: `x ?? Some(1)` on `Option(Option(Int))` has exactly
  one reading — `Option(Int)`).
- **Laziness**: the fallback is evaluated only on `None`/`Err` (same guarantee
  RFC-0021 gave; same lowering shape — desugar to a `match`, so both backends
  agree by construction and the interpreter/codegen need no new expression
  kind).
- **Chaining**: **right-associative** — `a ?? b ?? c` is `a ?? (b ?? c)`.
  This is what makes the natural chain type under the strict rule:
  `d.get(k1) ?? d.get(k2) ?? 0` groups as `d.get(k1) ?? (d.get(k2) ?? 0)`,
  where the inner `??` yields `Int` and the outer unwraps against it. (With
  left-associativity the parenthesized `(Option ?? Option)` would not type —
  C# and Swift make `??` right-associative for exactly this reason.) A
  mis-chain is an ordinary type error at the exact `??` that fails — never a
  semantic ambiguity.
- **Precedence**: one level looser than `||` (the loosest binary operator
  before ranges), so comparisons and arithmetic always bind tighter:
  `d.get(k) ?? n + 1` is `d.get(k) ?? (n + 1)`. (Lexer: `??` is a new
  greedily-lexed two-char token. The one adjacency to rule on: `e???d`
  (postfix `?` then infix `??`, unspaced) lexes as `?? ?` and fails to parse
  — `fmt`'s canonical spacing writes `e? ?? d`, and the parse error is
  immediate, not a silent regrouping.)
- **`Result` discards the error.** `res ?? d` is `unwrap_or`, not `or_else` —
  the error value is dropped, by design; when the error matters, `?`/`e? "msg"`
  and `match` remain the tools. The book gets one sentence drawing that line.

### 3. Truthiness is gone — explicit emptiness tests replace it

`name || "anon"` (String) and `xs || [0]` (List) have no `??` equivalent, on
purpose. The replacements are one honest conditional:

```
if name.is_empty(): "anon" else: name
if xs.is_empty(): [0] else: xs
```

If real corpora show this is a hot pattern, a later RFC can add
`string.or_empty(name, "anon")`-style helpers — data-level defaults belong in
the stdlib under [RFC-0044](0044-std-error-policy.md)'s policy ("nothing
silently defaults unless the name says so"), not in an operator.

### 4. Migration (one cut; the whole-corpus count)

Grepped 2026-07-03, fallback-form `||` (non-Bool operands) across the repo:

- **std/**: **0** — of the 47 `||` occurrences in std/*.witchy, every code
  occurrence is boolean (the rest are prose in doc-comments). std migrates by
  changing nothing.
- **examples/**: **0** — all 13 occurrences boolean.
- **projects/**: **1** — `req.uploaded_by || "anonymous"`
  (projects/coven/src/coven.witchy:328, a String truthy fallback) → an
  explicit `is_empty` conditional (or an Option-returning accessor, which is
  the RFC-0044-correct shape anyway).
- **book/**: 2 executable usages (book/src/tour-errors.md:65–66) plus the
  prose section teaching truthiness (:53–77) — rewritten to teach `??`.
- **spec/**: 3 executable usages (spec/language.md:217, :752–753) plus the
  operator-table row (:196) and the RFC-0021 section (:742) — rewritten.
- **tests**: 2 differential-test programs (src/example_tests.rs:1600,
  :10750–10754, ~11 usages between them) — become the `??` differential tests,
  plus new negative tests pinning the `||`-on-String error message.

Total: about a dozen call sites outside the tests that exist to test the old
behavior. The migration is an afternoon, and `witchy fmt` is not even needed
as a vehicle — the sites are enumerable by the type checker (every one becomes
a compile error naming the fix). This is the cheapest moment this split will
ever have.

RFC-0021 is superseded in its operator half (`??` takes the unwrap role) and
retained in its laziness/desugar design; mark it with a dated change-note on
acceptance. [RFC-0047](0047-one-equality.md) cleans the other overloaded
operator row of the same table; together the operator table becomes: every
operator has one type rule.

## Alternatives

- **Keep `||` overloaded, add Result to it.** Symmetric, minimal diff —
  rejected: it *adds* a fourth meaning to the existing three, deepens the
  `Some(None)` ambiguity (now `Result(Option(T))` too), and keeps truthiness.
- **`??` as same-typed fallback too** (`s ?? "anon"` for empty string, C#'s
  broader cousin). Rejected: reintroduces "which values count as absent" per
  type — that's truthiness with a new spelling. `??` meaning exactly
  "unwrap-or" keeps one typing rule, zero allowlists.
- **No operator; `unwrap_or` only** (Rust's position). Honest, but rejected:
  witchy explicitly optimizes for lightweight value-or-default at lookup sites
  (`d.get(k) ?? 0` is the dict idiom the book leads with), the operator
  precedent is already set by RFC-0021 and by `?`/`e? "msg"`, and method-form
  `unwrap_or` stays available for those who prefer it.
- **Spell it `or`** (Python keyword flavor). Rejected: `or` reads as
  logical-or and collides with `option.or`/`result.or` (Option-to-Option),
  which mean something different; `??` has no prior meaning to fight.
- **Do nothing.** The `Some(None)` ambiguity, the Option/Result asymmetry, and
  the Dict exclusion all stay; every future container type re-raises "is it
  truthy?" — a question the language should refuse to have.

## Drawbacks

- **Breaking**, including two spec programs and the book's error-tour — though
  the verified blast radius (§4) is roughly a dozen sites, zero of them in
  std. External pre-1.0 code with String/List truthy fallbacks breaks loudly
  with a teaching error.
- One more operator to learn — mitigated by it being the single most
  cross-language-recognizable operator addition of the last decade (C#, Swift,
  JS, Kotlin's `?:` cousin).
- The String/List empty-fallback idiom gets *longer* (an explicit
  conditional). This is the point — emptiness-as-absence was a trap — but it
  will be felt as a regression by anyone who liked `name || "anon"`.
- `??` discarding Result errors gives a compact spelling for silently
  swallowing failure. Accepted: `unwrap_or` already does; the operator names
  the intent at least as clearly.

## Prior art

- **JavaScript `??`** (ES2020) — added *because* `||`'s truthiness made
  `0 || d` / `"" || d` a chronic bug class; the committee's cure is the exact
  failure this RFC pre-empts. The precedent that `||`-fallback plus falsy
  values doesn't survive contact with real code.
- **C# `??`** (null-coalescing, the original) and **Swift `??`**
  (`Optional ?? T -> T` — precisely our typing rule, laziness included).
- **Rust `unwrap_or`/`unwrap_or_else`** — the method-form semantics `??`
  operator-izes; Rust's refusal of truthiness is the design north star here.
- **Kotlin `?:`** (Elvis) — same role; the `??` spelling avoids collision with
  witchy's `:`-heavy layout syntax.
- [RFC-0021](0021-or-unwrap-option.md) — the in-repo predecessor; its lazy
  match-desugar is reused verbatim under the new token.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the status/superseded-by fields, and appending
    dated change-notes below.
  - The current behavior lives in spec/ and the code — NOT here.
-->
