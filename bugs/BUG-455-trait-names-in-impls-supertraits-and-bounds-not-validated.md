# BUG-455: Trait names in impls, supertraits, and bounds are validated

Severity: MED
Status: FIXED (this commit)
Verified: 2026-07-07 on master 7dcc128 before fix; `impl Missing for T`,
`trait T: Missing`, `where a: Missing`, and `impl Missing` parameter bounds all
still checked successfully.
Component: trait namespace validation, impl lowering, generic bounds,
diagnostics

## Resolution

The type checker now runs a pre-trait-lowering trait-name validation pass after
record/type validation. It collects declared traits from the linked module,
accepting both fully-qualified and bare names, and treats the comparison
hierarchy (`PartialEq`, `Eq`, `PartialOrd`, `Ord`) as ambient because those
traits are part of the operator/type-checking core.

The pass rejects unknown trait names in:

- trait supertrait lists;
- trait impl heads;
- function `where` clauses;
- `impl Trait` parameter sugar;
- impl `where` clauses;
- impl method `where` clauses.

Regression: `unknown_trait_names_are_rejected` covers each source surface and a
valid local trait control. Command-level checks also verified imported `Show`
remains accepted while unimported `Show` is still rejected.
