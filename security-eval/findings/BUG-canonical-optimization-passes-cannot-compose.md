# BUG: canonical optimization passes cannot compose

## Impact

`optimize_unreachable_blocks` and `optimize_constant_folding` both consume
`CanonicalModule<InitialValid>` and each returns a terminal
`OptimizationOutcome`. No API accepts an already checked intermediate revision
and extends its ordered history. The production canonical pipeline can therefore
run only one pass, so its pass-by-pass safety and refinement model is not yet a
composable optimizer.

## Current behavior

The checked-source pipeline runs unreachable pruning only. Constant folding has
focused refinement evidence but cannot follow it without discarding or
reconstructing authority and history.

## Required fix

Introduce an optimizer session that owns the initial checked witness, current
validated revision, and ordered refinement receipts. Each pass must transform
the current validated revision into a candidate, run the common safety checker,
validate its per-instance refinement, and append its receipt. Only the session
may finalize an `OptimizationOutcome`; no pass may reset or splice history.

Tests must compose at least unreachable pruning followed by constant folding,
reject a broken intermediate link, and bind the final history to the exact
initial and final module digests.
