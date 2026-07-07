# BUG-540: Generic trait impls drop function type arguments

Severity: MED
Status: FIXED (this commit)
Verified: 2026-07-07
Component: generic trait impls, function types, trait dispatch, monomorphization, type scope names

## Summary

Generic trait impl dispatch now preserves function-typed type arguments instead
of collapsing the receiver to its bare head. `Box(fn(Int) -> Int)` and nested
forms such as `Box(List(fn(Int) -> Int))` can use a generic impl like
`impl Label for Box(a)` when the impl body does not inspect or render the
function value itself.

## Fix

The trait/monomorphization scope-name encoding now treats function types as
ordinary structured type arguments:

- AST `Type::Fn` and inferred `Ty::Fn` encode as `fn(...)->...`.
- The scope-name decoder round-trips that encoding back into `Type::Fn`.
- Top-level scope-name argument splitting now accounts for parentheses, so
  commas inside function parameter lists do not split generic or tuple slots.
- Generic type-variable binding descends through function parameter and return
  types.

Regression coverage lives in
`typeck::tests::generic_trait_impls_preserve_function_type_arguments`.
