# Shared capacity must not become logical length

Status: confirmed by code review; focused regression added with the repair.

`shared_phi_meet` conservatively replaces differing capacity identities with
the common logical count. The old `same_scalar` relation subsequently equated
a physical Capacity header load with this conservative bound. Consequently,
`index < physical_capacity` could discharge `index < logical_count` after a
shared join. For length 1, physical capacity 4, and index 2, this could admit
an out-of-length read, including an uninitialized owned-pointer slot.

The repair removes Capacity-header equivalence from the generic scalar
relation. Exact SSA identities and checked equal constants remain valid.
Length-header equivalence remains tied to the same live object's logical
count. No ownership or bounds requirement is removed.

Regression: `shared_phi_capacity_guard_cannot_authorize_read_past_length`
constructs a shared counted pointer-list meet, reads physical capacity,
records the capacity comparison, and requires the element read to fail with
`MissingBoundsGuard`. The shared-extent transport tests separately require
same-object Length evidence and reject foreign headers, arbitrary scalars,
unpromoted prefixes, and unproved distinct-capacity substitutions.

The new shared-edge scalar normalization does not rely on Capacity-header
equivalence. This finding does not establish a source-level exploit or a
compiler-wide safety claim; it identifies an invalid checker judgment.
