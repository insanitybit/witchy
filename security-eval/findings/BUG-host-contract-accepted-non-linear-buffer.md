# Host contract accepted non-linear buffer authority

## Finding

The canonical host checker accepted `HostParameterRole::Buffer` contracts whose
value type was a managed `Gc` reference. A call could therefore describe a
non-linear-memory value as a raw byte slice at the host-contract layer.

The current generated ABI catalog uses linear-memory references for byte
buffers, so this was not an observed generated import. It was nevertheless a
checker invariant gap: an authenticated catalog or future catalog projection
could authorize a value that the raw-slice ABI cannot safely represent.

## Cause

`check_import` verified that a buffer role matched its function signature and
had a known extent, but did not require the role to carry `ValueType::Ref(_)`.

## Fix

Host buffer roles now require a linear-memory `Ref` type. A focused adversarial
test proves that a `Gc` buffer using `Slice` authority is rejected. The existing
memory checker remains responsible for distinguishing raw-byte layouts from
typed or pointer-bearing linear-memory buffers.

## Evidence

`CARGO_TARGET_DIR=/Users/cobrien/workspace/witchy/.codex/worktrees/rfc0165-host-negatives/target-luna-host-negatives cargo test -p witchy-wir --features native canonical_safewir::check::host::tests --lib`

Result: 6 passed, 0 failed.
