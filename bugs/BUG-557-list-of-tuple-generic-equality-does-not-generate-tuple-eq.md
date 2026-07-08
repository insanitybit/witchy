# BUG-557: List-of-tuple generic equality does not generate tuple equality

Status: OPEN
Severity: MED
Component: trait monomorphization, tuple `PartialEq`, container equality

## Summary

After adding tuple `PartialEq` / `Eq` impls through arity 8, direct generic tuple
equality checks:

```witchy
fn same(x: a, y: a) -> Bool where a: PartialEq:
    x == y

same((1, 2, 3, 4, 5), (1, 2, 3, 4, 5))
```

But using that tuple equality only transitively through `List(a)` can fail during
the generated typecheck pass:

```witchy
fn total_same(x: a, y: a) -> Bool where a: Eq:
    x == y

total_same([(1, "x", true, 90s, Greater)], [(1, "x", true, 90s, Greater)])
```

Observed diagnostic:

```text
type error: `PartialEq__List__eq___Int__String__Bool__Duration__Ordering_`, line ...: call to unknown function `PartialEq__Tuple5__eq`
```

## Why It Matters

This is the remaining edge of the BUG-538 protocol-matrix work: tuple equality
is now a public protocol directly, and list equality composes through element
`PartialEq` for ordinary record/std values, but tuple equality is not generated
when it is only needed as a nested specialization.

## Direction

Trait monomorphization should discover and generate nested protocol
specializations required by generated generic bodies, so `List((...))` equality
works exactly like `List(Key)` and `List(Option(Key))`.

