# Structural aggregate selection included nominal closure environments

## Reproducer

`rfc0165_admitted_launch::admitted_record_write_back_in_a_loop_takes_a_shared_lane`
combines a single-Int `Counter` record with a capture-free lambda. Production
lowering refused it with `AmbiguousLayout(Tuple([Named("Int", [])]))`.

## Cause

Structural aggregate selection resolved every nominal root to its backing
descriptor before comparing fields. A capture-free closure environment has one
Int tag field, so its identity-bearing nominal layout competed with the concrete
single-Int source record. The same selection pattern existed for packed scalar
records.

## Repair

Structural selectors compare concrete root descriptors without unwrapping
nominals. Source nominal aggregates continue through the existing exact-identity
lookup in `aggregate_layout_for_source`. This does not select the first matching
layout, collapse distinct nominal identities, change pointer-slot typing, or
weaken validation. Ambiguous concrete candidates still fail closed.

## Evidence

The existing queue regression passed: one test, zero failures, 0.31 seconds
after the warmed build. Command: `cargo test -p witchy --test misc
rfc0165_admitted_launch::admitted_record_write_back_in_a_loop_takes_a_shared_lane
-- --exact --nocapture`, with absolute target
`/private/tmp/witchy-rfc0165-drop-worklist-target`.
Log: `/private/tmp/rfc0165-record-closure-layout.log`. Queue landing is pending.
