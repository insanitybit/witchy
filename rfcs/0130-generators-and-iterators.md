---
rfc: 0130
title: "Generators and the lazy iterator protocol"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical generator RFC. Acceptance rows 4, 6, and 7 are PROVEN by the source-diagnostic matrix, syntax/reflection/tooling preservation tests, and clean installed-archive generator smoke below. Rows 1-3 remain unadjudicated here: Iter and gen syntax are shipped with parity, but the replay-to-kth-yield lowering is not the final core contract. Promotion still requires an owned resumable frame or a statically enforced pure/replay-safe subset with an explicit complexity contract."
predecessors:
  - "[0052](0052-one-pattern-grammar.md) (loop and binding patterns)"
  - "[0059](0059-state-machine-async.md) (shared resumable-state destination)"
  - "[0074](0074-container-api-symmetry.md) (collection and iterator API coherence)"
related:
  - "[0129](0129-concurrency-tasks-and-channels.md) (owned resumable frames)"
  - "[0133](0133-standard-library-contract.md) (FromIterator and collection protocols)"
---

# RFC-0130: Generators and the lazy iterator protocol

## Decision

`Iter(T)`, lazy combinators, `gen fn`, and `yield` are permanent Witchy
features. They provide a lazy owned-value sequence without requiring users to
construct state-transition closures by hand.

The final generator model is a resumable owned frame. Pulling the next element
continues from the previous suspension exactly once. Re-running the body from
its beginning to locate the kth `yield` is retained only as an implementation
checkpoint, not as the promoted semantic or performance contract.

## Iterator protocol

`Iter(T)` is an ordinary standard-library type with a public pull operation and
lazy adapters including `map`, `filter`, `take`, `zip`, `chain`, `flat_map`,
`enumerate`, `find`, `any`, and `all`. Consumers include `fold`, `count`,
`for_each`, and `collect`.

`collect` is selected by the expected result type through `FromIterator`.
Lists, dictionaries, sets, strings, and user types may define deliberate
collection behavior without compiler-private cases.

## Generator source model

```witchy
gen fn fibs() -> Iter(Int):
    var a = 0
    var b = 1
    while true:
        yield a
        let next = a + b
        a = b
        b = next
```

- A bare `return` ends the sequence.
- Returning a value from a generator is rejected unless a future protocol adds
  an explicit terminal-result type.
- Locals live across `yield` are owned frame fields.
- Each effect before a `yield` executes once for one traversal step.
- A generator may be a top-level function or inherent method.
- Trait APIs express generation as an ordinary method returning `Iter(T)`.

## Ownership boundaries

`Iter(T)` yields owned `T` values. It is not a lending iterator. A reference may
not enter a generator frame or appear in its yielded element unless a later
lending-iterator RFC defines the required frame/owner relation.

Capabilities may be captured only when the stateful frame guarantees one-time
execution. Under the current replay implementation, capability-bearing
generators must be rejected or explicitly classified as experimental because
replay repeats effects.

## Complexity contract

A promoted sequential traversal performs amortized O(1) generator-state work
per pull, excluding the user's body and deliberate iterator adapters. Collecting
n generated values must not become O(n^2) solely because lowering replays the
first k yields for every element.

Infinite generators remain safe when a short-circuiting consumer bounds pulls.
Cancellation drops the owned frame and its captured values.

## Acceptance

1. Top-level and inherent generators preserve mutation and control flow across
   suspension.
2. Pulling a sequence runs each body segment and capability effect once.
3. n sequential pulls have linear lowering overhead and bounded live frame
   storage.
4. `return`, invalid yielded types, borrowed-frame escape, region escape, and
   unsupported trait syntax receive source-level diagnostics.
5. Iterator adapters and `FromIterator` collection agree on interpreter and
   compiled Wasm, including infinite short-circuit cases.
6. Formatter, reflection, docs, and editor grammar preserve generator syntax.
7. A clean installed example demonstrates a finite and bounded-infinite
   generator without relying on repository internals.

### Acceptance evidence

Rows 1-3 are intentionally not promoted by this surface slice. Their authority
is the owned-frame implementation and complexity evidence, not the existence of
generator syntax or examples.

| row | status | executable evidence |
| --- | --- | --- |
| 1 mutation and control flow | UNADJUDICATED | Lowering work owns this row; the surface suite makes no completion claim. |
| 2 one-time body segments and effects | UNADJUDICATED | Lowering/runtime work owns this row; the surface suite makes no completion claim. |
| 3 linear pulls and bounded frame storage | UNADJUDICATED | Lowering/performance work owns this row; the surface suite makes no completion claim. |
| 4 source diagnostics | **PROVEN** | `rfc0130_surface::rfc0130_row_4_generator_failures_name_source_syntax` covers returned values, yielded-value mismatch, borrowed frame/element types, region escape, unsupported trait syntax, and rejection of generated helper names. |
| 5 adapters and collection parity | UNADJUDICATED | This slice does not reclassify the existing iterator parity corpus. |
| 6 syntax preservation | **PROVEN** | `rfc0130_surface::rfc0130_row_6_formatter_reflection_docs_and_editor_surfaces_preserve_generators`, `web/witchy-runtime/witchy-highlight.test.mjs`, and the Zed extension's pinned Tree-sitter revision preserve `gen fn`/`yield` through formatting, AST and quoted-item reflection, generated docs, and editor highlighting. |
| 7 clean installed example | **PROVEN** | `scripts/release-smoke.sh`, invoked by the release workflow on every supported native archive, creates a fresh project through the installed public CLI and executes both `finite(4)` and `naturals().take(5)` with an isolated HOME, cache, working directory, and PATH containing only the extracted toolchain. |
