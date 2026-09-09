# Nested callable layout matching omitted source signature comparison

## Status

Found during integration review of worker commit `9d1868f65`. That commit was
not integrated into the production candidate in that form. The repaired stack
is now integrated locally as `0010279e1` and `0bbf2060c`; queue landing remains
pending. No runtime exploit has been demonstrated.

The repair compares source/callable and source/layout pairs using one bounded
context across both graphs. It checks signatures rather than mere registry
membership and preserves cycle detection across function/aggregate edges.
Focused wrong-signature and recursive-graph tests passed on the worker. The
integrated source/oracle/admitted-Wasm compose-and-reclaim probe also passed
(0.40 seconds; `/private/tmp/rfc0165-recursive-callable-integrated.log`).

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
