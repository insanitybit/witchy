# BUG: canonical `MoveValue` accepted shared and aliased objects

## Impact

The canonical SafeWIR memory checker copied an object's state to the destination
and marked only the source SSA value consumed. It did not require `Unique`
ownership and did not reject another live SSA binding or stored pointer with the
same object identity. A later use of that physical alias could therefore survive
the purported move.

This is a memory-safety defect in the canonical v1 ownership judgment. It was
found while evaluating whether a fresh object result could cross a call.

## Fix

`MoveValue` now requires a unique source and rejects every alias channel that v1
can currently represent: another live object binding and a live reference stored
in a typed slot. Focused tests pin all three refusal cases. Reference-return
lowering remains unsupported until it performs this checked transfer explicitly.

Future alias-bearing operations must extend this census before they can enter
canonical v1; absence of a modeled channel is not inferred from an ownership
label.
