---
rfc: 0180
title: "Error sets: composing and reducing typed errors"
status: proposed
created: 2026-10-08
tracking: >
  Typed remainder for anonymous-union error patterns, so handling a tag removes
  it from the error set; the union as the one error mechanism, deleting
  `From`-directed conversion at `?` and std's error `From` impls; and the
  decision that `? "msg"` stays the String-error tool.
---

# RFC-0180: Error sets

## Summary

witchy has two typed-error mechanisms that were designed years apart and never
reconciled. Retired RFC-0054 (implemented 2026-07-12, body at
`git show ebcef6140~1:rfcs/0054-structured-errors.md`) gave libraries nominal
error enums, the std `Error: Show` bound, and `?` that inserts a `From(Ein) for
Eout` conversion. The structural tier of RFC-0167 later added anonymous tagged
unions `.[A | B(Int)]` and one widening rule: `.[A | B]` flows into
`.[A | B | C]` at argument, return/tail, and `?` sites with no wrapper
(`anon_union_widening_ok` in `crates/witchy-types/src/typeck.rs`).

Together these cover *composition*: a function can say "I fail with A or B"
without declaring a new enum, and a caller can propagate that into a wider
set. What neither covers is *reduction*: handling one tag does not remove it
from the type. After a caller recovers from `.NotFound`, its signature still
advertises `.NotFound`, or the caller hand-writes a smaller union and re-injects
every remaining tag. That is the gap the surrounding ecosystem has converged on
closing (Zig error sets; the Rust `eros` crate's `recover` which subtracts a
type from an error union and `into_value` which compiles only when the set is
empty).

This RFC adds exactly one mechanism, **typed remainder in `match`**, records two
decisions about the existing mechanisms, and repairs the spec's dangling
reference to RFC-0054.

## Motivation

The useful distinction for an error type is whether a caller needs to **do
something different based on which error occurred**. If it does, the error
must be matchable, and the signature should say which errors are still
possible *after* the caller's own recovery policy has run. A signature that
lists errors a function cannot return is the same defect as a `String` error:
the caller cannot trust the type.

Today, with `type LoadErr = .[NotFound | BadPort(Int) | Missing(String)]`:

```sh
fn port_or_default(kind: Int) -> Result(Int, .[BadPort(Int) | Missing(String)]):
    match load(kind):
        Ok(p) -> Ok(p)
        Err(.NotFound) -> Ok(8080)
        Err(.BadPort(p)) -> Err(.BadPort(p))      // re-injection, by hand
        Err(.Missing(k)) -> Err(.Missing(k))      // re-injection, by hand
```

Every tag the policy does not handle must be named and rebuilt, so adding a
tag to `LoadErr` is a change to every caller that narrows it. The intent is
two lines: handle `.NotFound`, propagate the rest.

## Design

### Typed remainder in `match`

In a `match` whose scrutinee has anonymous-union type `.[T1 | ... | Tn]`, a
variable pattern or `_` that follows arms binding a subset of the tags has the
type of the **remaining** union. This applies through `Err(...)`, `Some(...)`,
and tuple/record positions, because patterns are one grammar.

```sh
fn port_or_default(kind: Int) -> Result(Int, .[BadPort(Int) | Missing(String)]):
    match load(kind):
        Ok(p) -> Ok(p)
        Err(.NotFound) -> Ok(8080)
        Err(rest) -> Err(rest)   // rest: .[BadPort(Int) | Missing(String)]
```

Rules:

- The remainder is computed from the tags the earlier arms **fully** cover.
  An arm `Err(.BadPort(0))` covers nothing (its payload is refutable); an arm
  `Err(.BadPort(_))` or `Err(.BadPort(p))` covers `BadPort`. Or-patterns
  cover each alternative. Guards (`if`) cover nothing.
- A remainder with one tag is still a union of one tag, not the payload.
- A remainder with **zero** tags is an unreachable arm, and `match` already
  rejects unreachable arms. So "every error has been handled" is expressed by
  omitting the catch-all, and the exhaustiveness checker confirms it. No
  `into_value` is needed: a `match` with no `Err` arm left is one.
- Widening then carries the remainder out: `Err(rest)` where the function
  returns a wider union is already admitted by `anon_union_widening_ok`.
  Implicit narrowing is still rejected everywhere else, as RFC-0167 requires.
  The remainder is not narrowing: it is the exhaustiveness judgment made
  visible as a type.
- Nominal enums get nothing new. Their arms are already exhaustive and their
  catch-all keeps the nominal type, as today. Reduction is a property of the
  structural tier only.

This is one typeck rule on the existing exhaustiveness walk. There is no new
runtime representation: the remainder shares the representation of its parent
union, which is why widening is representation-preserving and why the
remainder value can be returned without being rebuilt.

### Decision: one error mechanism, the union

Error types are anonymous unions, named by alias when a library needs a name:

```sh
type HttpError = .[Timeout(Duration) | Status(Int) | BadUrl(String)]

fn get(u: String) -> Result(Body, HttpError):
    let url = url.parse(u)?      // .[BadUrl(String)] widens into HttpError
    ...
```

`?` has exactly one typed-error rule, union widening. The `From`-directed
conversion at `?` that RFC-0054 added is **deleted**, together with every std
`impl From(E1) for E2` whose job was to let `?` nest one module's error enum
inside another's (26 on 2026-10-08). That nesting is the pattern RFC-0054
inherited from Rust and this RFC rejects: an I/O failure that can reach a
caller through three different variants is awkward to handle at exactly the
level that wants to handle it. A function that fails in two modules' ways
returns the union of their tags, the caller sees both, and the remainder rule
lets it peel one off.

Std's trust-boundary error types (`JwtError`, `DeserializeError`,
`EncodingError`, `HttpError`, `SemverError`, and the rest) become union
aliases. Their tags and payloads are unchanged; only the declaration form and
the conversions go. `Show` for an error renders through the union's tags, so
the std `Error: Show` bound keeps its one job, letting generic code and the CLI
print any error. A nominal `type X:` declaration is still a legal `e` in
`Result(T, e)`, since it is an ordinary value, but it gets no conversion at `?`
and the spec stops recommending it for errors.

The nominal-enum-plus-`From` and union-widening paths both existed because
they were designed years apart. Keeping both would give the language two ways
to say the same thing, with the worse one carrying std. Break, don't deprecate:
the std migration is one cut.

### Decision: `? "msg"` stays the String tool

RFC-0054 item 3 proposed that `e? "msg"` on a typed error should wrap through
`show` with the message prepended, making one operator serve typed and
application code. It was never implemented (`try_context.rs` rewrites only
`Option` and `Result(T, String)` operands) and this RFC decides against it.
Collapsing a matchable set to a `String` inside a propagation operator is the
exact failure mode typed errors exist to prevent: the type disappears at the
one site nobody reads. On a union-error or nominal-error `Result`, `? "msg"`
is a type error whose diagnostic names the two intended forms: `?` alone to
propagate the typed error, or `result.map_err` into an explicit
`.Context(String, E)` style tag when operational context must travel with the
set. The spec's text already says this; the forward reference to RFC-0054 is
replaced with a reference to this section.

## Acceptance criteria

Each is an independent expected result, exercised in admitted unoptimized Wasm,
with the type-level claims pinned by `witchy check` diagnostics.

1. **Remainder typing.** A `match` on `Result(Int, .[A | B(Int) | C(String)])`
   with arms for `Err(.A)` and `Err(rest) -> Err(rest)` type-checks against a
   return type `Result(Int, .[B(Int) | C(String)])`, and against the original
   wider type (widening). The remainder relayed into a wider set produces the
   independently computed payload.
2. **Full coverage needs no catch-all.** The same scrutinee with arms for all
   three tags and no `Err(_)` arm is accepted; adding `Err(_)` after them is
   rejected as unreachable, with the existing diagnostic.
3. **Refutable payloads cover nothing.** `Err(.B(0))` followed by `Err(rest)`
   types `rest` as the full three-tag union.
4. **Or-patterns and guards.** `Err(.A | .B(_))` removes two tags; a guarded
   arm removes none. Both pinned by the type of `rest` through a return
   annotation that would otherwise fail.
5. **Nested positions.** The remainder applies to a union inside a tuple
   scrutinee and inside `Some(...)`.
6. **No change for nominal enums.** A catch-all after some arms of a nominal
   error enum still has the nominal type; the fixture that today passes keeps
   passing.
7. **Contextual try refuses typed operands.** `e? "msg"` where `e` is
   `Result(T, .[A | B])` or `Result(T, HttpError)` is a type error whose
   message names `?` and `result.map_err`.
8. **Spec.** `spec/language.md`'s typed-error paragraph references this RFC,
   its executed example demonstrates the remainder, and no `RFC-0054`
   forward reference remains in `spec/`.
9. **No conversion at `?`.** `?` on `Result(T, E1)` inside a function
   returning `Result(U, E2)`, where `E1` is a nominal type with an
   `impl From(E1) for E2`, is a type error naming union widening as the
   typed-error rule. The `From`-insertion code in typeck is gone, not gated.
10. **Std on unions.** No `impl From(...) for <std error type>` remains in
    `std/`, every std error type is a union alias, and the existing std
    differential tests for those modules pass unchanged except for the
    declaration form.

The source tree walker is not acceptance evidence (RFC-0167).

## Non-goals

- A `recover(tag, f)` std combinator. Its signature needs type-level tag
  subtraction, which the language cannot express in a `fn` signature, and the
  `match` remainder is the general form of which it would be sugar.
- Inferred error sets in return position (Zig's `!T`). Return types stay
  explicit; the compiler tells you what to write.
- Migrating std's `Result(T, String)` APIs to typed errors. RFC-0054 settled
  which std surfaces are typed; this RFC changes the form of the typed ones,
  not the set.
- Error cause chains (`source()`). Not requested by any witchy program yet.

## Drawbacks

A pattern variable's type now depends on the arms above it. That is already
true of exhaustiveness, and the type error that results from reordering arms
is local and names the remainder, but it is one more thing the reader must
know about `match`.

## Prior art

Zig error sets (`||` composition, inferred sets, exhaustive `switch` with
`else` capturing the remainder). The Rust `eros` crate (`ErrorUnion<(A, B)>`,
`.union()?` widening, `recover::<A>` subtraction, `into_value` on the empty
set), described in "The Missing Piece in Rust Error Handling" (2026-10-06).
witchy already has the first two of those three pieces as language rules; this
RFC adds the third as a `match` rule rather than a method, because witchy has
one pattern grammar and exhaustiveness is already its judgment of coverage.
