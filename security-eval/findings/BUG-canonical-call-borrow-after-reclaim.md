# Canonical checker accepts a call borrow after reclaim

Status: confirmed by control-flow inspection on `audit/rfc0165-v1-checker` at `9c94f8f07`.

## Reproducer

A candidate function can perform this sequence in one block:

```text
Alloc object : Ref<PackedList<Int>> (Unique)
Reclaim object
Call callee(Borrow(CallBorrow { source: object, mode: Read }))
```

The callee signature has one `Ref<PackedList<Int>>` parameter with
`ParameterOwnership::CallBorrow { mode: Read }` and `non_escaping: true`, and has
no memory operations. The candidate is otherwise well formed: the allocation
uses `count = 0`, `capacity = 1`, and the catalog stride/header values.

## Why it is accepted

`check_operation` dispatches `Operation::Call` to `check_call` at
`crates/witchy-wir/src/canonical_safewir/check.rs:740-750`. `check_call` checks
the argument type and the declared ownership shape at lines `785-812`, but it
does not consult the `MemoryChecker` or transition/check the source object's
ownership. The preceding `Reclaim` has marked the object `Consumed` in
`check/memory.rs:469-484`; nevertheless the borrow is accepted because
`check_call` only calls `require_type`.

The resulting candidate reaches `check_initial` with a borrow of a reclaimed
object. This is a false authority grant: the backend is authorized to lower a
call whose argument is a stale object reference. The same missing state
transition also affects `CallArgument::Move`, `Share`, and ordinary memory-
bearing `Value` arguments; those paths can retain or consume an object without
updating the caller's live-object state.

## Required correction

Make call checking part of the memory judgment. For every memory-bearing call
argument, require the source to be live and enforce its ownership transition:

- `Borrow`: source live; require the declared non-escaping borrow contract.
- `Move`: source live and consume it at the call boundary; no later caller use.
- `Share`: source live with the required unique/shared transition and refcount
  obligation.
- memory-bearing `Value`: reject unless the contract explicitly defines its
  copy/retain semantics.

Returned aliases and callee effects must also be reflected in the caller's
object state before a later `Release` or `Reclaim` is accepted.

## Focused regression

`crates/witchy-wir/tests/canonical_checker_ownership_audit.rs` contains a
sub-two-second rejection test for the exact borrow-after-reclaim sequence. It
is expected to fail on this audit commit until the checker is corrected.
