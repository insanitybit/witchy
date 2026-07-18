---
rfc: 0097
title: Unit type as empty tuple
status: implemented
created: 2026-07-17
---

# RFC-0097: Unit type as empty tuple

## Summary

Replace the `Nil` builtin type with `()`, the zero-arity tuple. Functions that
return nothing omit the return annotation entirely; the explicit spelling `-> ()`
is permitted but never required.

## Motivation

Witchy already has structural tuples at every arity from 2 upward. `Nil` is an
ad-hoc builtin that plays exactly the role the zero-arity tuple would fill: the
type of expressions evaluated for effect only. Unifying them:

1. **Removes a special case.** One fewer keyword, one fewer `Ty` variant, one
   fewer thing to explain. The unit type falls out of the tuple system for free.
2. **Aligns with the existing tuple grammar.** `(a, b)` is arity-2; `()` is
   arity-0. The system is already structural — `Nil` is the outlier.
3. **Makes omitted return types principled.** "No annotation = returns `()`" is
   the standard rule in languages with structural products (Rust, Haskell, OCaml,
   Scala). Today witchy infers return types for non-pub functions, but pub
   side-effecting functions must write `-> Nil` — an annotation that carries no
   information.
4. **Witchy has no implicit Nil.** Unlike languages where `nil`/`null` infects
   every type (Ruby, Lua), `Nil` in witchy is purely the unit type. Calling it
   `()` makes that role explicit and removes any confusion with nullable
   semantics.

## Design

### Type system

- Remove `Ty::Nil` from the checker. Replace with `Ty::Tuple(vec![])` (or a
  distinguished `Ty::Unit` that prints as `()` — implementation choice).
- `()` is both a type and a value (the unique inhabitant of the zero-arity
  tuple).
- The tuple grammar is extended to arity 0: the literal `()` parses as the unit
  value in expression position and the unit type in type position.

### Function signatures

- An omitted return annotation on any function (pub or not) means `-> ()`.
- Explicit `-> ()` is legal but redundant; no warning.
- `-> Nil` becomes a parse error (there is no type named `Nil`).

### Expression rules

- A block whose last expression is a statement (assignment, `var` write-back,
  `say`, etc.) has type `()`.
- The literal `()` is a valid expression of type `()`.
- Pattern matching: `()` is an irrefutable pattern matching the unit value.

### Standard library

All `-> Nil` annotations in `std/*.witchy` are removed (the functions simply omit
the return type). For example:

```witchy
// Before
pub fn push(var xs: List(a), x: a) -> Nil:
    __list_push(xs, x)

// After
pub fn push(var xs: List(a), x: a):
    __list_push(xs, x)
```

### Migration

- The name `Nil` is retired. It is not reserved — user code may define a type
  named `Nil` if desired, though this is discouraged.
- Existing code using `-> Nil` gets a clear error: "unknown type `Nil`; functions
  that return nothing omit the return type."
- `Nil` as a value in existing code (e.g. `return Nil`) becomes `return ()` or
  simply `return`.

### Arity-1 tuple disambiguation

`(x)` remains a parenthesized expression, not a 1-tuple. This is unchanged.
Tuple arities are 0 and 2+, matching the current system.

## Alternatives

**Keep `Nil` as-is.** Works, but leaves the language with a named unit type
that has no relationship to its structural product system. Every tutorial must
explain that `Nil` is not null/none and is really just "the type with one
value."

**Allow both `Nil` and `()` as aliases.** Adds complexity without removing the
special case. Two names for one concept is worse than one.

**Use `Void` instead of `()`.** `Void` conventionally means "uninhabited" (no
values), not "unit" (one value). Wrong semantics.

## Drawbacks

- ~72 sites in the stdlib need their `-> Nil` annotations removed. Mechanical
  but touches many files.
- `()` is less visually distinctive than `Nil` in function signatures for
  readers skimming code. Mitigated by the convention of simply omitting the
  return type.
- Code that currently pattern-matches on the `Nil` value or names it explicitly
  must migrate. The error message guides them.

## Prior art

- **Rust:** `()` is the unit type; omitted return means `-> ()`.
- **Haskell:** `()` is the unit type and value.
- **OCaml:** `unit` with value `()`.
- **Scala 3:** `Unit` with value `()`.
- **Go:** no explicit unit type; functions with no return simply omit it. Witchy
  takes the same user-facing ergonomics (omit the annotation) but backs it with
  a real type for consistency.

---
