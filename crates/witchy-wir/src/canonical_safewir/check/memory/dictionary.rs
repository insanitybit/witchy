use crate::canonical_safewir::{
    AllocationClass, DictionaryHeaderField, DictionaryOperation,
    DictionaryRehashTransfer, DictionarySlotField, Extent, LoadTransfer,
    Ownership, StoreTransfer, ValueId, ValueType,
};
use crate::layout::{LayoutId, LayoutKind, PointerSlot, ScalarKind};

use super::{InitialCheckError, MemoryChecker, ObjectState, value_type};

impl MemoryChecker<'_> {
    fn dictionary_affine_child(
        &self, dict: ValueId, key: bool,
    ) -> Option<super::super::AffinePhysicalPlan> {
        let super::super::AffinePhysicalPlan::Mapping { key: key_plan, value: value_plan } =
            self.state.affine_plans.get(&dict)? else { return None };
        (if key { key_plan } else { value_plan })
            .as_ref().map(|(_, plan)| plan.as_ref().clone())
    }

    fn dictionary_layout(&self, dict: ValueId) -> Result<(LayoutId, LayoutId, LayoutId), InitialCheckError> {
        let root = self.live_object(dict)?;
        if !root.initialized_owned_slots {
            return self.unsupported("Dictionary(uninitialized table root)");
        }
        if let Some(expected @ super::super::AffinePhysicalPlan::Mapping { .. }) =
            self.affine_abi.and_then(|proofs| proofs.layouts.get(&root.layout))
            && self.state.affine_plans.get(&dict) != Some(expected)
        {
            return self.unsupported("Dictionary(missing source-bound affine table proof)");
        }
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

    fn dictionary_take_result(
        &mut self, destination: ValueId, slot: LayoutId,
        affine_child: Option<super::super::AffinePhysicalPlan>,
    ) -> Result<(), InitialCheckError> {
        let ty = self.dictionary_slot_type(slot)?;
        self.require_type(destination, ty)?;
        if let ValueType::Ref(layout) = ty {
            let extent = self.parameter_extent(layout)?;
            self.state.objects.insert(destination, ObjectState {
                identity: destination, owner_generation: 0, layout, extent,
                ownership: if affine_child.is_some() { Ownership::Unique } else { Ownership::Shared },
                allocation_class: Some(AllocationClass::ReferenceCounted),
                shared_immutable: false, initialized_owned_slots: true,
            });
            if let Some(plan) = affine_child { self.seed_affine_plan(destination, &plan); }
        }
        Ok(())
    }

    fn dictionary_store_value(
        &mut self, input: ValueId, slot: LayoutId, transfer: StoreTransfer,
        affine_child: Option<super::super::AffinePhysicalPlan>,
    ) -> Result<(), InitialCheckError> {
        let ty = self.dictionary_slot_type(slot)?;
        self.require_type(input, ty)?;
        if let Some(plan) = &affine_child
            && (transfer != StoreTransfer::MoveUnique || !self.require_affine_plan(input, plan)?)
        {
            return self.unsupported("Dictionary(affine child without recursive move proof)");
        }
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
                if !super::drop_ref_layout_supported(self.layouts, *layout, &mut super::SortedSet::new()) {
                    return self.unsupported("Dictionary(Initialize without recursive drop proof)");
                }
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout: *layout,
                    extent: Extent::Counted { header_bytes: 36, stride_bytes: 33, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
                self.attach_affine_plan(*destination, *layout);
            }
            DictionaryOperation::ReadHeader { destination, dict, field } => {
                self.dictionary_layout(*dict)?;
                self.require_type(*destination, ValueType::Size)?;
                let _ = field;
            }
            DictionaryOperation::ProbeGroup { matching, special, first_event, dict, bucket, fingerprint } => {
                self.dictionary_layout(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                self.require_type(*fingerprint, ValueType::Size)?;
                for destination in [matching, special, first_event] {
                    self.require_type(*destination, ValueType::Size)?;
                }
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
                        let affine_child = self.dictionary_affine_child(*dict, *field == DictionarySlotField::Key);
                        if affine_child.is_some()
                            && !(*field == DictionarySlotField::Key
                                && *transfer == LoadTransfer::BorrowAffine
                                && self.state.pending_affine_borrow.is_none())
                        {
                            return self.unsupported("Dictionary(cannot clone affine child)");
                        }
                        let slot = if *field == DictionarySlotField::Key { key } else { value };
                        let ty = self.dictionary_slot_type(slot)?;
                        self.require_type(*destination, ty)?;
                        if let ValueType::Ref(layout) = ty {
                            if !matches!(transfer, LoadTransfer::CloneShared | LoadTransfer::BorrowAffine) {
                                return self.unsupported("Dictionary(reference read transfer)");
                            }
                            if *transfer == LoadTransfer::BorrowAffine && affine_child.is_none() {
                                return self.unsupported("Dictionary(borrow requires affine child proof)");
                            }
                            self.state.objects.insert(*destination, ObjectState {
                                identity: *destination, owner_generation: 0, layout,
                                extent: self.parameter_extent(layout)?, ownership: Ownership::Shared,
                                allocation_class: (*transfer == LoadTransfer::CloneShared)
                                    .then_some(AllocationClass::ReferenceCounted),
                                shared_immutable: false, initialized_owned_slots: true,
                            });
                            if let Some(plan) = affine_child {
                                self.seed_affine_plan(*destination, &plan);
                                self.state.pending_affine_borrow = Some((*destination, *dict));
                            }
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
                self.dictionary_store_value(*key, key_slot, *key_transfer, self.dictionary_affine_child(*dict, true))?;
                self.dictionary_store_value(*value, value_slot, *value_transfer, self.dictionary_affine_child(*dict, false))?;
            }
            DictionaryOperation::ReplaceValue { displaced, dict, bucket, value, transfer } => {
                let (_, _, value_slot) = self.dictionary_layout(*dict)?;
                self.dictionary_unique(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                self.dictionary_store_value(*value, value_slot, *transfer, self.dictionary_affine_child(*dict, false))?;
                self.dictionary_take_result(*displaced, value_slot, self.dictionary_affine_child(*dict, false))?;
            }
            DictionaryOperation::RemoveSlot { removed_key, removed_value, dict, bucket } => {
                let (_, key_slot, value_slot) = self.dictionary_layout(*dict)?;
                self.dictionary_unique(*dict)?;
                self.require_type(*bucket, ValueType::Size)?;
                if removed_key.is_none() && self.dictionary_affine_child(*dict, true).is_some() {
                    return self.unsupported("Dictionary(remove would discard affine key)");
                }
                if let Some(removed_key) = removed_key {
                    self.dictionary_take_result(*removed_key, key_slot, self.dictionary_affine_child(*dict, true))?;
                }
                self.dictionary_take_result(*removed_value, value_slot, self.dictionary_affine_child(*dict, false))?;
            }
            DictionaryOperation::CloneExact { destination, source, capacity } => {
                if matches!(self.state.affine_plans.get(source),
                    Some(super::super::AffinePhysicalPlan::Mapping { .. })) {
                    return self.unsupported("Dictionary(cannot clone affine table)");
                }
                let (layout, _, _) = self.dictionary_layout(*source)?;
                self.require_type(*capacity, ValueType::Size)?;
                self.require_ref_type(*destination, layout)?;
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout,
                    extent: Extent::Counted { header_bytes: 36, stride_bytes: 33, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
                self.attach_affine_plan(*destination, layout);
            }
            DictionaryOperation::Rehash { destination, source, capacity, transfer } => {
                if *transfer == DictionaryRehashTransfer::CloneShared
                    && matches!(self.state.affine_plans.get(source),
                        Some(super::super::AffinePhysicalPlan::Mapping { .. })) {
                    return self.unsupported("Dictionary(cannot clone affine table)");
                }
                let (layout, _, _) = self.dictionary_layout(*source)?;
                self.require_type(*capacity, ValueType::Size)?;
                self.require_ref_type(*destination, layout)?;
                if *transfer == DictionaryRehashTransfer::MoveUnique {
                    self.dictionary_unique(*source)?;
                    self.state.objects.get_mut(source).expect("live root").ownership = Ownership::Consumed;
                }
                self.state.objects.insert(*destination, ObjectState {
                    identity: *destination, owner_generation: 0, layout,
                    extent: Extent::Counted { header_bytes: 36, stride_bytes: 33, count: *capacity, capacity: *capacity },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                });
                self.attach_affine_plan(*destination, layout);
            }
        }
        Ok(())
    }
}
