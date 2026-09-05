# BUG-0165: Owned pointer containers escaped without complete initialization

Pointer-bearing counted allocations begin uninitialized unless their slots are
filled by `Store` or an authenticated complete `MoveRange`. The checker
rejected an uninitialized `Load`, but previously allowed the same object to be
released, dropped, reclaimed, moved into a call, or returned. Recursive Wasm
destruction visits each owned child slot, so an uninitialized nonempty
container could reach a null child-pointer trap.

The checker now requires either a complete range fact, exact constant facts for
every slot, or an authenticated managed-ABI completeness fact before those
operations. Empty containers remain valid; scalar and POD collections do not
use this rule.
