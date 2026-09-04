# BUG: Canonical optimizer receipts did not check the claimed rewrite

## Impact

The canonical optimization assurance boundary checked that a refinement receipt
named the enabled pass, exact before/after digests, and one operation location.
It did not independently verify that the candidate at that location was the
result required by the pass's stated refinement mechanism. A buggy or malicious
in-crate optimizer could therefore mint a correctly bound constant-folding
receipt for an arbitrary type-correct replacement. Final SafeWIR validation
would establish memory safety, but not semantic preservation.

Unreachable-block receipts also omitted static-data segments from their direct
pre/post structural comparison. The module digest bound the changed data, but
the pass-specific refinement checker did not establish that pruning preserved
it.

## Resolution

`accept_transform` now invokes a pass-specific relational checker before safety
revalidation. Constant folding reconstructs the one permitted replacement from
the immutable input graph and requires every other function item and module
semantic input to remain exact. Unreachable pruning is rechecked at the same
boundary, and both relations compare static-data segments.

The focused regression constructs a receipt whose hashes and operation location
match a candidate replacing `40 + 2` with `41`; admission now rejects it with
`FoldCandidateMismatch`.
