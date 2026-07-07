# BUG-551: type names are rejected as values

Severity: HIGH
Status: FIXED
Fixed: 2026-07-06
Component: type checker, linker ambient names, compiled backend parity

## Problem

The resolver keeps some type heads bare after linking, either because they are
built-ins (`Int`, `String`, `Secret`), ambient std names (`Set`, `Iter`,
`Ordering`), compiler synthetic heads (`Tuple2`), or local type names used as
static-method receivers.

When one of those names appeared in expression position, the parser represented
it as a constructor expression. If no matching value constructor signature was
available, the type checker treated it like an unknown constructor with a fresh
result type. `witchy check` could then accept programs such as `Int(1)`,
`Set([])`, `Tuple2(1, 2)`, or a bare sum type name, while the compiled backend
later rejected them as unsupported constructs.

## Fix

Constructor inference now rejects proven type heads that are not constructors:
declared type names without a same-named constructor, built-in type names,
ambient std type heads, and compiler synthetic tuple/anonymous record heads.
Real constructors still use the ordinary constructor signature path, including
zero-field variants such as `None` and `Less` and policy constructors such as
`NetPolicy("...")`.

Regression:

- `type_names_are_rejected_as_values_after_linking`
