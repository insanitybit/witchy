# Design: closures that hold authority

Status: built 2026-11-16, as below except where noted (docs/wasm-closures.md, "Callbacks
that hold authority"). Written before building: Closures today hold numbers and Bools only
(docs/wasm-closures.md). This note is what holding a capability needs, decided against the
book (tour-functions.md "Closures", capabilities-narrowing.md "Withholding authority by
structure": "an ordinary callback may exercise authority captured by its creator, while
exposing only its narrower callable interface to the receiver").

## What blocks it today

1. **Type arguments may not hold authority** (semantic_types.rs `function_argument`).
   A function taking `f: fn(U64) -> U64` is generic over the closure's record, so a closure
   holding `Console<Write>` would be such a type argument. The rule exists so that
   `wasm-caps` and `pure fn`, which read signatures, see all authority.
2. **A non-`Any` type parameter is copied and dropped freely.** Capabilities here are moved,
   not copied (`Holder(move console)`), so a closure holding one is an owner and its
   parameter would need `Any`, which makes `f` something the callee must hand on or drop
   explicitly.

## Decisions

- **Capture by move.** A captured capability is named in the closure's body and the
  closure takes it as `move` does: the enclosing function no longer has it. No new
  syntax; a capability is never copied.
- **A closure holding authority is lent.** (As built; the first plan made it an owner
  handed over.) A parameter `f: &fn(U64) -> ()` is `&F` with `F: Any`: the callee calls it
  and can neither copy nor keep it, and the maker keeps the closure. By value
  (`f: fn(...)`, a copied type parameter) it is refused.
- **`wasm-caps` follows type arguments, for call traits only.** The type-argument rule is
  relaxed for exactly the parameters of function types (and `Self` of a call trait's
  `call`), and the footprint of a generic function
  reached with such a record adds what the record holds, marked
  `Console<Write> (through f)`. A function that never receives one reports nothing new.
  `pure fn` stays strict: a pure function takes no closure that holds authority.
- **The maker's footprint is unchanged:** it held the capability already.

## Order of work

1. Footprint over call-edge substitutions (no language change; testable with the existing
   generic programs).
2. Relax `function_argument` for closure records only, with the footprint in place.
3. Capture by move in closure.rs (captures of capability type), the record made linear.
4. Red-team round: authority leaking through a closure where `wasm-caps` does not show it,
   a capability duplicated through a copied closure, a `pure fn` reached with one.
