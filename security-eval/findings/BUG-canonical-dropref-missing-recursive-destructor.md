# BUG: canonical RC drop lacks a recursive layout destructor

Status: open, fail-closed in canonical SafeWIR

## Finding

The canonical Wasm backend's reference-count decrement reclaims an allocation
when its count reaches zero, but the reclaim path frees only the allocation
shell. It does not walk the authenticated layout and drop initialized owned
reference children.

Emitting that path for a pointer-bearing list, record, sum, dictionary, closure
environment, or frame would leak each owned child. If later code reused the
shell while retaining stale child ownership state, this could also invalidate
the affine accounting expected by SafeWIR.

## Current containment

`Operation::DropRef` is accepted only for reference-counted layouts whose
payload is catalog-proven scalar. Pointer-bearing and unresolved layouts reject
with:

```text
DropRef(pointer-bearing recursive destructor)
```

Legacy `Reclaim` remains migration-only and must not be used to bypass this
refusal.

## Required fix

Implement a versioned layout-driven destructor that:

- visits only initialized fields or elements;
- recursively drops every owned child exactly once;
- ignores borrowed, raw, capability, GC, and static slots according to their
  catalog semantics;
- handles partially initialized aggregate construction and rollback;
- cannot underflow or wrap child reference counts;
- reclaims the parent shell only after child destruction completes; and
- has independent SafeWIR-interpreter and emitted-Wasm probes for nested
  pointer-bearing collections.

After that evidence lands, broaden the `DropRef` checker judgment and remove
standalone `Reclaim` from canonical SafeWIR.
