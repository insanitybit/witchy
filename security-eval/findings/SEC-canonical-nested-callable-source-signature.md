# Nested callable layout matching omitted source signature comparison

## Status

Found during integration review of worker commit `9d1868f65`. That commit was
not integrated into the production candidate. Repair and an adversarial test
are required before integration; no runtime exploit has been demonstrated.

## Finding

The new `enqueue_layout_children` arm for a source `Type::Fn` checks that the
candidate is a registered closure layout with a Callable field and an owned
environment field. It does not compare the registered function signature with
the source function type at that field.

Registry membership establishes that the layout describes some callable, not
that it describes this source callable. The nested layout/source graph must
preserve parameter, result, and convention correspondence, including recursive
function-bearing aggregates. The direct function-value check is not a substitute
for checking function values encountered inside a stored aggregate.

## Required evidence

Keep the existing compose/capture positive probe. Add a negative candidate that
substitutes a different, otherwise registered callable signature into a nested
function field and require source-correspondence rejection. Recursive matching
must terminate using checked graph identities without treating an arbitrary
registered signature as equivalent.
