# Owned children outside a published list prefix

Status: static checker/backend discrepancy identified on master `b7b9dd8c6`;
candidate correction and seven regression tests implemented. Native library
validation is green; source-pipeline probes and merge evidence remain pending.
No source-reachable corruption or executable admission claim is established by
this finding.

## Counterexample

The candidate operation sequence is:

```text
allocate a reference-counted child
allocate an owned-pointer list with length 0 and capacity 1
store the child into element 0 by moving its owned reference
drop the list without publishing length 1
return
```

`check/memory.rs::require_owned_slots_initialized` accepts zero length without
accounting for the stored child. Its earlier `initialized_owned_slots` shortcut
can similarly miss a capacity-tail store made after prefix completeness was
established. Consuming the child SSA token into storage does not discharge the
container's obligation to release it.

The Wasm destructor visits the logical length, so the unpublished child is not
released. The oracle instead releases stored cells outside that length too,
which can conceal the discrepancy. Nesting the list in another owned object
does not make its hidden child part of the destructor's logical prefix.

This is an ownership-accounting and reclamation discrepancy. It is not evidence
of an out-of-bounds access or a use-after-free.

## Required correction and evidence

- Before either shortcut accepts a counted owned-pointer container at an
  ownership boundary, account for every live stored child and initialized range
  against the current logical prefix, resolving aliases by allocation identity.
- Preserve ordinary store-then-`UpdateCount` construction and ABI-derived prefix
  completeness. Clearing all initialization summaries is not the correction.
- Reject zero-length and previously-complete-prefix near misses; accept the same
  construction after the new element is published.
- Exercise count decrease independently. `UpdateCount` does not release children;
  no new count-decrease bypass has been established by the initial review.
- Keep checker, oracle, and backend implementations independent. Oracle cleanup
  is not evidence that the emitted destructor releases the same children.

## Candidate evidence

The integration owner ran:

```sh
RUSTC_WRAPPER= CC=cc \
CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-reflect-specialization-target \
taskpolicy -c utility cargo nextest run -p witchy-wir --lib --features native
```

Result: 680 passed, zero skipped, including seven new cases covering pending
children, prior completeness, publication, root moves, count shrink, and live
versus moved-out ranges. Log: `/tmp/witchy-owned-prefix-native-wir.log`.

The independent worker's focused run passed 12 tests, including the same seven
new cases. Log: `/private/tmp/witchy-owned-tail-b7-checker-final.log`.

The initial CLI rebuild was deliberately terminated under host memory pressure
and restarted with `CARGO_BUILD_JOBS=1`. Neither an interrupted build nor native
unit tests establish source-pipeline or backend execution agreement.
