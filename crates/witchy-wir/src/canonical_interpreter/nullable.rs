//! Nullable owned cells have an explicit absent state. Absence is not an
//! object id and can never escape into the interpreter's SSA value map.

use super::*;

impl Engine<'_> {
    pub(super) fn init_nullable_owned(
        &mut self,
        place: &Place,
        values: &BTreeMap<ValueId, InterpreterValue>,
    ) -> Result<(), InterpretError> {
        // Catalog membership and place typing are independently checked before
        // the oracle accepts a module. Retain dynamic ownership checks here.
        let object = self.object_value(place.object(), values)?;
        let root = self.object(object)?;
        if root.immutable {
            return Err(InterpretError::ImmutableObject(object));
        }
        if root.storage != RuntimeStorage::Allocation(AllocationClass::ReferenceCounted) {
            return Err(InterpretError::ObjectNotReferenceCounted(object));
        }
        if root.references != 1 {
            return Err(InterpretError::ReclaimWithAliases {
                object,
                references: root.references,
            });
        }
        let key = match place.projection() {
            Projection::Field { index } => CellKey::Field(*index),
            Projection::Element { index } => {
                let index = self.size_value(*index, values)?;
                self.check_range(object, index, 1, true)?;
                CellKey::Element(index)
            }
            _ => return Err(InterpretError::UnsupportedOperation("InitNullableOwned place")),
        };
        let root = self.object_mut(object)?;
        if root.cells.contains_key(&key) || root.absent_owned_slots.contains(&key) {
            return Err(InterpretError::UnsupportedOperation("InitNullableOwned initialized slot"));
        }
        root.absent_owned_slots.insert(key);
        Ok(())
    }
}
