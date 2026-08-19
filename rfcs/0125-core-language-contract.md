---
rfc: 0125
title: "The canonical Witchy language contract"
status: accepted
created: 2026-08-19
superseded-by:
tracking: "Canonical consolidation RFC. The shipped core is supported preview; RFC-0123 expression-boundary and explicit-discard acceptance remains the only open source-grammar cut in this contract. Historical predecessor RFCs remain frozen evidence."
predecessors:
  - "[0021](0021-or-unwrap-option.md), [0048](0048-fallback-operator.md) (fallback and Option extraction)"
  - "[0022](0022-index-assignment.md), [0043](0043-declared-mutation-writeback.md) (places and declared mutation)"
  - "[0042](0042-module-namespaces.md), [0050](0050-method-call-generalization.md), [0056](0056-keyword-arguments.md), [0113](0113-full-keyword-arguments.md) (modules and calls)"
  - "[0046](0046-typed-trait-dispatch.md), [0081](0081-existential-trait-values.md) (traits and dispatch)"
  - "[0045](0045-compiled-trap-diagnostics.md), [0062](0062-closure-escape-elision.md) (diagnostics and closure execution semantics)"
  - "[0052](0052-one-pattern-grammar.md), [0078](0078-anonymous-tagged-unions.md), [0097](0097-unit-type-as-empty-tuple.md), [0098](0098-structural-record-width.md) (data and patterns)"
  - "[0054](0054-structured-errors.md), [0065](0065-sealed-type-constructors.md) (errors and invariant-bearing types)"
  - "[0123](0123-expression-boundaries.md) (expression boundaries and explicit discard)"
related:
  - "[0084](0084-scoped-extensions-and-interception.md) (deferred extension mechanism, outside the core baseline)"
---

# RFC-0125: The canonical Witchy language contract

## Decision

Witchy keeps one compact, statically typed, expression-oriented language as its
normal mode. Records, tagged unions, structural records and unions, pattern
matching, modules, generics, traits, closures, explicit errors, and
`let`/`var`/`own` parameter conventions are the permanent core.

The historical RFCs above describe how individual pieces arrived. This RFC is
the canonical decision map for the resulting language. Current syntax and
semantics remain authoritative in [`spec/language.md`](../spec/language.md).

"Core" means retained and compatibility-governed. It does not mean every
advanced feature appears in the first tutorial. Capabilities, opt-mode
references, regions, concurrency, generators, metaprogramming, and `Dynamic`
have their own capstone RFCs because each carries a distinct semantic contract.

## Canonical surface

### Data and types

- Scalars are `Int`, `Float`, `Bool`, `String`, `Bytes`, `Duration`, and `()`.
- Product data uses tuples, nominal records, and anonymous structural records.
- Sum data uses nominal tagged unions and anonymous structural unions.
- `List`, `Dict`, `Set`, `Option`, and `Result` are ordinary generic types with
  standard-library protocols rather than compiler-private shadow types.
- `sealed type` makes construction the single enforcement point for a value
  invariant.

### Functions and calls

- Functions are values. Direct calls, methods, closures, trait calls, and
  indirect function values retain one checked function-type identity.
- `let`, `var`, and `own` state call behavior. They are not alternate type
  syntaxes.
- Arguments evaluate left-to-right as written. Labels reorder binding, not
  evaluation.
- Instance methods declare `self`; module functions remain first-class and may
  participate in method syntax only through the type-owned method rules.

### Control flow and failure

- Blocks are expressions and their final expression is their value.
- `if`, `match`, loops, comprehensions, `?`, `??`, and explicit `return` retain
  one type-checked control-flow model.
- Patterns use one grammar. Context determines whether a refutable pattern is
  legal.
- Recoverable failure uses `Option` or `Result`; traps are reserved for broken
  invariants and operations whose documented contract is aborting.

### Modules, traits, and abstraction

- Modules and Python-style imports define namespaces and visibility.
- Traits provide static generic constraints and explicit existential values.
- Dispatch is resolved from checked types and authenticated witness identity,
  never reconstructed from rendered type strings.
- Structural conformance is explicit and width-safe; nominal invariants remain
  nominal.

## Ordinary mode stays ordinary

Normal Witchy does not expose lifetime or reference syntax. It presents value
semantics and conventional `let`/`var`/`own` calls. The compiler may use loans,
uniqueness, regions, and references internally, but a missed optimization proof
cannot reject normal source. Advanced source-level access contracts are entered
explicitly with `mode opt` under RFC-0127.

## Open source-grammar cut

RFC-0123 is folded into this canonical contract. It completes three related
rules:

1. layout produces an unambiguous statement boundary;
2. written `;` means explicit expression discard; and
3. discarded collection results may select a result-free lowering without
   changing used-result behavior.

Until its acceptance matrix is green, this RFC remains accepted rather than
implemented even though the rest of the core is shipped.

## Acceptance

The canonical contract is implemented when:

1. every syntax form above has parser, formatter, checker, and documentation
   coverage;
2. every runnable semantic form agrees on interpreter and compiled Wasm;
3. invalid calls, patterns, trait uses, control flow, and discarded results fail
   before backend-specific lowering;
4. RFC-0123's layout, `;`, editor grammar, and discard-lowering rows are proven;
5. the installed binary can check, format, test, run, and compile the curated
   core examples; and
6. `PRODUCT-STATUS.md` names this exact supported-preview boundary.

## Historical status

This RFC consolidates navigation and the long-term promise. It does not erase
the predecessor RFCs, their alternatives, migration reports, or acceptance
evidence. A future change to one core decision receives a new RFC that names
this one as its predecessor.
