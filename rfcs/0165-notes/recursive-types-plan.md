# Plan: recursive nominal types on the canonical route (RFC-0165, in progress)

Initial analysis of 2026-09-08, amended after integration wave `297b7b42e`.
Opaque and nullable layout transport, cycle-aware source-shape traversal, and
bounded-stack destruction are landed. Recursive source admission remains sealed:
transport is not evidence that recursive values are supported.
The family covers `examples/bst`, `calc`, `eval`, `generic_stack`, and `std/json`'s `Json`
(`config_merge`, `jq`, `serve_api`, `caps_audit`, `caps_guard`, `coven_check`).

## Original blockers and current disposition

- `LayoutId` is a content hash of the descriptor bytes, which embed child ids
  (`crates/witchy-wir/src/layout/specialized.rs`: `canonical_bytes`, `LayoutId`), so a
  self-referential descriptor is impossible, and two guards refuse cycles independently
  (`intern_resolved`'s `active_nominals`, `LayoutBundle::from_interner`'s dependency walk).
- The one-level nominal shape limit is removed. Root projection is cycle-aware;
  physical interning still emits a precise recursive-layout refusal pending the
  nullable operation family and authenticated nominal bindings.
- `lower_aggregate` fills every non-selected slot with a fresh empty value
  (`push_empty_value`), so constructing `Leaf` would build an empty `Tree` recursively.
- The former static-depth Wasm destructor and recursive oracle release are
  replaced with explicit worklists. Production recursive source coverage still
  requires the layout, nullable-slot, and lowering obligations below.

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
2. **Nullable owned slot.** `PointerSlot::OwnedNullable` transport is sealed pending
   coupled checker, oracle, and backend support. `InitNullableOwned` explicitly
   initializes absence; it is not a scalar null store or an uninitialized cell.
   `ClonePresent` traps on absence before retaining or dereferencing a child and
   produces a non-null SSA reference. Ordinary ownership transfers initialize a
   present edge, never overwrite a live edge silently. Destruction skips absent
   nullable slots only; zero in a non-nullable owned slot still traps. This removes
   the recursive empty-child allocations without weakening live references.
3. **Bounded destruction.** The landed backend uses an explicit depth-first
   worklist in checked temporary linear-memory storage; oracle release also uses
   a worklist. Each owned edge is released once, including repeated shared edges.
   The backend checks worklist growth and clears temporary storage before restoring
   the allocation frontier. A recursive Wasm destructor and deliberate deep-tree
   stack disagreement are not acceptable substitutes. Preserve the deep-chain,
   wide-graph, underflow, and scratch-restoration probes when adding nullable edges.
4. **Shapes and lowering.** `field_type_ok` becomes a cycle-tolerant walk (a name on the
   stack is accepted, it sits behind an owned pointer). `lower_record_constructor`,
   `lower_match`, field reads, and `clone_aggregate_for_storage` must preserve the
   nullable distinction. A live reference loaded from a nullable field uses
   `ClonePresent`; a sum tag alone does not authorize an unchecked reference load.
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
the wir layout/bundle/checker chase; the wir backend worklist plus oracle semantics;
the types/lowering gates and `push_empty_value`.
