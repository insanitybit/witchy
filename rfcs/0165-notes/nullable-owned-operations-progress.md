# Nullable owned operations: sealed implementation evidence

The preceding integration wave landed on master as `297b7b42e` at
2026-09-08 11:40:08 UTC. This follow-up is on
`impl/rfc0165-nullable-operations`; it is not recursive source admission.

## Implemented

- `InitNullableOwned` explicitly initializes absent fixed fields on unique RC
  objects. It cannot overwrite a live edge or duplicate initialization.
- `ClonePresent` has an explicit runtime presence check before retain and
  result use. SSA references remain non-null; absent storage is not a scalar.
- Present stores clear absence through allocation identity, including after a
  root SSA move. Completed fixed-field summaries cannot permit live overwrite.
- The oracle stores absence separately from values. Backend destruction skips
  zero only for nullable slots, preserving non-nullable-zero traps.
- Nullable range transfers and production nullable/opaque catalog admission
  remain fail-closed.

## Focused evidence

Command, from the existing owner worktree:

```sh
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= \
  CARGO_TARGET_DIR=/private/tmp/witchy-rfc0165-drop-worklist-target \
  taskpolicy -c utility cargo test -p witchy-wir --lib --features native nullable_owned
```

Nine tests passed: four checker rules/regressions, two oracle behaviors, two
catalog seals, and one backend destruction probe. Log:
`/private/tmp/rfc0165-nullable-focused-final.log`.

The destructor fixture initially asserted the opposite free-list order.
Reclaim prepends each object: releasing child 64 before parent 40 leaves
`free = 40`, `word[40] = 64`, `word[64] = 0`. The corrected probe checks all
three values; the implementation was not weakened to satisfy the fixture.

A bounded read-only core/oracle review by the Terra worker found no concrete
authority bypass. Its suggested absent/present-join limitation still needs an
executable probe: per-slot absence facts intersect, but complete-object summaries
can also preserve initialization. Do not treat either join coverage or the
whole checker as established by that review.

## Remaining before opening admission

1. Exercise initial/final checking, InitNullableOwned, present stores,
   ClonePresent, mechanical encoding, and exact-byte admission together.
   Internal checker/oracle and destructor fixtures are not this evidence.
2. Add the absent/present branch-join and forged-operation negative probes,
   including runtime trapping before any child header/RC write.
3. Authenticate nominal bindings from checked source, lower recursive sums
   through these operations, and run the deep recursive source corpus.

The ordinary-record consolidation overwrite bug and correction are recorded
in `security-eval/findings/BUG-canonical-fixed-field-overwrite-after-consolidation.md`.
Compiler-wide memory safety is not established.
