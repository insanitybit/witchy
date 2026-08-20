---
rfc: 0130
title: "Generators and the lazy iterator protocol"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical generator RFC. Acceptance rows 1-7 are PROVEN by the no-replay owned-frame mutation, control-flow, one-time-effect, and scaling suites; adapter/backend parity; the source-diagnostic matrix; syntax/reflection/tooling preservation tests; and clean installed-archive generator smoke named below."
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

The generator model is a resumable owned frame. Pulling the next element
continues from the previous suspension exactly once. Lowering never re-runs the
body from its beginning to locate the kth `yield`; a residual suspension CFG
fails at its source declaration instead of receiving replay semantics.

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

### Supported frame catalog

The supported source catalog includes finite direct yields; conditional and
`match` yields; one terminal `while`, `while let`, or list `for`; early return;
outer-loop `break` and `continue`; a finite prefix before a terminal loop; and
one finite tail after a yielding loop. Entry effects, inferred direct-call
results, destructured bindings, and locals used after a yield are carried by the
owned frame and execute or initialize once.

A nested yielding loop is not yet one frame in this catalog. Compose it from a
separate inner generator. If lowering sees another residual suspension CFG, it
reports the `gen fn` name and line rather than changing effect or complexity
semantics.

## Ownership boundaries

`Iter(T)` yields owned `T` values. It is not a lending iterator. A reference may
not enter a generator frame or appear in its yielded element unless a later
lending-iterator RFC defines the required frame/owner relation.

Capabilities may be captured by accepted generator shapes because owned-frame
lowering guarantees one-time segment execution. No accepted generator lowering
falls back to replay.

## Complexity contract

A successfully lowered sequential traversal performs amortized O(1)
generator-state work
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

Rows 1-3 are promoted by the owned-frame implementation and its executable
mutation, control-flow, effect-count, frame-shape, and scaling evidence.

| row | status | executable evidence |
| --- | --- | --- |
| 1 mutation and control flow | **PROVEN** | `rfc0130_generator_owned_frame::rfc0130_rows_1_to_3_generators_resume_owned_frames_once_with_linear_work_on_wasm` exercises mutable top-level and inherent generators with loop and branch control on compiled Wasm. `rfc0130_generator_liveness`, `rfc0130_generator_nested_cfg`, `rfc0130_generator_match_cfg`, `rfc0130_generator_match_binding`, and `rfc0130_generator_while_let_cfg` preserve live locals and branch, match, and `while let` control flow across suspension on both backends. |
| 2 one-time body segments and effects | **PROVEN** | The rows 1-3 fixture checks exact pre-yield capability output for every yielded item, including exhaustion. `generator_owned_frame_delays_post_yield_effect_until_resume`, `generator_direct_multi_yield_loop_resumes_each_segment_once`, and the paired CFG fixtures prove that post-yield, multi-phase, conditional, match, and loop effects resume once without replay. |
| 3 linear pulls and bounded frame storage | **PROVEN** | `rfc0130_generator_scaling::rfc0130_acceptance_row_3_owned_frame_pulls_are_linear_and_storage_is_bounded_on_wasm` compares 8 and 64 compiled-Wasm pulls and pins a fixed direct five-lane lazy-entry carrier. The rows 1-3 fixture independently checks fixed carrier dimensions and live cells, deterministic counters, exactly linear effect count, and no pull-dependent RC, bump-allocation, or reown calls. |
| 4 source diagnostics | **PROVEN** | `rfc0130_surface::rfc0130_row_4_generator_failures_name_source_syntax` covers returned values, yielded-value mismatch, borrowed frame/element types, region escape, unsupported trait syntax, and rejection of generated helper names. `generators::target_availability_tests::residual_suspension_cfg_fails_closed_instead_of_replaying` pins the source-located residual-CFG boundary and proves replay is not offered as a fallback. |
| 5 adapters and collection parity | **PROVEN** | `rfc0130_adapter_collection::rfc0130_row_5_iterator_adapters_and_from_iterator_collections_agree_on_both_backends` composes representative lazy adapters, selects `List`, `Dict`, `Set`, and `String` collection through expected-type `FromIterator` dispatch, bounds an infinite adapter pipeline, and short-circuits an infinite `find` identically on the interpreter and compiled Wasm. |
| 6 syntax preservation | **PROVEN** | `rfc0130_surface::rfc0130_row_6_formatter_reflection_docs_and_editor_surfaces_preserve_generators`, `web/witchy-runtime/witchy-highlight.test.mjs`, and the Zed extension's pinned Tree-sitter revision preserve `gen fn`/`yield` through formatting, AST and quoted-item reflection, generated docs, and editor highlighting. |
| 7 clean installed example | **PROVEN** | `scripts/release-smoke.sh`, invoked by the release workflow on every supported native archive, creates a fresh project through the installed public CLI and executes both `finite(4)` and `naturals().take(5)` with an isolated HOME, cache, working directory, and PATH containing only the extracted toolchain. |
