# Owned-prefix summaries must follow content changes

`coven_check` reaches `rights.net_verbs` and fails closed at
`Return(uninitialized owned slot)`: its shared result joins a complete empty
list and a complete two-element string list, but extent demotion loses the
per-predecessor initialization evidence.

Simply setting `initialized_owned_slots` at that join was not sufficient.
The existing summary could seed a requested new count during `UpdateCount`,
and it survived `MoveRange` after elements had left the source. A promise that
the current logical value is complete is not authority to initialize future
slots or to keep using moved contents. Capacity does not establish initialization.

The repair binds prefix seeding to the current counted extent or an
authenticated current length-header read. Count changes first preserve the old
prefix, invalidate same-identity summaries, and require ordinary slot/range
evidence for the new count. Moves invalidate source and destination summaries;
the moved range remains explicit evidence for the destination. Shared phi
summaries are derived from each predecessor's checked current contents before
renaming/demotion, then intersected normally.

Focused native tests passed:

- `owned_prefix_summary_does_not_authorize_count_growth`
- `owned_prefix_summary_is_invalidated_by_move`
- `owned_prefix_summary_at_shared_phi_requires_predecessor_evidence`

A bounded independent review found no concrete defect in these paths. The
tests exercise checker states, including an ABI-style initialized prefix; this
is not a claim of a demonstrated source-level exploit or compiler-wide safety.
CLI parity for hello, wrap, jq, and coven_check passes on the repair candidate.
The exact `coven_check_example_flags_under_declared_manifest_in_witchy` test
also passes; its deferral is removed. Queue validation remains pending.
