# BUG-454: Duplicate trait impl heads are rejected

Severity: MED
Status: FIXED
Fixed: 2026-07-06
Component: trait coherence, impl lowering, declaration uniqueness

## Problem

Witchy accepted multiple `impl Trait for Type` blocks for the same trait and
target head. Trait lowering then generated the same mangled method names and
updated dispatch tables with ordinary inserts, making the selected body an
artifact of lowering order rather than a source-level rule.

## Fix

The pre-lowering declaration uniqueness pass now tracks trait impl heads by the
same shape the current trait lowering can represent: trait name plus target type
head. A repeated `impl Label for Box` fails with a source diagnostic before any
generated function or dispatch table can overwrite a previous impl.

Different target types implementing the same trait remain valid.

Regression coverage:

- `crates/witchy-types/src/typeck_tests.rs::duplicate_declarations_are_rejected`
