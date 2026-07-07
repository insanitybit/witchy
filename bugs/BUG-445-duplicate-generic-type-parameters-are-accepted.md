# BUG-445: Duplicate generic type parameters are rejected

Status: FIXED
Fixed: 2026-07-06
Severity: MED
Component: language frontend, generic ADTs/records, traits, type inference

## Problem

Explicit generic type-parameter lists accepted duplicate names. A declaration
such as `type Pair(a, a): Pair(a, a)` allocated multiple internal generic slots
with one source spelling, then name-map insertion made only the later slot
reachable from field annotations.

Trait declarations had the same public-surface problem with declarations such as
`trait Codec(a, a): ...`.

## Fix

The pre-lowering declaration uniqueness pass now validates generic binder lists
on type and trait declarations. Duplicate type parameter names produce a
source-level diagnostic before type checking, trait lowering, derive expansion,
or documentation/codegen can observe an incoherent generic shape.

Regression coverage:

- `crates/witchy-types/src/typeck_tests.rs::duplicate_declarations_are_rejected`
