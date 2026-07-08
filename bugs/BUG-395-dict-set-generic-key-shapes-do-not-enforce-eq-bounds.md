# BUG-395: std/dict wrappers bypass generic key `Eq` enforcement

Severity: MED
Status: FIXED
Verified: 2026-07-08 FIXED on `worktree-wt-bug-395-bound-call-enforcement`
Component: `crates/witchy-types`, `std/dict`, RFC-0047 key/member rules, generic bounds
Found: 2026-07-05
Updated: 2026-07-08

## Summary

Fixed in two parts:

- `4c5efe04` made public `std/dict` helper signatures and generated stdlib docs
  expose the same `where k: Eq` / `where v: Eq` obligations as the direct native
  dict operations.
- `worktree-wt-bug-395-bound-call-enforcement` preserves function `where`
  clauses in the checker signature table and validates them at generic call
  sites. A wrapper such as `pub fn wrapped(d: Dict(k, v), key: k):
  dict.get(d, key)` must now forward `where k: Eq` instead of exporting an
  apparently unbounded API.

Regression coverage:

- `typeck::tests::bounded_generic_call_requires_wrapper_to_forward_bound`
  proves ordinary bounded helper calls cannot have their obligations erased by
  an unbounded wrapper.
- `example_tests::dict_wrapper_key_operations_require_visible_eq_bounds` proves
  public `std/dict` helpers expose and enforce their key/value `Eq` contracts,
  including the no-`main` library wrapper case.

Residual not covered here: BUG-321 remains the separate compiled-backend
limitation for concrete record/enum Dict keys that already satisfy `Eq`.

## Original summary

RFC-0047 says `Dict(k, v)` keys require `Eq`, with the checker enforcing that as
one type-level rule. Current source now partially enforces this for direct native
dictionary operations: a user generic helper that calls `dict.get_or`,
`dict.insert`, `dict.contains_key`, or `dict.remove` without `where k: Eq` is
rejected.

The residual gap is the standard-library wrapper layer. `std/dict` still exports
generic helpers such as `dict.get`, `dict.from_pairs`, `dict.map_values`,
`dict.filter`, `dict.merge`, and `dict.invert` without key bounds, and the
checker explicitly exempts std modules from the generic-dict-key operation check.
User code can therefore route key-sensitive operations through the public std
helpers and get an unbounded generic API despite the direct builtin path being
guarded.

## Evidence

- `crates/witchy-types/src/typeck.rs:924-935` identifies direct native dict key
  operations (`dict.insert`, `dict.get_or`, `dict.update`, `dict.contains_key`,
  `dict.remove`).
- `crates/witchy-types/src/typeck.rs:1412-1418` records generic dict key
  operations performed in the current body.
- `crates/witchy-types/src/typeck.rs:4150-4182` rejects unbounded generic dict
  key operations after inference, but skips the check when the function belongs
  to a std module.
- `crates/witchy-types/src/typeck_tests.rs:1429-1448` covers the direct builtin
  path: an unbounded generic helper using `dict.get_or` fails, while the same
  helper with `where k: Eq` passes.
- `std/dict.witchy:20-38` gives `where k: Eq` to the direct public wrappers for
  `insert`, `get_or`, `update`, `contains_key`, and `remove`.
- `std/dict.witchy:58-65` implements `dict.get` by comparing keys with `==`, but
  the signature has no `where k: Eq`.
- `std/dict.witchy:70-76` implements `from_pairs` by inserting each key into a
  new dict, but has no `where k: Eq`.
- `std/dict.witchy:78-109` implements `map_values`, `filter`, `merge`, and
  `invert` by creating/inserting into dictionaries without exposing the needed
  key/value `Eq` bounds in their signatures.
- `spec/stdlib.md:397-419` publishes these unbounded helper signatures.
- `std/iter.witchy:242-254` shows the intended shape for the same operation:
  `impl FromIterator((k, v)) for Dict(k, v) where k: Eq`.

## Why this is a release gap

This is a language/stdlib consistency issue. Direct builtins teach one rule
("generic dict keys must be `Eq`"), while the ergonomic std helpers expose a
different rule because their signatures omit the same bound.

The comment in `typeck.rs` says std dict helpers are safe because they key their
output dicts from an input `Dict`, whose existence should prove the key is `Eq`.
That argument does not cover `from_pairs`, which builds a dict from a list, nor
`invert`, whose output key type is the input value type. It also makes the public
contract depend on a private std exemption instead of a visible type signature.

This is distinct from BUG-321, which is about compiled support for concrete
record/enum dict keys that already satisfy `Eq`. BUG-395 is about generic API
bounds and the public stdlib contract.

## Expected fix

Make the key/member invariant visible at the std boundary:

- Add `where k: Eq` to `dict.get`, `from_pairs`, `map_values`, `filter`, `merge`,
  and `values_where` if their signatures need key comparison/insertion.
- Add `where v: Eq` or a renamed helper for `invert`, because its output keys are
  the input values.
- Narrow or remove the std-module exemption in `typeck.rs`; trusted std helpers
  should not need a hidden rule that user code cannot express.
- Keep the already-added direct builtin tests, and add tests for wrapper calls:
  - an unbounded generic wrapper around `dict.get` is rejected or requires
    `where k: Eq`;
  - `dict.from_pairs` / `dict.invert` cannot manufacture unbounded generic dicts;
  - bounded versions continue to type-check.

## Acceptance

- Direct native dict operations and public `std/dict` helpers present one
  consistent key-bound story.
- Generated stdlib docs show the same `where` bounds users need to write.
- BUG-321's concrete record/enum key support can be fixed without relying on
  unbounded generic equality.
