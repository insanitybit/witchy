# BUG-530: Tuple trait coverage stops at arity four

Status: FIXED (this commit)
Severity: MED
Component: tuple stdlib protocols, `Show`, `Reflect`, `json.stringify`, public docs

Witchy accepts tuples wider than four elements, but `std/show.witchy` and
`std/reflect.witchy` only had tuple impls through arity 4. That made 5-tuples
legal structural values while `show.say(console, (1, 2, 3, 4, 5))`,
`json.stringify((1, 2, 3, 4, 5))`, and `derive(Reflect)` on a record containing a
5-tuple all failed with missing `Tuple5` protocol impls.

The public contract is now explicit: tuple `Show` and `Reflect` impls are
provided through arity 8. Wider tuples remain legal structural values, but code
that needs protocol-backed display or reflection should use a named record or a
homogeneous list.

The regression `tuple5_show_and_reflect_protocols_work_on_both_backends` covers:

- `show.say` and interpolation for a 5-tuple;
- `reflect.debug` and `json.stringify` for a 5-tuple;
- `derive(Reflect)` for a record with a 5-tuple field;
- an 8-tuple `show.render`/`json.stringify` smoke path on both backends.
