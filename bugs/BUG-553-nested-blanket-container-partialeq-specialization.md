# BUG-553: nested blanket container PartialEq impls do not specialize reliably

Severity: MED
Status: FIXED
Fixed: 2026-07-07
Component: trait dispatch, monomorphization, std/cmp container protocols

## Problem

`List(a)` could satisfy `PartialEq` and `Eq` bounds, but extending the same
blanket protocol style to nested containers exposed a deeper specialization
gap. When `Option(List(T))`, `Result(List(T), E)`, or similar shapes called
through a generic blanket impl body, the impl body could fall back to structural
payload equality instead of resolving the concrete helper for the inner receiver
type.

The practical symptom is that additive stdlib impls such as:

```witchy
impl PartialEq for Option(a) where a: PartialEq:
    ...
```

could typecheck, but did not compose correctly once the body compared nested
generic payloads.

## Why this matters

The language should present protocol bounds as a coherent abstraction: if a
container advertises `PartialEq` only when its element does, nested uses should
compose naturally. Leaving this unresolved pushes stdlib authors toward ad hoc
helpers and special cases, which is exactly the inconsistency the 0.1 cleanup is
trying to remove.

## Fix

`std/cmp` now provides blanket impls for:

- `Option(a): PartialEq/Eq`
- `Result(a, e): PartialEq/Eq`

The impl bodies compare payloads with ordinary bounded equality, and
monomorphization now carries concrete constructor-pattern payload shapes through
specialized generic impl bodies. That makes nested protocol calls specialize
instead of falling back to structural equality for concrete containers such as
`List(Key)`.

`Dict(k, v)` keeps direct equality coverage, but compiled equality through a
generic `PartialEq` bound still needs a separate codegen/helper pass before it
can be claimed as a fully compositional protocol value.

Regression coverage:

- `example_tests::nested_container_equality_satisfies_protocol_bounds_on_both_backends`

## Related work

BUG-535 intentionally stops at `List(a): PartialEq/Eq` because direct list
equality also has native structural semantics that must remain available for
underived records and ADTs.
