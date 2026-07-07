# BUG-553: nested blanket container PartialEq impls do not specialize reliably

Severity: MED
Status: OPEN
Component: trait dispatch, monomorphization, std/cmp container protocols

## Problem

`List(a)` can now satisfy `PartialEq` and `Eq` bounds, but extending the same
blanket protocol style to nested containers exposed a deeper specialization gap.
When `Option(List(T))`, `Result(List(T), E)`, or similar shapes call through a
generic blanket impl body, monomorphization can leave an unspecialized nested
container protocol call behind instead of resolving the concrete helper for the
inner receiver type.

The practical symptom is that additive stdlib impls such as:

```witchy
impl PartialEq for Option(a) where a: PartialEq:
    ...
```

can typecheck, but compiled execution may fail during linking or dispatch once
the body compares nested generic payloads.

## Why this matters

The language should present protocol bounds as a coherent abstraction: if a
container advertises `PartialEq` only when its element does, nested uses should
compose naturally. Leaving this unresolved pushes stdlib authors toward ad hoc
helpers and special cases, which is exactly the inconsistency the 0.1 cleanup is
trying to remove.

## Suggested fix

Teach trait-call rewriting/monomorphization to specialize nested blanket impl
calls from the concrete receiver type all the way through generic impl bodies.
Then add stdlib blanket impls and backend-parity tests for at least:

- `Option(a): PartialEq/Eq`
- `Result(a, e): PartialEq/Eq`
- `Dict(k, v): PartialEq/Eq` where keys and values satisfy the required bounds

## Related work

BUG-535 intentionally stops at `List(a): PartialEq/Eq` because direct list
equality also has native structural semantics that must remain available for
underived records and ADTs.
