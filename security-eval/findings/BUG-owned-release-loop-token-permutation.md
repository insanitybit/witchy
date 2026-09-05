# Owned release placement: loop-token permutation limitation

The initial `canonical_owned.rs` release-placement implementation does not yet
handle simultaneous edge renaming when source IDs overlap destination IDs.
In particular, a loop backedge forwarding `(x, y)` to parameters `(y, x)` must
remove both old bindings before installing either destination binding.

The current set update removes and inserts one pair at a time. Starting with
`{x, y}`, it can leave only `{x}` and spuriously report an incompatible owned
edge, or omit a later release that independent final ownership validation must
refuse. No accepted unsafe executable has been demonstrated.

This is an implementation limitation, not a source-language restriction or a
completed ownership-placement claim. It remains to be repaired with a focused
simultaneous-permutation regression. Do not weaken the checker to accept the
missing token, and do not count general loop-carried ownership as covered by
the straight-line or per-iteration temporary-release probes.
