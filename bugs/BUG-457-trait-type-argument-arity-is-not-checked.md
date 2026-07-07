# BUG-457: trait type-argument arity is not checked

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: trait namespace validation, generic bounds, impl lowering

## Problem

Parameterized traits such as `From(a)`, `Into(b)`, and `FromIterator(e)` had a
declared type-argument count, but uses of those traits did not consistently check
that count. Bounds like `where a: From(Int, String)` could be accepted even
though `From` declares exactly one type parameter.

That made generic API contracts weaker than ordinary type applications:
`List(Int, String)` was rejected, but `From(Int, String)` in a trait position
could survive as a source contract.

## Resolution

The pre-lowering trait-name validation pass now records each known trait's
declared arity and validates every trait-use site it already checks:

- impl heads, including `impl From(Int) for T`
- function `where` clauses
- `impl Trait` parameter sugar
- impl-level and impl-method `where` clauses
- supertrait lists, which currently carry zero type arguments

Diagnostics name the trait, expected count, actual count, and the use context.

Regression:

- `trait_type_argument_arity_is_checked`
