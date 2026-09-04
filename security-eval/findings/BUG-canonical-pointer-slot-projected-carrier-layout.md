# Canonical pointer slots projected their carrier layout as object type

## Finding

`value_type_for_layout` treated every non-scalar element descriptor as `Ref<descriptor>`. For an
owned `RuntimePointerValueSlot`, that made indexed access produce a reference to the eight-byte
carrier slot rather than a reference to the catalog-authenticated pointee. Correct source values
were rejected, and any future workaround based on matching physical widths would have recreated
the raw-WIR carrier confusion RFC-0165 is intended to remove.

## Resolution

The checker now interprets an owned pointer slot as `Ref<pointee>` directly from the canonical
layout descriptor. Borrowed and raw pointer slots remain unsupported in v1. Exact element slots
carry structural ownership state: `MoveUnique` consumes the SSA token into an empty slot and
`CloneShared` creates an explicitly shared result. Scalar copy transfer of references is rejected.

## Evidence

- `pointer_slot_move_and_clone_preserve_structural_ownership`
- `pointer_slot_rejects_scalar_copy_transfer`
