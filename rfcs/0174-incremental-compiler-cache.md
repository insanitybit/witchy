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

## Amendment: stable fact keys (2026-10-05)

Delivery step 3 is the prerequisite for every reuse beyond a single
allocation. This amendment fixes its design and the order of its slices.

### Why per-function replay cannot pay without it

Measured on release builds over the example and RFC-0165 corpus:

- Replaying one function's checked facts costs about 19 µs; checking it fresh
  costs about 10 µs. Stamping each read and validating it on replay is the
  cost, and a body check is a small unit of work.
- Body checks are about a fifth of checking. About 97% of that time is the
  standard library, which is identical in every program and is rechecked
  per program, one function at a time.
- Sharing function bodies between modules (`Arc<Function>` copy-on-write)
  was 5% slower and lost facts. Every whole-module pass writes every
  function, so copies move to the first write. And a fact keyed by a node's
  address is not found at the copy's address.

Content-keyed reuse already works where its key is a digest of its inputs:
the SafeWIR verdict memo replays a function's verdict with one digest
comparison. The checker's facts need the same property.

### Design

A checked fact is stored under a `NodeKey`:

```text
NodeKey { function: FunctionKey, node: u32 }
```

- `FunctionKey` names the owning callable: its declaration name in the
  checked module, or a top-level constant's name. It is unique within one
  module.
- `node` is the node's preorder index within that callable's body. The
  same walk order is used by every producer and consumer. It is stable for a
  body whose content is unchanged, wherever that body is allocated.

The tables keep their accessors. `TypeTable::type_of(&Expr)` and the loan,
access, place, view and optimization accessors keep their signatures. Each
table holds an address index for the one module allocation it was built
against, mapping a node's address to its `NodeKey`, and the facts by
`NodeKey`. An accessor resolves the address through the index, then reads the
fact. Rebinding a table to another allocation of the same content is one walk
that rebuilds the index. It needs no recheck.

Facts for a changed body are discarded with that body's `FunctionKey`.
Unchanged bodies keep their keys, so the per-function incremental annotation
(`forget_expression_facts`, `absorb`) becomes key arithmetic instead of
address arithmetic.

APIs that hand out raw addresses migrate to `NodeKey`:

- `CheckedAccessFacts::call_contracts`;
- `LoanFacts::event_key`;
- `LambdaSite::expr_address`;
- the lowering maps keyed by expression addresses.

The owner-module staleness checks (`ptr::eq` on the retained module) become
checks that the index was built against this allocation.

### Slices

Each slice lands behind the existing verify modes:
`WITCHY_VERIFY_INCREMENTAL_ANNOTATE=1` compares against a whole-module
recheck, and preorder normalization compares facts across allocations.

1. `NodeKey`, the preorder walk, and the address index, used by `TypeTable`
   alone. Output is byte-identical. Address keys remain only inside the
   index.
2. Access, place, loan, view and optimization-readiness facts move to
   `NodeKey` behind their accessors. The raw-address APIs above migrate.
3. Lowering-local maps and task-provenance maps move to `NodeKey`.
4. Rebinding: a checked module's facts follow a copied module through a
   rebuilt index. Whole-module clones in the pipeline no longer force a
   re-annotation.
5. Standard library as one unit: the checked registration and body facts of
   the linked std modules are keyed by their content digest and reused
   across compiles in one process, then through the persistent store.

### Acceptance

- The corpus produces identical Wasm and identical diagnostics with every
  slice, and the verify modes pass on the full corpus.
- After slice 5, checking a program reuses the standard library's facts
  without rechecking any std body. The warm frontend cost of a small
  program falls by at least half against the same build without reuse.
- No fact is read through an index built for another allocation.
