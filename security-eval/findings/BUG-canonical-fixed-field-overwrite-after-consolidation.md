# Canonical fixed-field overwrite after initialization consolidation

Status: correction in progress on `impl/rfc0165-nullable-operations`; not yet
claimed landed or independently reviewed.

The baseline memory checker consolidates all initialized owned fields into
`ObjectState::initialized_owned_slots`, removing their individual
`stored_objects` entries. Its `Store` overwrite check consulted only those
entries (and counted-prefix facts for lists). A second store into a complete
fixed record could therefore replace an owned edge without releasing it.

This violates owned-edge accounting and leaks the displaced child. It is not
evidence by itself of use-after-free or capability forgery.

The correction rejects a reference-field store into a complete fixed object
unless that exact slot is independently known to contain explicit nullable
absence. Absence is keyed through allocation identity, so moving the root SSA
token cannot leave a stale permission to overwrite the newly stored edge.

Focused regression:
`nullable_owned_checker_keeps_ordinary_completed_fields_non_overwritable`
constructs an ordinary non-nullable record, consolidates its first store,
allocates a second child, and requires the second owning store to reject.
The nullable counterpart exercises absence initialization, root move, a
present store, and refusal to overwrite the resulting live edge.
