# BUG-172: LSP omits `async fn` and `gen fn` symbols

Severity: MED
Status: FIXED
Verified: 2026-07-09 - completion and hover cover the shipped generators and async examples
Component: LSP completion, LSP hover, async/generator language support
Discovered: 2026-07-05

## Summary

LSP completion, hover, signature qualification, and declaration lookup each
recognized only `fn` and `pub fn` through separate source-prefix checks. As a
result, accepted `async fn` and `gen fn` declarations were absent from the
editor symbol surface.

## Historical Reproduction

The CLI accepted both release-facing examples:

```console
$ witchy check examples/generators/src/generators.witchy
$ witchy check examples/async_tasks/src/async_tasks.witchy
```

Before the fix, completion for the generator example offered `main` but omitted
`fibs` and `collatz`; hover on `fibs` returned null. Completion for the async
example omitted `ticker` and `main`; hover on `ticker` returned null.

## Resolution

One tolerant function-declaration indexer now recognizes all source forms:

- `fn` and `pub fn`
- `async fn` and `pub async fn`
- `gen fn` and `pub gen fn`

Completion, imported-module completion, hover signature lookup, module
qualification, and mode-diagnostic declaration lookup all consume that helper.
It deliberately operates on one header line rather than requiring the whole
buffer to parse, preserving useful editor behavior while a user is typing.

Regression coverage drives completion and hover against the shipped
`examples/generators` and `examples/async_tasks` sources, and also checks public
async/generator signature qualification.

## Acceptance

- Generator completion includes `fibs` and `collatz`.
- Hover renders `gen fn fibs() -> Iter(Int)` and its leading docs.
- Async completion includes `ticker` and `main`.
- Hover renders the full `async fn ticker(...) -> Nil` signature.
- Ordinary function completion and hover remain unchanged.
