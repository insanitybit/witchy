# Retired language RFCs

The 2026-09-16 consolidation removes 55 obsolete design bodies from the working
corpus. The single target language contract is [RFC-0167](0167-witchy-language-contract.md).
Its status is accepted; deletion of conflicting history is not compiler completion.

All removed files are recoverable at baseline commit
`418a69b9ca75ae70fae5e96f797057e161f93aeb`:

```sh
git show 418a69b9ca75ae70fae5e96f797057e161f93aeb:rfcs/0122-uniform-borrow-relations.md
```

Retired numbers are never reused. Existing source/test names may still contain
those IDs as historical identifiers. Retained companion ledgers record evidence
against their original commits and contracts, not current language authority.
Concurrency/generator evidence was moved into the new acceptance ledger.
Optimization mechanisms and active security/runtime evidence are retained;
removing a roadmap does not close its outstanding implementation obligations.

| Retired file | Replacement decision |
|---|---|
| `0006-compile-time-tagged-literals.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0021-or-unwrap-option.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0022-index-assignment.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0025-frozen-deep-immutability.md` | [Language contract](0167-witchy-language-contract.md#4-unique-and-frozen) |
| `0026-unique-qualifier.md` | [Language contract](0167-witchy-language-contract.md#4-unique-and-frozen) |
| `0028-ergonomic-mutable-value-semantics.md` | [Language contract](0167-witchy-language-contract.md#3-owned-values-and-calls) |
| `0029-performance-tier-contract.md` | [Language contract](0167-witchy-language-contract.md#6-normal-and-opt-interoperation) |
| `0042-module-namespaces.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0043-declared-mutation-writeback.md` | [Language contract](0167-witchy-language-contract.md#3-owned-values-and-calls) |
| `0044-std-error-policy.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0047-one-equality.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0048-fallback-operator.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0049-naming-lexicon.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0050-method-call-generalization.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0052-one-pattern-grammar.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0053-one-rendering.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0054-structured-errors.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0055-channel-message-types.md` | [Language contract](0167-witchy-language-contract.md#7-resources-effects-and-suspension) |
| `0056-keyword-arguments.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0064-complete-mutation-classification.md` | [Language contract](0167-witchy-language-contract.md#3-owned-values-and-calls) |
| `0065-sealed-type-constructors.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0069-structured-typeinfo.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0074-container-api-symmetry.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0078-anonymous-tagged-unions.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0080-structured-hygienic-metaprogramming.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0081-existential-trait-values.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0082-runtime-dynamic-values.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0083-opt-mode-lifetimes.md` | [Language contract](0167-witchy-language-contract.md#5-references-and-lifetimes) |
| `0087-fused-mutators.md` | [Language contract](0167-witchy-language-contract.md#3-owned-values-and-calls) |
| `0097-unit-type-as-empty-tuple.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0098-structural-record-width.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0110-opt-ownership-access-abi.md` | [Language contract](0167-witchy-language-contract.md#6-normal-and-opt-interoperation) |
| `0112-borrowed-aggregate-types.md` | [Language contract](0167-witchy-language-contract.md#5-references-and-lifetimes) |
| `0113-full-keyword-arguments.md` | [Language contract](0167-witchy-language-contract.md#2-functions-control-flow-and-abstraction) |
| `0122-uniform-borrow-relations.md` | [Language contract](0167-witchy-language-contract.md#5-references-and-lifetimes) |
| `0123-expression-boundaries.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0125-core-language-contract.md` | [Language contract](0167-witchy-language-contract.md#1-core-syntax) |
| `0127-ownership-and-opt-mode.md` | [Language contract](0167-witchy-language-contract.md#3-owned-values-and-calls) |
| `0128-regions-and-reclamation.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0129-concurrency-tasks-and-channels.md` | [Language contract](0167-witchy-language-contract.md#7-resources-effects-and-suspension) |
| `0130-generators-and-iterators.md` | [Language contract](0167-witchy-language-contract.md#7-resources-effects-and-suspension) |
| `0131-reflection-and-comptime.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0132-runtime-dynamic.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `0133-standard-library-contract.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `execution-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `language-evolution.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `ownership-analysis.md` | [Language contract](0167-witchy-language-contract.md#4-unique-and-frozen) |
| `performance-modes.md` | [Language contract](0167-witchy-language-contract.md#6-normal-and-opt-interoperation) |
| `regions.md` | [Language contract](0167-witchy-language-contract.md#8-regions-layout-reflection-and-libraries) |
| `round3-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `round4-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `round5-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `round6-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `round7-plan.md` | [Language contract](0167-witchy-language-contract.md#9-deliberate-changes-and-tradeoffs) |
| `impl-plans/0110-execution-plan.md` | [Normal/opt contract](0167-witchy-language-contract.md#6-normal-and-opt-interoperation); [retained evidence and obligations](retained-contract-audit.md#removed-execution-plan) |
