# BUG-258: `impl Trait for T` blocks are not validated against the trait

Severity: MED
Status: FIXED (this commit)
Verified: 2026-07-07
Component: trait impl validation, `crates/witchy-types/src/traits.rs`, diagnostics

## Fix

Trait impl blocks are now validated at declaration time before trait lowering:

- methods not declared by the trait are rejected at the impl, with a near-miss
  suggestion when available;
- required trait methods without defaults must be present;
- provided method arity, annotated parameter types, parameter conventions, and
  return type must match the trait signature after substituting `Self` and the
  trait's type arguments.

Regression coverage lives in `typeck::tests::trait_impls_must_match_trait_methods`.
