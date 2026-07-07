# BUG-263: Uncalled generic function bodies were unchecked

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Branch: fix/generic-body-check

## Problem

A `where`-bounded generic function with an ill-typed body could pass
`witchy check` if no call site instantiated it. Trait lowering treated bounded
generics as no-fallback templates, then removed uninstantiated originals before
the ordinary body checker saw them.

## Fix

`traits::lower_checked` now runs a selected declaration-time sanity check over
no-fallback template bodies before dropping them. Lazy monomorphization
placeholders such as unresolved bounded trait dispatch are still deferred, but
ordinary body type errors surface at the generic function declaration.

Regression coverage:

- `typeck::tests::uncalled_bounded_generic_bodies_are_checked`
- `typeck::tests::generic_dict_key_operation_requires_eq_bound`
- `example_tests::all_std_modules_type_check`
