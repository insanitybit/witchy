# MoveRange completeness used a narrower identity than its destructive update

Independent review found a static counterexample in the canonical memory
checker: a populated pointer list can be renamed with `MoveValue`, subjected
to a zero-length `MoveRange`, and then shrunk to zero with `UpdateCount`.

`require_source_range_complete` examined only slot and range facts keyed by the
current SSA token. The subsequent `move_range` erased facts for the entire
allocation identity. The live child was still recorded under the consumed
source token, so the completeness check missed it and the deletion erased it.
The length update then saw no child preventing shrink. Wasm's zero-iteration
transfer leaves the pointer untouched; destruction using logical length skips
the child. The independent oracle's release of all recorded cells can conceal
this leak, so oracle agreement alone is not sufficient evidence here.

The correction checks the same allocation identity that the destructive
update uses. A complete-owned-prefix summary also requires the whole logical
prefix, even if individual facts have not been materialized. It does not infer
child uniqueness from container uniqueness or grant partial-transfer support.

Regression candidates cover zero and partial moves after SSA renaming, valid
full and empty transfers, and a white-box summarized-prefix obligation. The
summary test establishes the predicate requirement, not a claimed executable
ABI transition. Validation results and any executable leak reproduction must
be recorded separately; the original counterexample was established by static
checker/backend/oracle inspection, not by executing an exploit.

## Corrected focused evidence (2026-09-12)

The five new checker fixtures now contain valid store guards, child-reference
slice types, and a real entry block. All five passed in the integrated native
run `93840add-5141-4a1e-9ef5-dc40fd8a7e7c`: zero and partial transfers through a
renamed source reject specifically as `MoveRange(source-partial)`; complete and
empty transfers remain accepted; an unmaterialized owned-prefix summary cannot
be erased by an empty transfer. Existing range/update-count controls and the
admitted pointer-list move/clone execution probe also passed.

The complete selected batch was 38/39, not green: the unrelated new bare-driver
diagnostic assertion expected `Nil` while the correct rejection prints `()`.
Log: `/private/tmp/witchy-driver-range-integration-20260912.log`.
The original pre-fix ownership-erasure chain remains a static finding, not an
executed pre-fix exploit. Full native WIR validation and landing are separate
obligations; neither is implied by this focused result.
