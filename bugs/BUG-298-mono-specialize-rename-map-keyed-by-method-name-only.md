# BUG-298: Mono::specialize rename map is keyed by method name only

Severity: HIGH
Status: FIXED
Fixed: 2026-07-07
Component: crates/witchy-types/src/traits.rs, RFC-0046 typed trait dispatch, monomorphization

## Problem

Same-trait generic bounds must preserve the identity of each bound variable:
`where a: Named, b: Named` introduces two independent dispatch obligations.
Older monomorphization keyed trait-call rewrites too coarsely, so calls with the
same method name could route through the wrong concrete impl. Most receiver
method cases were already fixed by keying value calls by `(receiver head,
method)`, but static/no-self trait methods such as `a.tag()` still lowered to a
bare `tag()` call with no receiver evidence and failed as an unknown function.

## Fix

Static trait calls on bound type variables now lower to an internal marker that
preserves the source receiver variable. During specialization, the marker is
rewritten through the same concrete bound/impl resolution as receiver method
calls, so `a.tag()` and `b.tag()` dispatch independently even when both bounds
use the same trait and method name.

Regression coverage:

- `static_trait_methods_on_distinct_bounds_keep_receiver_identity` in
  `crates/witchy-types/src/typeck_tests.rs`
- scratch probes `t_f1_silent_noself.witchy` and
  `t_f1_silent_noself_swap.witchy` now print `A B` and `B A`
