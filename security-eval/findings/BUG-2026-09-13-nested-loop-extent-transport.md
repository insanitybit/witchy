# Nested-loop extent invalidation loses allocation-capacity transport

Status: confirmed fail-closed compiler regression; correction and final evidence pending.

## Reproduction and evidence

The staged `nested_loop_extents_reload_current_list_and_string_on_backedge`
probe on `impl/rfc0165-canonical-dir-metadata`, based on `ac124234c`, appends to
a string list, preserves an earlier value snapshot, and executes an inner loop
that changes only an integer. It is ordinary value-semantic source code.

Run `edd22326-b4d7-4088-a83b-cb6bbe3124fb` in
`/private/tmp/witchy-dir-metadata-integrated-r4.log` records the refusal. The
post-ownership-fixpoint diagnostic in `/private/tmp/witchy-loop-ir-backedge.txt`
states:

```text
canonical shared phi refused at BlockId(1): object ValueId(41)
extent Counted { header_bytes: 8, count: ValueId(44),
capacity: ValueId(84), stride_bytes: 8 } is not carried by the parameters
InvalidOwnership { function: FunctionId(0), object: ValueId(41) }
```

## Traced cause

- Block 2 allocates object 89 with capacity 84, computed before the inner loop.
- Inner blocks 4, 5 and 6 carry only an integer. Object 89 and its allocation
  capacity definition dominate that loop and remain available afterward.
- Invalidating the lowerer's outer extent cache causes header reloading to
  produce a different capacity definition, 145.
- The backedge from block 6 to block 1 carries
  `[120, 142, 89, 134, 143, 144, 145]`, not allocation-capacity definition 84.
- The checker retains the allocation's capacity identity. Renaming the object
  and count does not rename that uncarried capacity into the header's scope,
  so the shared-phi rule correctly refuses it.

This trace does not establish a physical alias violation or a failed ownership
demotion. The error category alone must not be used to justify changing those
rules. SSA numbers are diagnostic-instance details, not recognition patterns.

## Correction obligations

Preserve or transport version-correct extent facts for unaffected live values,
accounting for actual loop effects and reference replacement. Any equivalence
between a reloaded header and allocation capacity needs independent validation;
do not invent equality or admit an uncarried body-local definition at a phi.

Retain focused backedge, `continue`, `break`, shorter-list replacement and
source/oracle/admitted-Wasm probes. The separate `MissingBoundsGuard` refusal in
`gm_engine.audit_then_flags` is still being traced and is not proven to have
the same cause. No compiler-wide memory-safety conclusion follows from this
finding or its eventual coverage repair.
