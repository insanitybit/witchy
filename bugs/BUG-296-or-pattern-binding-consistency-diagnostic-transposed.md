# BUG-296: Or-pattern binding-consistency diagnostic transposed bind sets

Severity: LOW
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-62390-1783511734`
Component: crates/witchy-types/src/typeck.rs, RFC-0052 patterns, diagnostics

## Problem

The checker correctly rejected or-pattern alternatives that bound different
names, but the diagnostic attached the first alternative's bind set to the later
alternative's displayed pattern.

For example, `Circle(r) | Square(_)` reported that `Square(_)` bound `{r}`,
which is false and points the user at the wrong side of the pattern.

## Fix

The diagnostic now prints the later alternative with its own bind set and names
the first alternative's bind set as "another alternative".

## Regression

- `typeck_tests::or_pattern_binding_diagnostic_names_the_actual_alt_bindings`
