# BUG-503: `std/iter` docs still call shipped `gen`/`yield` syntax planned

Severity: LOW
Status: FIXED
Verified: 2026-07-09 fixed on master 040ce13b
Component: `std/iter`, generated stdlib docs, generator language surface, release docs polish

## Resolution

Current `std/iter` module prose and the generated stdlib reference describe
`gen fn` / `yield` as the shipped ergonomic surface over `Iter(a)`, not as
planned future syntax:

- `std/iter.witchy` says `gen fn`/`yield` lower to the iterator representation.
- `std/iter.witchy` documents `from_gen` as the low-level desugaring target for
  compiler-generated iterators.
- `spec/stdlib.md` no longer contains the stale "planned `gen`/`yield`"
  wording.

Focused verification:

```sh
rg -n 'planned.*gen|planned.*yield' std/iter.witchy spec/stdlib.md
```

No matches.

## Problem

Historical problem: `std/iter` described generator syntax as future work:

- `std/iter.witchy:1-9` introduces `Iter(a)` and says "The planned
  `gen`/`yield` syntax will de-sugar to these constructors."
- `spec/stdlib.md:859` publishes the same wording in the generated standard
  library reference.

But `gen fn` and `yield` were no longer planned. They were implemented and
release-facing:

- `crates/witchy-syntax/src/parser.rs:314-315` parses top-level
  `fn`/`gen fn`/`async fn` declarations.
- `crates/witchy-syntax/src/parser.rs:1107-1114` parses `yield` and rejects it
  outside a generator body.
- `crates/witchy-syntax/src/generators.rs:1-29` documents and implements
  lowering from `gen fn` / `yield` to `std/iter`.
- `spec/language.md:870-883` documents `gen fn` as current language syntax.
- `examples/generators/src/generators.witchy:1-33` ships a public example using
  `gen fn` and `yield`.

This was small, but visible. The iterator module is the canonical lazy sequence
API, and the docs currently make a shipped language feature sound aspirational.
That undercuts the release story around what is actually implemented.

## Distinct from nearby issues

- BUG-167 is about `witchy doc` dropping `async`/`gen` markers from rendered API
  signatures.
- BUG-306, BUG-428, and BUG-443 cover generator lowering semantics and hygiene.
- BUG-461 is about `iter.next` being documented as the pull primitive while
  remaining private.

This bug is only the stale `std/iter`/generated-reference wording that calls
current generator syntax "planned".

## Expected

Update the module prose to describe `gen fn` / `yield` as the current ergonomic
surface over `std/iter`, for example:

- `gen fn` / `yield` lowers to these constructors; and
- `iter.from_gen` is the low-level desugaring target, not an API users normally
  need for hand-written generators.

Regenerate `spec/stdlib.md` after the source comment changes.

## Acceptance

- `std/iter.witchy` no longer calls `gen`/`yield` planned.
- `spec/stdlib.md` no longer calls `gen`/`yield` planned.
- The wording aligns with `spec/language.md` and `examples/generators`.
