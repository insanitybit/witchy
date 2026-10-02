use super::*;

impl<'a> MemoryChecker<'a> {
    pub(super) fn promote_header_extent(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let Some((extent, raw_buffer)) = self.header_extent_in(&self.state, object)? else {
            return Ok(());
        };
        let rebound = if !raw_buffer {
            let Extent::Counted { count, .. } = extent else { unreachable!() };
            // The header and its bounds are authenticated before relating the
            // old complete prefix to this newly loaded logical count.
            let rebound = self.rebind_full_current_prefix(object, count)?;
            self.consolidate_pointer_prefix(object, count)?;
            rebound
        } else {
            None
        };
        let Extent::Counted { count, capacity, .. } = &extent else { unreachable!() };
        let (count, capacity) = (*count, *capacity);
        self.state.objects.get_mut(&object).expect("live object exists").extent = extent;
        // The explicit guard that follows authenticated header reads can
        // precede this lazy promotion. It can name only these fresh lane
        // definitions (older generations cannot name new SSA values), so bind
        // it to this generation without reviving any stale guard.
        self.bind_extent_generation(object)?;
        let current = self.current_generation(object)?;
        for (operator, left, right) in self.state.guards.clone() {
            if self.same_scalar(right, count) || self.same_scalar(right, capacity) {
                self.state
                    .guard_generations
                    .entry((operator, left, right))
                    .or_default()
                    .insert(current);
            }
        }
        if let Some(identity) = rebound
            && self.state.range_states.get(&identity).is_some_and(|range|
                range_is_full_current_prefix(&self.state, &identity, range))
        {
            // Consolidation clears old proofs before replacing any ranges.
            // Retain only the transported range that actually survived it.
            self.state.full_current_prefixes.insert(identity);
        }
        Ok(())
    }

    pub(super) fn invalidate_full_current_prefixes(&mut self, object: ValueId) {
        let allocation = self.state.objects.get(&object).map(|state| state.identity);
        let MemoryState { objects, full_current_prefixes, .. } = &mut self.state;
        full_current_prefixes.retain(|range| {
            range.object != object && !allocation.is_some_and(|allocation|
                objects.get(&range.object).is_some_and(|state| state.identity == allocation))
        });
    }

    /// Retiring a Shared token does not mutate the live allocation or its
    /// initialized children. Keep existing proof only while another checked
    /// managed token names that exact storage. This never creates proof and
    /// deliberately does not consult the possible origins of Shared phis.
    pub(super) fn retire_shared_prefix_token(&mut self, object: ValueId) {
        let survives = self.state.objects.get(&object).is_some_and(|retired| {
            self.state.objects.iter().any(|(value, candidate)| {
                *value != object
                    && candidate.identity == retired.identity
                    && candidate.layout == retired.layout
                    && matches!(candidate.ownership, Ownership::Unique | Ownership::Shared)
                    && self.is_shared_immutable_storage(*value, candidate)
            })
        });
        if !survives {
            self.invalidate_full_current_prefixes(object);
        }
    }

    pub(super) fn rebind_full_current_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
    ) -> Result<Option<RangeIdentity>, InitialCheckError> {
        let layout = self.live_object(object)?.layout;
        let Some(element) = self.descriptor(layout)?.kind().packed_element_layout() else {
            return Ok(None);
        };
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(None);
        }
        let mut candidates = self.state.full_current_prefixes.iter().filter_map(|identity| {
            let range = self.state.range_states.get(identity)?;
            (range.element == element && self.same_allocation(object, identity.object)
                && range_is_full_current_prefix(&self.state, identity, range))
                .then_some((*identity, *range))
        });
        let Some((old, range)) = candidates.next() else { return Ok(None); };
        if candidates.next().is_some() {
            return self.unsupported("HeaderPrefix(ambiguous complete prefix)");
        }
        let fresh = RangeIdentity { object, start: old.start, length: count };
        if fresh != old && self.state.range_states.contains_key(&fresh) {
            return self.unsupported("HeaderPrefix(complete prefix collision)");
        }
        self.state.full_current_prefixes.remove(&old);
        self.state.range_states.remove(&old);
        // Preserve initialization, Shared child ownership and allocation class
        // verbatim. In particular, do not reseed a Unique managed ABI prefix.
        self.state.range_states.insert(fresh, range);
        Ok(Some(fresh))
    }

    /// Compute only the header-derived extent. Prefix consolidation remains
    /// the caller's obligation, with its original ordering and error behavior.
    pub(super) fn header_extent_in(
        &self,
        facts: &MemoryState,
        object: ValueId,
    ) -> Result<Option<(Extent, bool)>, InitialCheckError> {
        let state = facts.objects.get(&object).ok_or(InitialCheckError::InvalidOwnership {
            function: self.function.id,
            object,
        })?;
        if state.ownership == Ownership::Consumed {
            return Err(InitialCheckError::UseAfterConsume { function: self.function.id, object });
        }
        let Extent::HeaderPrefix { minimum_bytes } = state.extent else {
            return Ok(None);
        };
        let LayoutSize::Dynamic { base, stride } = self.descriptor(state.layout)?.size() else {
            return self.invalid_projection(object);
        };
        if minimum_bytes != base || stride == 0 {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        let count = facts.header_values
            .get(&(object, HeaderField::Length))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        let is_raw_runtime_buffer = matches!(self.descriptor(state.layout)?.kind(), LayoutKind::RuntimeBuffer { element }
            if self.descriptor(*element).is_ok_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)));
        if is_raw_runtime_buffer {
            if !Self::allocation_fits_in(facts, count, base, stride) {
                return Err(InitialCheckError::MissingAllocationSizeGuard {
                    function: self.function.id,
                    capacity: count,
                    maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
                });
            }
            return Ok(Some((Extent::Counted {
                header_bytes: base,
                count,
                capacity: count,
                stride_bytes: stride,
            }, true)));
        }
        let capacity = facts.header_values
            .get(&(object, HeaderField::Capacity))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        if !Self::less_equal_in(facts, count, capacity) {
            return Err(InitialCheckError::MissingCapacityGuard {
                function: self.function.id,
                count,
                capacity,
            });
        }
        if !Self::allocation_fits_in(facts, capacity, base, stride) {
            return Err(InitialCheckError::MissingAllocationSizeGuard {
                function: self.function.id,
                capacity,
                maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
            });
        }
        Ok(Some((Extent::Counted {
            header_bytes: base,
            count,
            capacity,
            stride_bytes: stride,
        }, false)))
    }

    pub(super) fn update_count(&mut self, object: ValueId, count: ValueId) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        if self.state.static_data_objects.contains(&object) {
            return Err(InitialCheckError::WritableStaticData {
                function: self.function.id,
                object,
            });
        }
        let state = self.live_object(object)?.clone();
        if state.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        let Extent::Counted { header_bytes, count: old_count, capacity, stride_bytes } = state.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        };
        if self
            .state.header_values
            .get(&(object, HeaderField::Capacity))
            .is_some_and(|loaded| *loaded != capacity)
        {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        if !self.less_equal(count, capacity) {
            return Err(InitialCheckError::MissingCapacityGuard {
                function: self.function.id,
                count,
                capacity,
            });
        }
        let mut complete_scalar_prefix = None;
        if !self.same_scalar(old_count, count) {
            // A complete value proves only its old logical prefix. Materialize
            // that prefix before invalidating the summary; newly added slots
            // must be justified by the ordinary store/range rules below.
            if let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout() {
                if matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. } | LayoutKind::Scalar(ScalarKind::Unit)) {
                    self.seed_managed_prefix(object, old_count, element)?;
                } else {
                    self.require_scalar_range_element(element, "UpdateCount(uninitialized scalar prefix)")?;
                    if !self.scalar_prefix_initialized(object, count, element) {
                        return self.unsupported("UpdateCount(uninitialized scalar prefix)");
                    }
                    complete_scalar_prefix = Some(element);
                }
            }
            for candidate in self.state.objects.values_mut() {
                if candidate.identity == state.identity {
                    candidate.initialized_owned_slots = false;
                }
            }
        }
        self.consolidate_pointer_prefix(object, count)?;
        if complete_scalar_prefix.is_some() {
            for candidate in self.state.objects.values_mut() {
                if candidate.identity == state.identity {
                    candidate.initialized_owned_slots = true;
                }
            }
        }
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        };
        self.state.header_values.insert((object, HeaderField::Length), count);
        if let Some(element) = complete_scalar_prefix {
            let aliases = self
                .state
                .objects
                .iter()
                .filter_map(|(value, candidate)| (candidate.identity == state.identity).then_some(*value))
                .collect::<SortedSet<_>>();
            self.state
                .initialized_scalar_slots
                .retain(|(container, _)| !aliases.contains(container));
            self.state
                .range_states
                .retain(|identity, _| !aliases.contains(&identity.object));
            self.state.range_states.insert(
                RangeIdentity { object, start: count, length: count },
                RangeState {
                    element,
                    ownership: state.ownership,
                    allocation_class: state.allocation_class,
                    initialized: true,
                    zero_width: self.is_zero(count),
                    zero_start: true,
                },
            );
        }
        Ok(())
    }

    /// A managed ABI entry parameter (`initialized_owned_slots`) denotes a
    /// complete logical value: every slot below its runtime count holds an
    /// owned reference-counted element. When such an object has no range
    /// fact yet (its count is read at runtime, never built by stores), seed
    /// the whole initialized prefix from that contract. Returns whether a
    /// range was seeded.
    pub(super) fn seed_managed_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
        element: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let state = self.live_object(object)?;
        let current_count = match state.extent {
            Extent::Counted { count, .. } => Some(count),
            Extent::HeaderPrefix { .. } => self.state.header_values.get(&(object, HeaderField::Length)).copied(),
            _ => None,
        };
        if !state.initialized_owned_slots
            || !current_count.is_some_and(|current| self.same_scalar(current, count))
            || self.state.range_states.iter().any(|(identity, range)| {
                // MoveValue changes the token, not the initialized storage.
                // Reuse its prefix, including Shared child status, rather
                // than inventing a second, apparently unique prefix.
                self.same_allocation(identity.object, object)
                    && range.initialized && range.zero_start
                    && range.ownership != Ownership::Consumed
                    && !range.zero_width
            })
        {
            return Ok(false);
        }
        self.invalidate_full_current_prefixes(object);
        self.state.range_states.insert(
            RangeIdentity { object, start: count, length: count },
            RangeState {
                element,
                ownership: Ownership::Unique,
                allocation_class: Some(AllocationClass::ReferenceCounted),
                initialized: true,
                zero_width: false,
                zero_start: true,
            },
        );
        Ok(true)
    }

    pub(super) fn consolidate_pointer_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.invalidate_full_current_prefixes(object);
        let layout = self.live_object(object)?.layout;
        let Some(element) = self.descriptor(layout)?.kind().packed_element_layout() else {
            return Ok(());
        };
        if self.descriptor(element)?.kind() == &LayoutKind::Scalar(ScalarKind::Unit) {
            if !self.scalar_prefix_initialized(object, count, element) { return self.unsupported("UpdateCount(uninitialized Unit)"); }
            self.state.objects.get_mut(&object).expect("live object exists").initialized_owned_slots = true;
            return Ok(());
        }
        // Only the catalog's owned pointer slot is a v1 aggregate whose

        // initialized prefix carries object ownership. Raw bytes and boolean
        // value slots are pointer-free storage, not SafeWIR value types, and
        // therefore have no prefix ownership facts to consolidate.
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(());
        }
        let pointee = self.owned_pointer_slot_pointee(element)?;
        if self.is_zero(count) {
            // The zero-prefix replacement below erases facts by allocation
            // identity, so its precondition must cover exactly that identity.
            // A moved root cannot hide live edges under its old SSA key.
            if self.state.stored_objects.iter().any(|((stored_object, _), stored)|
                self.same_allocation(object, *stored_object) && stored.ownership != Ownership::Consumed)
                || self.state.range_states.iter().any(|(identity, state)| {
                    self.same_allocation(object, identity.object)
                        && state.initialized && state.ownership != Ownership::Consumed
                        && !state.zero_width && !self.is_zero(identity.length)
                })
            {
                return self.unsupported("UpdateCount(pointer shrink with live elements)");
            }
            {
                let object_identity = self.state.objects.get(&object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    objects.get(&identity.object).map(|state| state.identity) != object_identity
                });
            }
            self.state.range_states.insert(
                RangeIdentity { object, start: count, length: count },
                RangeState {
                    element,
                    ownership: Ownership::Unique,
                    allocation_class: None,
                    initialized: true,
                    zero_width: true,
                    zero_start: true,
                },
            );
            return Ok(());
        }
        if self.seed_managed_prefix(object, count, element)? {
            return Ok(());
        }
        let existing = self.state.range_states.iter().find_map(|(identity, state)| {
            (self.same_allocation(identity.object, object) && state.initialized && state.zero_start
                && state.ownership != Ownership::Consumed)
                .then_some((*identity, *state))
        }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation: "UpdateCount(pointer prefix missing)",
        })?;
        if self.same_scalar(existing.0.length, count) {
            if existing.0.length != count || existing.0.object != object {
                self.state.range_states.remove(&existing.0);
                self.state.range_states.insert(
                    RangeIdentity { object, start: existing.0.start, length: count },
                    existing.1,
                );
            }
            return Ok(());
        }

        let one_step = self.state.exact_additions.get(&count).is_some_and(|(left, right)| {
            (self.same_scalar(*left, existing.0.length) && self.state.constants.get(right) == Some(&1))
                || (self.same_scalar(*right, existing.0.length) && self.state.constants.get(left) == Some(&1))
        }) || self.constant_relation(existing.0.length, count, |old, new| {
            old.checked_add(1) == Some(new)
        });
        if one_step {
            let slot = self.slot_key(object, existing.0.length).1;
            // Only the newly published edge is consolidated. Other stored
            // slots remain visible to the existing completeness checks.
            let key = self.state.stored_objects.keys().copied().find(|(container, candidate)| {
                self.same_allocation(object, *container) && *candidate == slot
            }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "UpdateCount(pointer slot uninitialized)",
            })?;
            let stored = self.state.stored_objects.remove(&key).ok_or(
                InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "UpdateCount(pointer slot uninitialized)",
                },
            )?;
            if stored.layout != pointee {
                return self.unsupported("UpdateCount(pointer slot layout)");
            }
            if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared) {
                return self.unsupported("UpdateCount(pointer slot ownership)");
            }
            if existing.1.allocation_class.is_some()
                && existing.1.allocation_class != stored.allocation_class
            {
                return self.unsupported("UpdateCount(pointer allocation class)");
            }
            self.state.range_states.remove(&existing.0);
            self.state.range_states.insert(
                RangeIdentity { object, start: existing.0.start, length: count },
                RangeState {
                    element,
                    // A retained shared edge stays shared in the published
                    // prefix; appending cannot restore exclusivity.
                    ownership: if stored.ownership == Ownership::Shared {
                        Ownership::Shared
                    } else { existing.1.ownership },
                    allocation_class: stored.allocation_class,
                    initialized: true,
                    zero_width: self.is_zero(count),
                    zero_start: existing.1.zero_start,
                },
            );
            return Ok(());
        }

        let Some(total) = self.state.constants.get(&count).copied() else {
            return self.unsupported("UpdateCount(pointer prefix gap)");
        };
        if !self.is_zero(existing.0.length) {
            return self.unsupported("UpdateCount(pointer prefix gap)");
        }
        let mut allocation_class = None;
        let mut shared_child = false;
        let mut keys = Vec::new();
        for index in 0..total {
            let key = (object, SlotIdentity::Constant(index));
            let stored = self.state.stored_objects.get(&key).ok_or(
                InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "UpdateCount(pointer slot uninitialized)",
                },
            )?;
            if stored.layout != pointee {
                return self.unsupported("UpdateCount(pointer slot layout)");
            }
            if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared) {
                return self.unsupported("UpdateCount(pointer slot ownership)");
            }
            shared_child |= stored.ownership == Ownership::Shared;
            if allocation_class.is_some() && allocation_class != stored.allocation_class {
                return self.unsupported("UpdateCount(pointer allocation class)");
            }
            allocation_class = stored.allocation_class;
            keys.push(key);
        }
        for key in keys {
            self.state.stored_objects.remove(&key);
        }
        self.state.range_states.remove(&existing.0);
        self.state.range_states.insert(
            RangeIdentity { object, start: existing.0.start, length: count },
            RangeState {
                element,
                ownership: if shared_child { Ownership::Shared } else { Ownership::Unique },
                allocation_class,
                initialized: true,
                zero_width: self.is_zero(count),
                zero_start: existing.1.zero_start,
            },
        );
        Ok(())
    }

    /// A move-only list's source ABI or an exact locally checked sequence of
    /// unique stores establishes exclusive elements. A complete root alone
    /// does not: ordinary lists may retain shared child references.
    pub(super) fn exclusive_list_elements(&self, object: ValueId) -> Result<bool, InitialCheckError> {
        let root = self.live_object(object)?;
        if root.ownership != Ownership::Unique || !self.is_sole_live_token(object, root.identity)
            || !self.state.affine_list_roots.iter().any(|candidate| self.same_allocation(object, *candidate))
        {
            return Ok(false);
        }
        if self.state.range_states.iter().any(|(range, state)| self.same_allocation(object, range.object)
            && state.initialized && state.ownership != Ownership::Unique && !state.zero_width)
            || self.state.stored_objects.iter().any(|((container, _), state)| self.same_allocation(object, *container)
                && state.ownership != Ownership::Unique)
        {
            return Ok(false);
        }
        Ok(true)
    }

    pub(super) fn attach_affine_plan(&mut self, object: ValueId, layout: crate::layout::LayoutId) {
        let Some(proofs) = self.affine_abi else { return };
        if let Some(plan) = proofs.layouts.get(&layout) {
            self.state.affine_plans.insert(object, plan.clone());
        }
    }

    pub(super) fn seed_affine_plan(&mut self, object: ValueId, plan: &super::super::AffinePhysicalPlan) {
        self.state.affine_plans.insert(object, plan.clone());
        match plan {
            super::super::AffinePhysicalPlan::Leaf => {}
            super::super::AffinePhysicalPlan::NullableFields { fields, .. } => for (field, _, _) in fields {
                let key = (object, SlotIdentity::Constant(u64::from(*field)));
                self.state.nullable_unique_slots.insert(key);
                self.state.nullable_present_slots.insert(key);
            },
            super::super::AffinePhysicalPlan::Sequence { .. } => {
                self.state.affine_list_roots.insert(object);
            }
        }
    }

    pub(super) fn require_affine_plan(
        &self, object: ValueId, plan: &super::super::AffinePhysicalPlan,
    ) -> Result<bool, InitialCheckError> {
        let root = self.live_object(object)?;
        let trivial_raw_leaf = matches!(plan, super::super::AffinePhysicalPlan::Leaf)
            && self.is_raw_byte_buffer(root.layout)?;
        if root.ownership != Ownership::Unique || !self.is_sole_live_token(object, root.identity)
            || (!trivial_raw_leaf && self.state.affine_plans.get(&object) != Some(plan))
        { return Ok(false); }
        match plan {
            super::super::AffinePhysicalPlan::Leaf => Ok(true),
            super::super::AffinePhysicalPlan::NullableFields { fields, allow_absent } => Ok(fields.iter().all(|(field, _, _)| {
                let slot = SlotIdentity::Constant(u64::from(*field));
                self.nullable_unique_slot_by_identity(object, slot)
                    && (*allow_absent || self.nullable_present_slot_by_identity(object, slot))
            })),
            super::super::AffinePhysicalPlan::Sequence { .. } => self.exclusive_list_elements(object),
        }
    }

    pub(super) fn invalidate_affine_list_root(&mut self, object: ValueId) {
        let aliases = self.state.affine_list_roots.iter().copied()
            .filter(|root| self.same_allocation(object, *root)).collect::<Vec<_>>();
        for root in aliases { self.state.affine_list_roots.remove(&root); }
    }

    pub(super) fn slot_key(&self, object: ValueId, index: ValueId) -> (ValueId, SlotIdentity) {
        (
            object,
            self.state.constants.get(&index).copied()
                .map(SlotIdentity::Constant)
                .unwrap_or(SlotIdentity::Dynamic(index)),
        )
    }

    pub(super) fn move_value(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        self.require_type(destination, source_type)?;
        let state = self.live_object(source)?.clone();
        if state.ownership != Ownership::Unique
            || !self.is_sole_live_token(source, state.identity)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        // The physical nullable-slot proof belongs to the sole moving
        // owner. Re-key it before a later unique phi rebases that owner's
        // allocation identity; leaving it on the consumed SSA name would
        // make an otherwise valid consuming take lose its proof.
        let nullable_slots = self.state.nullable_unique_slots.iter().copied()
            .filter(|(container, _)| self.same_allocation(source, *container))
            .collect::<Vec<_>>();
        for (container, slot) in nullable_slots {
            self.state.nullable_unique_slots.remove(&(container, slot));
            self.state.nullable_unique_slots.insert((destination, slot));
        }
        let present_slots = self.state.nullable_present_slots.iter().copied()
            .filter(|(container, _)| self.same_allocation(source, *container))
            .collect::<Vec<_>>();
        for (container, slot) in present_slots {
            self.state.nullable_present_slots.remove(&(container, slot));
            self.state.nullable_present_slots.insert((destination, slot));
        }
        if self.state.affine_list_roots.remove(&source) {
            self.state.affine_list_roots.insert(destination);
        }
        if let Some(plan) = self.state.affine_plans.remove(&source) {
            self.state.affine_plans.insert(destination, plan);
        }
        self.state.objects.insert(destination, state);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    pub(super) fn is_shared_immutable_storage(&self, object: ValueId, state: &ObjectState) -> bool {
        Self::is_managed_storage(state) || self.state.static_data_objects.contains(&object)
    }

    pub(super) fn is_managed_storage(state: &ObjectState) -> bool {
        match state.allocation_class {
            Some(AllocationClass::ReferenceCounted) => true,
            Some(AllocationClass::Bump | AllocationClass::Scratch) => false,
            None => state.shared_immutable,
        }
    }

    pub(super) fn clone_ref(
        &mut self,
        destination: ValueId,
        source: ValueId,
    ) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        let ValueType::Ref(layout) = source_type else {
            return self.unsupported("CloneRef(scalar)");
        };
        self.require_ref_type(destination, layout)?;
        if self.has_detached_owned_field(source)? {
            return self.unsupported("CloneRef(detached owned field)");
        }
        let source_is_static = self.state.static_data_objects.contains(&source);
        let mut state = self.live_object(source)?.clone();
        if !matches!(state.ownership, Ownership::Unique | Ownership::Shared) {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        if !self.is_shared_immutable_storage(source, &state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: source,
            });
        }
        state.ownership = Ownership::Shared;
        self.state.objects.insert(destination, state);
        // The clone names the same storage: the source's loaded header facts
        // hold for it, so a later promotion of the clone uses the same ids.
        let header_values = self
            .state
            .header_values
            .iter()
            .filter(|((object, _), _)| *object == source)
            .map(|((_, field), value)| ((destination, *field), *value))
            .collect::<Vec<_>>();
        self.state.header_values.extend(header_values);
        if source_is_static {
            self.state.static_data_objects.insert(destination);
        }
        self.share_allocation_tokens(source)?;
        Ok(())
    }

    /// A physical retain creates another token even when that token is hidden
    /// inside an opaque owner. Every checker-visible token for the same
    /// allocation loses Unique authority together.
    pub(super) fn share_allocation_tokens(&mut self, source: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?.clone();
        if !matches!(state.ownership, Ownership::Unique | Ownership::Shared) {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: source });
        }
        if !self.is_shared_immutable_storage(source, &state) {
            return Err(InitialCheckError::InvalidAllocationClass { function: self.function.id, object: source });
        }
        for candidate in self.state.objects.values_mut() {
            if candidate.identity == state.identity && candidate.ownership != Ownership::Consumed {
                candidate.ownership = Ownership::Shared;
            }
        }
        for candidate in self.state.stored_objects.values_mut() {
            if candidate.identity == state.identity && candidate.ownership != Ownership::Consumed {
                candidate.ownership = Ownership::Shared;
            }
        }
        self.invalidate_affine_list_root(source);
        Ok(())
    }

    pub(super) fn require_destructor_slots_initialized(
        &mut self,
        object: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        match self.require_owned_slots_initialized(object, operation) {
            Ok(()) => Ok(()),
            Err(error) => {
                if !self.state.objects.get(&object).is_some_and(|state|
                    matches!(state.extent, Extent::HeaderPrefix { .. }))
                {
                    return Err(error);
                }
                // Destruction may be the first operation after header loads:
                // there need not have been a body access to promote this
                // extent. Use the same authenticated promotion as a range
                // access, including its retained-prefix rebinding, then rerun
                // every existing owned-edge/initialization check. Already
                // complete ABI values need no new header or bounds evidence.
                self.promote_header_extent(object)?;
                self.require_owned_slots_initialized(object, operation)
            }
        }
    }

    pub(super) fn release(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        if self.is_borrowed_parameter(object) {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object });
        }
        let state = self.live_object(object)?;
        if state.ownership != Ownership::Shared {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        if !self.is_shared_immutable_storage(object, state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_destructor_slots_initialized(object, "Release(uninitialized owned slot)")?;
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        self.retire_shared_prefix_token(object);
        Ok(())
    }

    pub(super) fn drop_ref(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        if self.is_borrowed_parameter(object) {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object });
        }
        let state = self.live_object(object)?;
        let shared = state.ownership == Ownership::Shared;
        let sole_owner = !shared && self.is_sole_live_token(object, state.identity);
        if !self.is_shared_immutable_storage(object, state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        // The object's layout id is the interner key; re-deriving it from
        // the descriptor would hash the descriptor bytes on every DropRef.
        let layout = state.layout;
        self.require_destructor_slots_initialized(object, "DropRef(uninitialized owned slot)")?;
        if self.layouts.get(layout).is_none() {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        if !drop_ref_layout_supported(self.layouts, layout, &mut SortedSet::new()) {
            return self.unsupported("DropRef(pointer-bearing recursive destructor)");
        }
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        if sole_owner {
            // The destructor consumes every remaining owned edge. Its exact
            // child facts cannot survive into a join with a path that took
            // those edges before destroying the parent.
            let edges = self.state.stored_objects.keys().copied()
                .filter(|(container, _)| self.same_allocation(object, *container))
                .collect::<Vec<_>>();
            for edge in edges {
                self.state.stored_objects.remove(&edge);
            }
        }
        if shared {
            self.retire_shared_prefix_token(object);
        } else {
            self.invalidate_full_current_prefixes(object);
        }
        Ok(())
    }

    pub(super) fn reclaim(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if state.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        if state.allocation_class != Some(AllocationClass::ReferenceCounted) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_owned_slots_initialized(object, "Reclaim(uninitialized owned slot)")?;
        if !self.fixed_pointer_fields(state.layout)?.is_empty() {
            return self.unsupported("Reclaim(pointer-bearing layout requires DropRef)");
        }
        let descriptor = self.descriptor(state.layout)?;
        if let Some(element) = descriptor.kind().packed_element_layout() {
            let element_descriptor = self.descriptor(element)?;
            if matches!(element_descriptor.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
                return self.unsupported("Reclaim(pointer-bearing layout requires DropRef)");
            }
        }
        if self.state.range_states.iter().any(|(range, state)| {
            range.object == object
                && state.initialized
                && !self.is_zero(range.length)
                && self
                    .value_type_for_layout(state.element)
                    .is_ok_and(|value_type| value_type.is_memory_bearing())
        }) {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        self.state.objects.get_mut(&object).expect("live object exists").ownership = Ownership::Consumed;
        Ok(())
    }

    pub(super) fn live_object(&self, object: ValueId) -> Result<&ObjectState, InitialCheckError> {
        let state = self.state.objects.get(&object).ok_or(InitialCheckError::InvalidOwnership {
            function: self.function.id,
            object,
        })?;
        if state.ownership == Ownership::Consumed {
            return Err(InitialCheckError::UseAfterConsume {
                function: self.function.id,
                object,
            });
        }
        Ok(state)
    }
}
