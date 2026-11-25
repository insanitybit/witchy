# Proposal: type invariants over plain fields, in proofs over numbers

Status: implemented 2026-11-18 (decisions.md), in the narrower form below: a construction is
accepted only as `return T(...)` of the result type, where the synthesised `ensures` covers it,
rather than proved at any construction. Found by a test program (`tests/fixtures/programs/ledger-invariant.wy`).

## The gap

A sealed type may state an invariant, `invariant balance <= limit`. Today only a function
proved by the memory model or a record model may build, change or take apart such a type
(`check/locals.rs`, `unsealed`), and those models apply only to container-shaped functions (an
element type, buffers: `memory_contract::eligible`). A plain record of numbers with an invariant
cannot be used at all: `Account.open` is refused at its constructor even with `proof: auto`.

That loses one of Witchy's strongest uses of proofs. An account whose balance never passes its
limit makes the arithmetic in `deposit` provably in range, so its checks can go, and no program
can hold an `Account` that breaks the rule.

## Soundness argument

Every value of the type satisfies its invariant, by induction over the program: only functions
in the type's file can build or change it (sealing), and each place that does is proved to
establish the invariant there. So the invariant may be *assumed*, not required, wherever a value
of the type is received. Callers need not show it, which matters: an `Account` taken out of a `Vec`
or a field of another record carries no proof at the call, yet satisfies the invariant.

The obligations are where values are made or changed:

1. **Construction.** `T(a, b)` anywhere in a scalar-proved function: the invariant over `a, b`
   at that point. Proving it at the construction, not only at a return, is what keeps a broken
   value from leaving through a call, a store or a local passed on.
2. **Field writes through a `&mut` parameter.** The invariant over the parameter's final fields
   at every return, the same as an `ensures` on it; mid-function states may break it.
3. **Field writes to a local or through any other reference.** Refused: there is no point at
   which the value is known to be released, and a method on the type is the way to change it.
4. **Taking apart** (`let T(a, b) = value`): reading, no obligation.

Assumed at entry: the invariant of every value of the type reachable from a parameter (by value,
`&` or `&mut`), and after a call, of every `&mut` argument of the type (the callee was proved to
keep it, by 2.).

## Implementation sketch

- `Contract` gains `assumed: Vec<Clause>`, resolved like `requires` from the invariant
  expressions with bare field names rewritten to the parameter's path; the scalar prover pushes
  them as hypotheses at entry and never makes them call-site obligations.
- `ensures` clauses are synthesised for each `&mut` parameter of the type (2.).
- The scalar prover adds an obligation at each `Value::Aggregate` whose type has an invariant,
  over the parts' values (1.). The prover tracks record fields as places, not records as values,
  so this needs the parts' `Math` at the construction: the `gather` path that stores an aggregate's
  parts already computes them.
- `unsealed` allows these forms in scalar-proved functions and still refuses 3.

## Tests to write first

The refusal fixtures matter more than the accepting ones: a construction that breaks the
invariant passed to an unproved function; a field write to a local; a write through a reference
that is not a parameter; an invariant assumed for an `Account` read out of a `Vec` and then
deposited into (accepted); `deposit`'s subtraction proved without a check, with a fixture that
would print a wrong number if the invariant were assumed falsely.

## Open (2026-11-18)

- Done the same day: `return Ok(T(...))`, the validating constructor, once contracts could name
  what a returned variant holds (decisions.md).
- After a call that takes a `&mut` value of the type, the caller does not assume the invariant
  again unless the callee's contract says it (a callee proved over numbers in the type's file does,
  by its synthesised `ensures`). Only facts are lost, never soundness.
