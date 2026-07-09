# BUG-500: VM tour claims packed records can cross worker boundaries

Severity: LOW
Status: FIXED
Verified: 2026-07-08 CODE on master 5547bc8
Component: book VM tour, `std/vm`, packed layout, cross-VM marshaling docs

## Problem

The VM tour teaches that a `packed` record can cross a VM boundary directly:

> A general record can't cross a VM boundary directly (its fields are pointers), but a
> `packed` record can (it's flat)...

That is not the model implemented or specified today. The current worker-VM
surface only has native marshaling for scalars and flat buffers (`String` /
`Bytes`), and the current packed-layout contract is explicitly confined-local:
host-visible or cross-function packed ABI is future work.

This is a release-facing language consistency issue because it suggests `packed`
is a serialization or VM wire-format tool. Today the dependable boundary for
structured data is `Bytes`.

## Evidence

- `book/src/tour-vm.md:51-66` says crossing VM boundaries wants `Bytes`, then
  says a `packed` record can cross directly because it is flat.
- `std/vm.witchy:21-43` exposes:
  - `par_map(xs: List(a), f: fn(a) -> b) -> List(b)`
  - `with_dir(dir: Dir, f: fn(Dir, Bytes) -> Bytes, input: Bytes) -> Bytes`
  - `serve(init: Bytes, requests: List(Bytes), handler: fn(Bytes, Bytes) -> Bytes) -> List(Bytes)`
  There is no public worker API that accepts or returns arbitrary packed records
  as a distinct wire payload.
- `crates/witchy-lower/src/codegen/builtins.rs:509-545` only intercepts native
  VM calls for scalar `par_map`, `String`/`Bytes` `par_map`, `with_dir(Bytes)`,
  and `serve(Bytes)`.
- `crates/witchy-lower/src/codegen/mod.rs:6278-6302` defines the native
  `par_map` fast paths as i64-representable scalar types or `String`/`Bytes`.
  Records are not included.
- `crates/witchy-types/src/typeck.rs:721-740` describes packed lists as confined
  local buffers with no cross-function or stored-field ABI, and rejects boundary
  positions for `List(P)` where `P` is declared `packed`.
- `spec/performance.md:163-173` says declared `packed` guarantees "flat or a
  loud error" confined to one function, and that cross-function / host-visible
  packed layout remains future work.

## Expected Direction

Either:

1. Update the VM tour to say structured VM payloads should serialize to `Bytes`
   today, and that `packed` is a local layout/performance contract rather than a
   VM wire format; or
2. Implement and test a host-visible packed ABI and worker-VM marshaling path for
   packed records, then update the spec and `std/vm` docs around that contract.

The first option is the smaller release-polish fix.

## Fix

The VM tour now says structured VM or wire-boundary payloads should choose an
explicit `Bytes` encoding, and that `packed` is a local layout/performance
contract rather than a worker-VM wire format.

## Related

- `BUG-414` covers worker-isolation fallbacks for non-top-level callbacks.
- `BUG-060` covers broader browser-vs-VM runnable documentation issues.
- RFC-0027 and `spec/performance.md` describe the current confined packed layout
  contract.
