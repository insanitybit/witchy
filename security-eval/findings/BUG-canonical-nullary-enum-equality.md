# Canonical payload-free enum equality refusal

Status: corrected on the enum-comparison integration branch; focused evidence
is in the RFC-0165 acceptance ledger.

Coven's latest_released_footprint compares the State returned by state_of with
Released. Both are references to the same tagged enum layout. The lowerer
handled scalar and String comparisons but rejected this case as
UnsupportedExpression("comparison type"). Comparing allocation addresses would
be incorrect because distinct constructions can represent the same variant.

The correction requires matching resolved source identities and a tagged shape
with no payload fields. It loads the initialized Int discriminator through the
existing checked record-field operation and compares those scalars. Loading the
left tag before RHS evaluation preserves a snapshot across var writeback.

The positive matrix covers every pair of four variants. Additional probes cover
writeback, evaluation order, ownership cleanup, and explicit PartialEq dispatch.
Payload-bearing sums and ordinary records may remain unsupported; if they are
accepted, the tests require their distinct payloads to compare unequal rather
than permitting tag-only equality. No memory-authority rule was weakened.
