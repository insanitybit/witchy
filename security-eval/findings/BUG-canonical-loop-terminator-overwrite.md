# Canonical loop lowering overwrote terminating body edges

`lower_while` and `lower_range_for` replaced an already-emitted body terminator
with their ordinary backedge. `lower_for` also appended its counter operations
without checking whether the body had terminated. A return that had released an
owned parameter could consequently become a backedge using a consumed token.
The independent owned-release pass refused the generator example with
`IncompatibleOwnedEdge(BlockId(8))`; its safety rule was not the defect.

Correction: build counter increments, carried lanes, and the ordinary backedge
only when the body has no terminator. Preserve return, tail-call, break, and
continue edges exactly as lowered, matching the existing `while let` rule.

Focused evidence belongs to `loop_terminators_preserve_owned_returns_three_legs`
and `generators_and_pascal_expected_output_three_legs` in the oracle shard.
These compare specified output, source execution, SafeWIR interpretation, and
admitted Wasm execution, including reference-counted heap cleanup.
