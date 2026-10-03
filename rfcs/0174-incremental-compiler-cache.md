---
rfc: 0174
title: "Incremental compiler cache"
status: proposed
created: 2026-10-03
tracking: "Content-addressed incremental reuse for compiler frontend and admitted artifacts."
---

# RFC-0174: Incremental compiler cache

This RFC defines a Witchy-specific incremental cache. It preserves the
source-first proof boundary and does not require Salsa or change language
semantics.

## Cache products

The implementation may cache these immutable products independently:

1. parsed bundled modules;
2. standard-library interface summaries;
3. checked generic specializations;
4. canonical admitted artifacts.

Whole `CheckedModule` values are not cache entries until their AST facts have
stable node identities. Pointer-keyed facts must never be reused for another
AST allocation.

## Invalidation key

Every entry is keyed by the exact content and authority that produced it:

```text
compiler fingerprint
standard-library digest
source-bundle digest and entry
module-owner digest
precompiled-declaration digest
compiler-session digest
optimization policy and target ABI
```

Dependency records must be stored with each entry. A changed dependency evicts
or bypasses every dependent result. Failed, partial, corrupt, or mismatched
entries are cache misses and never accepted as compiler evidence.

## Representation requirements

Use the smallest data structure that matches each workload. Hash maps and sets
should use the repository's measured `hashbrown`/`foldhash` choices where
appropriate. Read-mostly persistent indexes may use an mmap-friendly format
only after matched cold/warm measurements show a benefit; memory mapping is
not a correctness or performance assumption. Cache writes are atomic and
bounded by an explicit size and entry policy.

## Authority boundary

A cache hit may reuse computation, but it does not mint authority. The caller
must still establish the requested source digest, owner assignments, compiler
session, declarations, target, and policy. Final artifact use remains behind
canonical SafeWIR admission and authenticated host-import validation.

## Delivery and evidence

Implementation proceeds in this order:

1. add query keys, dependency manifests, hit/miss counters, and corruption tests;
2. persist bundled parsing and interface summaries;
3. introduce stable module and AST node IDs;
4. cache specialization and canonical artifact products;
5. measure a cold/warm/cross-process matrix against the slow workspace tests.

The proposal is successful only if warm and cross-process workloads improve
without a material cold-start regression, stale-result acceptance, output
non-determinism, or unbounded memory/disk growth. Salsa remains an optional
backend decision after this measured interface exists.
