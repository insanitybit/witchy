# Recursive type integration contract

This refines `recursive-types-plan.md`. Recursive values stay rejected by
production validation until all three boundaries below are implemented.

## Layout identity and transport

`LayoutKind::OpaqueNominal { key: [u8; 32] }` is an identity with no storage.
It can appear as a pointer pointee, never as an inline field. The key must be
derived from the canonical checked nominal type (including generic arguments);
a name or key alone does not authenticate its body.

`LayoutInterner::bind_nominal(identity, concrete_body)` binds once. A target
must exist and cannot itself be opaque. Ordinary descriptor dependencies stay
acyclic; a binding can close a cycle across a pointer. `get` returns the
original descriptor. `resolve_nominal` explicitly resolves one binding, so
hashing never silently substitutes a different descriptor.

Layout bundle version 2 includes the sorted reachable binding table. Decoding
rejects missing targets, duplicate bindings, unbound reachable identities,
unused descriptors/bindings, and noncanonical order. Existing acyclic bundles
retain version 1 and identical bytes. The whole bundle, including bindings,
already participates in candidate identity; final source admission must also
compare the concrete body with the checked nominal's fields. That comparison
must memoize exact (source type, candidate layout) pairs through recursive
pointer edges, not treat a repeated name as proof.

## Nullable owned slots

The planned nullable slot may contain zero only as an explicitly initialized
absence. Zero is never an object reference, must not increment a count, and
must not authorize a projection. Destruction skips absent children. Loading
a nullable slot as a reference must establish non-nullness or trap before
granting reference authority. Merely teaching destruction to skip zero is
insufficient. Keep nullable slots unimplemented until initialization, load,
copy, and destruction rules can land together.

## Destruction

Do not enable recursive Wasm calls for structural destruction and then hide
the stack-overflow discrepancy by limiting corpus depth. The implementation
target is an explicit checked worklist, with bounded Wasm call-stack use.

The backend can reserve temporary worklist storage above the allocator's
current frontier, push (object, layout ordinal) entries, and iteratively
release them. Every size/address expansion must be checked, memory growth
failure must trap, and no host callback or ordinary allocation may run during
the drain. The worklist must never overlap live allocations or free-list
metadata. Restore the frontier only after the worklist is empty. An object
is reclaimed only after its last reference is consumed and its child tokens
are transferred to the worklist. Shared/static children follow the existing
reference-count/static rules. The independent oracle uses its own worklist.

This is a target algorithm, not a completed backend implementation. Require
a deep-chain execution probe, a branching tree with shared children, and
nullable-child/refcount adversarial tests before source admission is enabled.

## Current slice

Only layout identity/transport and the production refusal are implemented.
`structural_contracts::recursive_layout_transport` tests self/mutual recursion, round-trip identity,
missing/rebound/inline rejection, binding-dependent bytes, and the encoder
authority refusal. It establishes infrastructure, not recursive source coverage.
