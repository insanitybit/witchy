use crate::canonical_safewir::{
    AllocationClass, DictionaryHeaderField, DictionaryOperation,
    DictionaryRehashTransfer, DictionarySlotField, Extent, LoadTransfer,
    Ownership, StoreTransfer, ValueId, ValueType,
};
use crate::layout::{LayoutId, LayoutKind, PointerSlot, ScalarKind};

use super::{InitialCheckError, MemoryChecker, ObjectState, value_type};

impl MemoryChecker<'_> {
    fn dictionary_layout(&self, dict: ValueId) -> Result<(LayoutId, LayoutId, LayoutId), InitialCheckError> {
        let root = self.live_object(dict)?;
        let LayoutKind::CanonicalDictionary { key, value, .. } = self.descriptor(root.layout)?.kind() else {
            return self.unsupported("Dictionary(non-dictionary root)");
        };
        Ok((root.layout, *key, *value))
    }

    fn dictionary_slot_type(&self, slot: LayoutId) -> Result<ValueType, InitialCheckError> {
        if matches!(self.descriptor(slot)?.kind(), LayoutKind::Scalar(ScalarKind::BoolValueSlot)) {
            return Ok(ValueType::Bool);
        }
        self.value_type_for_layout(slot)
    }

    fn dictionary_unique(&self, dict: ValueId) -> Result<(), InitialCheckError> {
        let root = self.live_object(dict)?;
        if root.ownership != Ownership::Unique
            || !self.is_sole_live_token(dict, root.identity)
            || root.allocation_class != Some(AllocationClass::ReferenceCounted)
            || root.shared_immutable
        {
            return self.unsupported("Dictionary(write without unique RC root)");
        }
        Ok(())
    }

    fn dictionary_take_result(&mut self, destination: ValueId, slot: LayoutId) -> Result<(), InitialCheckError> {
        let ty = self.dictionary_slot_type(slot)?;
        self.require_type(destination, ty)?;
        if let ValueType::Ref(layout) = ty {
            let extent = self.parameter_extent(layout)?;
            self.state.objects.insert(destination, ObjectState {
                identity: destination, owner_generation: 0, layout, extent,
                ownership: Ownership::Shared,
                allocation_class: Some(AllocationClass::ReferenceCounted),
                shared_immutable: false, initialized_owned_slots: true,
            });
        }
        Ok(())
    }

    fn dictionary_store_value(&mut self, input: ValueId, slot: LayoutId, transfer: StoreTransfer) -> Result<(), InitialCheckError> {
        let ty = self.dictionary_slot_type(slot)?;
        self.require_type(input, ty)?;
        match ty {
            ValueType::Ref(_) => {
                let owner = self.live_object(input)?.clone();
                if owner.allocation_class != Some(AllocationClass::ReferenceCounted) || owner.shared_immutable {
                    return self.unsupported("Dictionary(store non-RC child)");
                }
                match transfer {
                    StoreTransfer::MoveUnique if owner.ownership == Ownership::Unique => {
                        self.state.objects.get_mut(&input).expect("live child").ownership = Ownership::Consumed;
                    }
                    StoreTransfer::CloneShared if owner.ownership == Ownership::Shared => {}
                    _ => return self.unsupported("Dictionary(reference transfer)"),
                }
            }
            _ if transfer == StoreTransfer::CopyScalar => {}
            _ => return self.unsupported("Dictionary(scalar transfer)"),
        }
        Ok(())
    }

    pub(super) fn check_dictionary(&mut self, operation: &DictionaryOperation) -> Result<(), InitialCheckError> {
        match operation {
            DictionaryOperation::Initialize { destination, layout, capacity } => {
                self.require_type(*capacity, ValueType::Size)?;
                self.require_ref_type(*destination, *layout)?;
                if !matches!(self.descriptor(*layout)?.kind(), LayoutKind::CanonicalDictionary { .. }) {
                    return self.unsupported("Dictionary(Initialize layout)");
                }
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout: *layout,
                    extent: Extent::Counted { header_bytes: 20, stride_bytes: 32, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
            }
            DictionaryOperation::ReadHeader { destination, dict, field } => {
                self.dictionary_layout(*dict)?;
                self.require_type(*destination, ValueType::Size)?;
                let _ = field;
            }
            DictionaryOperation::ReadSlot { destination, dict, bucket, field, transfer } => {
                let (_, key, value) = self.dictionary_layout(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                match field {
                    DictionarySlotField::State | DictionarySlotField::Next | DictionarySlotField::Prev => {
                        self.require_type(*destination, ValueType::Size)?;
                        if *transfer != LoadTransfer::CopyScalar { return self.unsupported("Dictionary(scalar read transfer)"); }
                    }
                    DictionarySlotField::Hash => {
                        self.require_type(*destination, ValueType::Int)?;
                        if *transfer != LoadTransfer::CopyScalar { return self.unsupported("Dictionary(hash read transfer)"); }
                    }
                    DictionarySlotField::Key | DictionarySlotField::Value => {
                        let slot = if *field == DictionarySlotField::Key { key } else { value };
                        let ty = self.dictionary_slot_type(slot)?;
                        self.require_type(*destination, ty)?;
                        if let ValueType::Ref(layout) = ty {
                            if *transfer != LoadTransfer::CloneShared { return self.unsupported("Dictionary(reference read transfer)"); }
                            self.state.objects.insert(*destination, ObjectState {
                                identity: *destination, owner_generation: 0, layout,
                                extent: self.parameter_extent(layout)?, ownership: Ownership::Shared,
                                allocation_class: Some(AllocationClass::ReferenceCounted),
                                shared_immutable: false, initialized_owned_slots: true,
                            });
                        } else if *transfer != LoadTransfer::CopyScalar {
                            return self.unsupported("Dictionary(scalar read transfer)");
                        }
                    }
                }
            }
            DictionaryOperation::InsertSlot { dict, bucket, key, value, hash, key_transfer, value_transfer } => {
                let (_, key_slot, value_slot) = self.dictionary_layout(*dict)?;
                self.dictionary_unique(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                self.require_type(*hash, ValueType::Int)?;
                self.dictionary_store_value(*key, key_slot, *key_transfer)?;
                self.dictionary_store_value(*value, value_slot, *value_transfer)?;
            }
            DictionaryOperation::ReplaceValue { displaced, dict, bucket, value, transfer } => {
                let (_, _, value_slot) = self.dictionary_layout(*dict)?;
                self.dictionary_unique(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                self.dictionary_store_value(*value, value_slot, *transfer)?;
                self.dictionary_take_result(*displaced, value_slot)?;
            }
            DictionaryOperation::RemoveSlot { removed_value, dict, bucket } => {
                let (_, _, value_slot) = self.dictionary_layout(*dict)?;
                self.dictionary_unique(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                self.dictionary_take_result(*removed_value, value_slot)?;
            }
            DictionaryOperation::CloneExact { destination, source, capacity } => {
                let (layout, _, _) = self.dictionary_layout(*source)?;
                self.require_type(*capacity, ValueType::Size)?;
                self.require_ref_type(*destination, layout)?;
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout,
                    extent: Extent::Counted { header_bytes: 20, stride_bytes: 32, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
            }
            DictionaryOperation::Rehash { destination, source, capacity, transfer } => {
                let (layout, _, _) = self.dictionary_layout(*source)?;
                self.require_type(*capacity, ValueType::Size)?;
                self.require_ref_type(*destination, layout)?;
                if *transfer == DictionaryRehashTransfer::MoveUnique {
                    self.dictionary_unique(*source)?;
                    self.state.objects.get_mut(source).expect("live root").ownership = Ownership::Consumed;
                }
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout,
                    extent: Extent::Counted { header_bytes: 20, stride_bytes: 32, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
            }
        }
        Ok(())
    }
}
