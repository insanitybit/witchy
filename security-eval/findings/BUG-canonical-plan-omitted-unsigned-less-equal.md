# BUG: canonical exact-byte plan omitted unsigned less-equal guards

## Impact

Canonical SafeWIR validation and Wasm emission both supported unsigned `<=`
comparisons and arithmetic guards, but the independent exact-byte plan accepted
only unsigned `<` and bounds guards. A valid borrowed `List(Int)` parameter
therefore passed initial/final safety validation and encoded correctly, then
failed closed during exact artifact admission with `UnsupportedPlan`.

## Reproducer

```witchy
fn first(xs: List(Int)) -> Int:
    xs[0]

fn main() -> Int:
    first([42])
```

The sound header bootstrap loads length and capacity, guards `length <= capacity`
and the Wasm32 allocation-size bound, then guards `index < length`. Before the
fix, the first `<=` could not be represented by `PlanToken`.

## Resolution

Add `I32LeU` to the decoded plan vocabulary, map the already-supported
`ComparisonOperator::LessEqualUnsigned` emission to it, and model every guard
kind using the encoder's common trap sequence. The focused end-to-end probe now
executes `42` through exact-byte admission.
