# Canonical checker used the wrong width for signed Size comparisons

`MemoryChecker::constant_condition` interpreted both Int and Size constants as
signed i64. Size lowers to a 32-bit Wasm value, so `0x80000000 <s 0` is true,
not false. Incorrect branch folding could skip the memory judgments on an
actually reachable branch. This is a safety-checker defect, not merely an
interpreter discrepancy.

The focused regression constructs an empty counted buffer with capacity one.
The signed comparison selects a branch reading element zero without a bounds
guard. Validation must reject it. An otherwise identical unsigned comparison
selects the safe branch and remains accepted.

The repair uses the operand's explicit SafeWIR width and refuses to infer a
constant condition from out-of-domain Size values. It does not add a range
annotation or trust an optimizer-provided reachability fact. The source type
checker, runtime, and remaining arithmetic/checker judgments need their own
evidence; this narrow repair establishes no compiler-wide safety claim.
