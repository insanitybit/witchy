# BUG-444: Duplicate parameter names are accepted

Status: FIXED
Severity: MED
Found: 2026-07-04
Fixed: 2026-07-06

## Summary

Witchy accepted duplicate parameter names in function, method, trait-method, and
lambda parameter lists. The type checker inserted parameters into a scope map in
order, so the later parameter silently shadowed the earlier one. That made reads
of the duplicated name ambiguous and made keyword argument labels incoherent.

## Fix

The type checker now validates parameter uniqueness before source lowering, and
again after trait lowering so generated helpers obey the same invariant.
Duplicate parameters produce a direct diagnostic naming the duplicated parameter
and the callable surface.

Regression coverage:

- `crates/witchy-types/src/typeck_tests.rs::duplicate_parameter_names_are_rejected`
