# Plan: recursive nominal types on the canonical route (RFC-0165 slice 43, not started)

Read-only analysis of 2026-09-08 (a planning subagent; every claim cites the code it read).
The family covers `examples/bst`, `calc`, `eval`, `generic_stack`, and `std/json`'s `Json`
(`config_merge`, `jq`, `serve_api`, `caps_audit`, `caps_guard`, `coven_check`).

## Why it is refused today

- `LayoutId` is a content hash of the descriptor bytes, which embed child ids
  (`crates/witchy-wir/src/layout/specialized.rs`: `canonical_bytes`, `LayoutId`), so a
  self-referential descriptor is impossible, and two guards refuse cycles independently
  (`intern_resolved`'s `active_nominals`, `LayoutBundle::from_interner`'s dependency walk).
- `field_type_ok` (`crates/witchy-types/src/canonical_aggregates.rs`) admits a nominal field
  only one level deep, so `Tree` inside `Tree` is refused.
- `lower_aggregate` fills every non-selected slot with a fresh empty value
  (`push_empty_value`), so constructing `Leaf` would build an empty `Tree` recursively.
- The Wasm destructor is unrolled inline to a static depth and `drop_layout_depth`
  refuses recursive layouts; the oracle's `Engine::release` is Rust recursion.

## The extension (smallest sound shape)

1. **Opaque nominal pointee.** A childless `LayoutKind::OpaqueNominal { key }` keyed by the
   type's name (with generic arguments once slice C lands), resolved through an interner
   side table `nominal_bindings: key -> LayoutId`, carried by the layout bundle as its own
   section. The recursive occurrence is `RuntimePointerValueSlot { pointee: opaque, Owned }`,
   so hashing stays well-founded and the bundle acyclic. Every consumer that reads a
   pointee's descriptor chases the binding: the checker's `descriptor`,
   `value_type_for_layout`, `drop_ref_layout_supported`, `fixed_pointer_fields`; the
   encoder's `fixed_pointer_fields`, `emit_drop_ref_local`, `emit_release`; the oracle's
   release.
2. **Nullable owned slot.** `PointerSlot::OwnedNullable`: `push_empty_value` stores a null
   there, `require_owned_slots_initialized` accepts the null, and destruction skips a zero
   child instead of trapping. This is the one v1 ownership-model amendment: an
   uninitialized-by-design child is *null*, never a dangling or shared pointer, and
   pointer-bearing reclamation still drops every initialized child. It also removes the
   per-empty-slot allocation every sum pays today.
3. **Bounded destruction.** One emitted `$drop_<layout>` Wasm function per pointer-bearing
   layout that calls itself for children (depth becomes a deterministic Wasm stack trap);
   the oracle's release becomes an explicit work-list. Very deep trees then trap on the
   backend and not in the oracle: cap corpus depth and record the residual as an
   obligation rather than claim agreement.
4. **Shapes and lowering.** `field_type_ok` becomes a cycle-tolerant walk (a name on the
   stack is accepted, it sits behind an owned pointer). `lower_record_constructor`,
   `lower_match`, field reads (`CloneShared` retains, not deep copies; values are
   immutable so value semantics hold) and `clone_aggregate_for_storage` need no
   structural change.
5. **Generics** (slice C): type definitions are not monomorphized; instances arrive as
   `Named("Stack", [Int])`. Key the aggregate shape and the opaque nominal by
   `aggregate_key` (renders `Stack(Int)`) and substitute through the interner's
   `instantiate_fields`.

## Delivery

- Slice A: items 1-4 for a non-generic self-recursive sum; target `witchy parity
  examples/bst/src/bst.witchy` on all three legs. Tests: a launch test constructing,
  matching, and releasing a tree (leak receipt through the oracle), a negative for an
  inline/packed recursive type, layout unit tests for opaque interning, binding round-trip
  through `LayoutBundle::decode_canonical`, and refusal of an unbound nominal, an owned-slot
  test for nullable destruction of a 1000-node tree.
- Slice B: `Option(Tree)`, `List(Tree)`, tuples of recursive types (calc, eval).
- Slice C: generic instances (generic_stack), then `Json`.

Tracks that can run in parallel once the layout kind and the nullable slot are frozen:
the wir layout/bundle/checker chase; the wir backend `$drop` helper plus oracle work-list;
the types/lowering gates and `push_empty_value`.
