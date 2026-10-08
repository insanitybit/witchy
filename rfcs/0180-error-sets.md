---
rfc: 0180
title: "Error sets: composing and reducing typed errors"
status: proposed
created: 2026-10-08
tracking: >
  Typed remainder for anonymous-union error patterns, so handling a tag removes
  it from the error set; the decided boundary between `From`-converted nominal
  errors and widened anonymous unions; and the decision that `? "msg"` stays the
  String-error tool.
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

### Decision: nominal `From` at package boundaries, unions for local composition

Both mechanisms stay. The rule, now written down:

- A **nominal** error type (`type HttpError: ...` with `impl Error`) is the
  contract surface of a module that other packages depend on. It can carry
  `impl`s and invariants, and `?` converts into it through `From`. Std's
  trust-boundary APIs (`JwtError`, `DeserializeError`, `EncodingError`,
  `HttpError`, `SemverError`) stay nominal.
- An **anonymous union** is for composition that does not deserve a
  declaration: inside a module, across a few private helpers, or for a
  function whose distinct failure modes a caller branches on but no package
  needs to name.
- Std adds **no new `From` impls between std error types** whose only job is
  to let `?` nest one module's enum inside another's. That nesting is the
  pattern RFC-0054 inherited from Rust and this RFC rejects: an I/O failure
  that can reach a caller through three different variants is awkward to
  handle at exactly the level that wants to handle it. Where a std function
  genuinely fails in two modules' ways, it returns a union of the two nominal
  types, `.[Io(IoError) | Parse(ParseError)]`, so the caller sees both and
  the remainder rule lets it peel one off.

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

The source tree walker is not acceptance evidence (RFC-0167).

## Non-goals

- A `recover(tag, f)` std combinator. Its signature needs type-level tag
  subtraction, which the language cannot express in a `fn` signature, and the
  `match` remainder is the general form of which it would be sugar.
- Inferred error sets in return position (Zig's `!T`). Return types stay
  explicit; the compiler tells you what to write.
- Migrating std's `Result(T, String)` APIs. RFC-0054 settled which std
  surfaces are typed; this RFC only fixes how typed errors compose and reduce.
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
