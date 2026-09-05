# BUG-0165: SafeWIR interpreter modeled target-sized values as 64-bit

## Symptom

The reference interpreter accepted 64-bit `Size` arithmetic and reference
counts although canonical SafeWIR lowers `Size` to a target `i32`.

## Root cause

`InterpreterValue::Size` and the runtime reference counter used `u64`
operations, including 64-bit wrap/trap checks and 6-bit shift masks.

## Fix

Keep the public `u64` wrapper for diagnostics, reject values above `u32::MAX`,
and execute `Size` arithmetic and reference counts with 32-bit semantics.

## Residual scope

The reference interpreter remains intentionally fail-closed for aggregate byte
movement and host calls; this repair does not expand those operation semantics.

## Related finding, not repaired here

`MemoryChecker::constant_condition` casts `Size` constants to `i64` for signed
comparisons. Because `Size` is an `i32` bit pattern, a value with bit 31 set
must be interpreted as negative for signed comparisons. This can derive a
wrong branch fact; the centralized checker owner is repairing that separately.
