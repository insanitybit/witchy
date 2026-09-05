# Owned release placement: loop-token permutation defect

The initial `canonical_owned.rs` release-placement implementation did not
handle simultaneous edge renaming when source IDs overlapped destination IDs.
In particular, a loop backedge forwarding `(x, y)` to parameters `(y, x)` must
remove both old bindings before installing either destination binding.

The old set update removed and inserted one pair at a time. Starting with
`{x, y}`, it can leave only `{x}` and spuriously report an incompatible owned
edge, or omit a later release that independent final ownership validation must
refuse. No accepted unsafe executable has been demonstrated.

The repair collects the entire edge permutation, removes every source token,
and only then installs destination tokens. The focused
`owned_releases_apply_loop_token_permutations_simultaneously` regression carries
two owned references through a swapping loop backedge and proves both tokens
remain accounted for without injected drops. This closes the specific defect;
it is not evidence for every loop-carried ownership shape.
