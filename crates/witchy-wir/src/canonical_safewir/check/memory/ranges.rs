use super::*;

impl<'a> MemoryChecker<'a> {
    pub(super) fn copy_bytes(
        &mut self,
        destination: &Place,
        source: &Place,
        length: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(length, ValueType::Size)?;
        let destination_length = self.check_byte_slice(destination, AccessMode::Write)?;
        let source_length = self.check_byte_slice(source, AccessMode::Read)?;
        if destination_length != length || source_length != length {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: destination.object(),
            });
        }
        let destination = self.check_typed_range(destination, AccessMode::Write, "CopyBytes")?;
        let source = self.check_typed_range(source, AccessMode::Read, "CopyBytes")?;
        self.require_initialized_scalar_range(source, "CopyBytes(uninitialized source)")?;
        self.mark_initialized_scalar_range(destination);
        Ok(())
    }

    pub(super) fn move_range(
        &mut self,
        destination: &Place,
        source: &Place,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        let destination = self.check_typed_range(destination, AccessMode::Write, "MoveRange")?;
        let source = self.check_typed_range(source, AccessMode::Read, "MoveRange")?;
        self.check_range_pair(destination, source, count, "MoveRange")?;

        if self.value_type_for_layout(destination.element)?.is_memory_bearing() {
            self.require_source_range_complete(source)?;
            let moved = self.require_initialized_range(
                source,
                destination.element,
                "MoveRange(uninitialized-source)",
            )?;
            self.require_destination_range_empty(destination)?;
            let carries_exclusive_elements = self.exclusive_list_elements(source.object)?
                && moved.ownership == Ownership::Unique
                && moved.allocation_class == Some(AllocationClass::ReferenceCounted)
                && self.is_zero(destination.start);
            self.invalidate_affine_list_root(source.object);
            self.invalidate_affine_list_root(destination.object);
            let source_object = source.object;
            let consumed = self.state.stored_objects.keys()
                .filter(|(object, _)| self.same_allocation(source_object, *object))
                .copied()
                .collect::<Vec<_>>();
            for key in consumed {
                self.state.stored_objects.remove(&key);
            }
            {
                // Range facts follow the allocation, not the token: an
                // alias's prefix fact must not survive the move of the
                // elements it names.
                let source_identity = self.state.objects.get(&source.object).map(|state| state.identity);
                let destination_identity = self.state.objects.get(&destination.object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    let identity = objects.get(&identity.object).map(|state| state.identity);
                    identity != source_identity && identity != destination_identity
                });
                for object in objects.values_mut() {
                    if Some(object.identity) == source_identity || Some(object.identity) == destination_identity {
                        object.initialized_owned_slots = false;
                    }
                }
            }
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: destination.length },
                RangeState { element: destination.element, ownership: moved.ownership, allocation_class: moved.allocation_class, initialized: true, zero_width: self.is_zero(count), zero_start: self.is_zero(destination.start) },
            );
            self.state.range_states.insert(
                RangeIdentity { object: source.object, start: source.start, length: source.length },
                RangeState { element: source.element, ownership: Ownership::Consumed, allocation_class: moved.allocation_class, initialized: false, zero_width: self.is_zero(count), zero_start: self.is_zero(source.start) },
            );
            if carries_exclusive_elements {
                self.state.affine_list_roots.insert(destination.object);
            }
        } else {
            self.require_scalar_range_element(destination.element, "MoveRange")?;
            self.require_initialized_scalar_range(source, "MoveRange(uninitialized scalar source)")?;
            self.move_scalar_initialization(source)?;
            self.mark_initialized_scalar_range(destination);
        }
        if destination.ownership != Ownership::Unique
            || source.ownership != Ownership::Unique
            || !self.is_sole_live_token(destination.object, destination.identity)
            || !self.is_sole_live_token(source.object, source.identity)
            || destination.allocation_class != Some(AllocationClass::ReferenceCounted)
            || source.allocation_class != Some(AllocationClass::ReferenceCounted)
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
        }
        Ok(())
    }

    pub(super) fn require_source_range_complete(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
        // MoveRange removes source facts across the allocation, not merely
        // the current SSA token. Its completeness obligation must use the
        // same scope, including facts retained under a consumed MoveValue
        // source and complete-value summaries with no materialized slots.
        let object = self.live_object(range.object)?;
        let summarized_partial = object.initialized_owned_slots
            && !(self.is_zero(range.start)
                && matches!(object.extent, Extent::Counted { count, .. }
                    if self.same_scalar(count, range.length)));
        if summarized_partial || self.state.stored_objects.iter().any(|((object, slot), _)| {
            self.same_allocation(range.object, *object) && !self.slot_in_range(*slot, range)
        }) || self.state.range_states.iter().any(|(identity, state)| {
            self.same_allocation(range.object, identity.object)
                && state.initialized
                && !self.is_zero(identity.length)
                && !(self.range_start_matches(identity, state, range.start)
                    && self.same_scalar(identity.length, range.length))
        }) {
            if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                let ranges = self.state.range_states.iter().filter(|(identity, _)| self.same_allocation(range.object, identity.object)).collect::<Vec<_>>();
                let stored = self.state.stored_objects.keys().filter(|(object, _)| self.same_allocation(range.object, *object)).collect::<Vec<_>>();
                eprintln!("MoveRange(source-partial) object={:?} start={:?} length={:?} ranges={ranges:#?} stored={stored:?}", range.object, range.start, range.length);
            }
            return self.unsupported("MoveRange(source-partial)");
        }
        Ok(())
    }

    pub(super) fn slot_in_range(&self, slot: SlotIdentity, range: CheckedRange) -> bool {
        let SlotIdentity::Constant(index) = slot else {
            return false;
        };
        let Some(start) = self.state.constants.get(&range.start) else {
            return false;
        };
        let Some(length) = self.state.constants.get(&range.length) else {
            return false;
        };
        index >= *start && index - *start < *length
    }

    pub(super) fn require_initialized_range(
        &self,
        range: CheckedRange,
        element: crate::layout::LayoutId,
        operation: &'static str,
    ) -> Result<RangeState, InitialCheckError> {
        if let Some(state) = self.state.range_states.iter().find_map(|(identity, state)| {
            (self.same_allocation(range.object, identity.object)
                && self.range_start_matches(identity, state, range.start)
                && self.same_scalar(identity.length, range.length)
                && state.element == element
                // A prefix some element of which has a live shared clone
                // still owns its slots: relocating them keeps every clone
                // valid, so a move accepts the shared mark and carries it.
                && matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                && state.initialized
                && (!state.zero_width || self.is_zero(range.length)))
                .then_some(*state)
        }) {
            return Ok(state);
        }
        // A checked ABI parameter or result denotes a complete logical value.
        // Its child slots were initialized by the caller. Both callers first
        // pass the source through check_typed_range(Read), which proves this
        // exact subrange lies within the current logical length of the same
        // owner generation. A clone can therefore retain a bounded subrange
        // without reconstructing each caller-owned slot locally.
        let object = self.live_object(range.object)?.clone();
        if object.initialized_owned_slots
            && self.descriptor(object.layout)?.kind().packed_element_layout() == Some(element)
        {
            return Ok(RangeState {
                element,
                // Container completeness proves the children exist, not that
                // they are unique or belong to one allocation class.
                ownership: Ownership::Shared,
                allocation_class: None,
                initialized: true,
                zero_width: self.is_zero(range.length),
                zero_start: true,
            });
        }
        if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
            let ranges = self.state.range_states.iter().filter(|(identity, _)| identity.object == range.object).collect::<Vec<_>>();
            let stored = self.state.stored_objects.keys().filter(|(object, _)| *object == range.object).collect::<Vec<_>>();
            eprintln!("{operation} object={:?} start={:?} length={:?} object_state={:#?} ranges={ranges:#?} stored={stored:?}", range.object, range.start, range.length, self.state.objects.get(&range.object));
        }
        let Some(start) = self.state.constants.get(&range.start).copied() else {
            return self.unsupported(operation);
        };
        let Some(length) = self.state.constants.get(&range.length).copied() else {
            return self.unsupported(operation);
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let end = start.checked_add(length).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation,
        })?;
        let mut allocation_class = None;
        let mut ownership = Ownership::Unique;
        for index in start..end {
            // A slot fact may be keyed by an alias of this object (a clone
            // of the same allocation); resolve it through the identity.
            let Some(stored) = self.stored_slot_by_identity(range.object, SlotIdentity::Constant(index)) else {
                return self.unsupported(operation);
            };
            if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared) || stored.layout != pointee {
                return self.unsupported(operation);
            }
            if stored.ownership == Ownership::Shared {
                ownership = Ownership::Shared;
            }
            if allocation_class.is_some() && allocation_class != stored.allocation_class {
                return self.unsupported("MoveRange(incompatible element storage)");
            }
            allocation_class = stored.allocation_class;
        }
        Ok(RangeState { element, ownership, allocation_class, initialized: true, zero_width: self.is_zero(range.length), zero_start: self.is_zero(range.start) })
    }

    /// Whether `object` names the same allocation as `other` (an alias such
    /// as a `CloneRef` of it, or the object itself).
    pub(super) fn same_allocation(&self, object: ValueId, other: ValueId) -> bool {
        object == other
            || match (self.state.objects.get(&object), self.state.objects.get(&other)) {
                (Some(left), Some(right)) => left.identity == right.identity,
                _ => false,
            }
    }

    /// Whether a checked scalar lane of `object`'s allocation has been stored, under
    /// any alias.
    pub(super) fn checked_scalar_slot_stored(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state
            .initialized_scalar_slots
            .iter()
            .any(|(container, stored)| *stored == slot && self.same_allocation(object, *container))
    }

    /// Whether the checked scalar lane at `place` may be read: stored on every
    /// path into this block, or held by a record that arrived complete (a
    /// parameter, a loaded child).
    pub(super) fn checked_scalar_slot_initialized(&self, place: &Place) -> Result<bool, InitialCheckError> {
        let (object, slot) = self.reference_slot(place)?;
        let state = self.live_object(object)?;
        if state.initialized_owned_slots || self.state.static_data_objects.contains(&object) {
            return Ok(true);
        }
        if self.checked_scalar_slot_stored(object, slot) {
            return Ok(true);
        }
        if let (Projection::Element { index }, Extent::Counted { count, .. }) =
            (place.projection(), &state.extent)
            && self.strict_less(*index, *count)
            && self.descriptor(state.layout)?.kind().packed_element_layout()
                .is_some_and(|element| {
                    self.require_scalar_range_element(element, "Load(scalar element)").is_ok()
                        && self.scalar_prefix_initialized(object, *count, element)
                })
        {
            return Ok(true);
        }
        Ok(self.state.range_states.iter().any(|(identity, range)| {
            if !self.same_allocation(object, identity.object) || !range.initialized {
                return false;
            }
            match slot {
                SlotIdentity::Dynamic(index) => {
                    (!range.zero_start
                        && self.same_scalar(index, identity.start)
                        && self.state.constants.get(&identity.length) == Some(&1))
                        || (range.zero_start && self.strict_less(index, identity.length))
                }
                SlotIdentity::Constant(index) => (if range.zero_start { Some(&0) } else { self.state.constants.get(&identity.start) })
                    .zip(self.state.constants.get(&identity.length))
                    .is_some_and(|(start, length)| {
                        index >= *start && index - *start < *length
                    }),
            }
        }))
    }

    /// A stored slot fact of `object`'s allocation under any alias.
    pub(super) fn stored_slot_by_identity(&self, object: ValueId, slot: SlotIdentity) -> Option<&ObjectState> {
        self.state
            .stored_objects
            .iter()
            .find(|((stored_object, stored_slot), _)| *stored_slot == slot && self.same_allocation(object, *stored_object))
            .map(|(_, stored)| stored)
    }

    pub(super) fn nullable_slot_absent(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state.nullable_absent_slots.iter().any(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        })
    }

    pub(super) fn nullable_unique_slot_by_identity(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state.nullable_unique_slots.iter().any(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        })
    }

    pub(super) fn nullable_present_slot_by_identity(&self, object: ValueId, slot: SlotIdentity) -> bool {
        self.state.nullable_present_slots.iter().any(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        })
    }

    pub(super) fn clear_nullable_present_slot_by_identity(&mut self, object: ValueId, slot: SlotIdentity) {
        let aliases = self.state.nullable_present_slots.iter().copied().filter(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        }).collect::<Vec<_>>();
        for alias in aliases { self.state.nullable_present_slots.remove(&alias); }
    }

    pub(super) fn clear_nullable_unique_slot_by_identity(&mut self, object: ValueId, slot: SlotIdentity) {
        let aliases = self.state.nullable_unique_slots.iter().copied().filter(|(container, candidate)| {
            *candidate == slot && self.same_allocation(object, *container)
        }).collect::<Vec<_>>();
        for alias in aliases {
            self.state.nullable_unique_slots.remove(&alias);
        }
    }

    /// Scalar-prefix initialization is dataflow evidence, never implied by a
    /// physical allocation or by the scalar's width.
    pub(super) fn scalar_prefix_initialized(
        &self,
        object: ValueId,
        count: ValueId,
        element: crate::layout::LayoutId,
    ) -> bool {
        self.scalar_prefix_initialized_with_budget(
            object,
            count,
            element,
            self.state.exact_additions.len().saturating_add(1),
        )
    }

    pub(super) fn scalar_prefix_initialized_with_budget(
        &self,
        object: ValueId,
        count: ValueId,
        element: crate::layout::LayoutId,
        budget: usize,
    ) -> bool {
        // The scan asks `less_equal` once per exact addition and recurses;
        // `&self` fixes the state for all of it, so one index serves every
        // query instead of one rebuild per query.
        let steps = UpperSteps::new(&self.state, |_| true);
        self.scalar_prefix_initialized_scan(&steps, object, count, element, budget)
    }

    pub(super) fn scalar_prefix_initialized_scan(
        &self,
        steps: &UpperSteps,
        object: ValueId,
        count: ValueId,
        element: crate::layout::LayoutId,
        budget: usize,
    ) -> bool {
        if budget == 0 {
            return false;
        }
        let Ok(state) = self.live_object(object) else { return false };
        let complete_count = match state.extent {
            Extent::Counted { count, .. } => Some(count),
            Extent::HeaderPrefix { .. } => self.state.header_values.get(&(object, HeaderField::Length)).copied(),
            Extent::FixedBytes(_) => None,
        };
        if (state.initialized_owned_slots || self.state.static_data_objects.contains(&object))
            && complete_count.is_some_and(|current| self.less_equal_with(steps, count, current))
        {
            return true;
        }
        if self.is_zero(count) {
            return true;
        }
        if self.state.range_states.iter().any(|(identity, range)| {
            self.same_allocation(object, identity.object) && range.initialized && range.zero_start
                && (self.same_scalar(identity.length, count)
                    || self.less_equal_with(steps, count, identity.length))
                && range.element == element
        }) {
            return true;
        }
        if let Some(length) = self.state.constants.get(&count) {
            let initialized = self.state.initialized_scalar_slots.iter().filter_map(|(container, slot)| match slot {
                SlotIdentity::Constant(index) if index < length && self.same_allocation(object, *container) => Some(*index),
                _ => None,
            }).collect::<SortedSet<_>>();
            if initialized.len() as u64 == *length {
                return true;
            }
            let mut end = self.state.range_states.iter().filter_map(|(identity, range)| {
                (self.same_allocation(object, identity.object) && range.initialized && range.zero_start
                    && range.element == element)
                    .then(|| self.state.constants.get(&identity.length).copied()).flatten()
            }).max().unwrap_or(0);
            for index in initialized {
                if index == end {
                    end += 1;
                }
            }
            if end >= *length {
                return true;
            }
        }
        let mut initialized_constants = self.state.initialized_scalar_slots.iter().filter_map(
            |(container, slot)| match slot {
                SlotIdentity::Constant(index) if self.same_allocation(object, *container) => {
                    Some(*index)
                }
                _ => None,
            },
        ).collect::<SortedSet<_>>();
        let mut prefix = 0;
        while initialized_constants.remove(&prefix) {
            prefix += 1;
        }
        if prefix > 0 && self.proven_at_most(count, prefix) {
            return true;
        }
        for (sum, (left, right)) in &self.state.exact_additions {
            if !self.less_equal_with(steps, count, *sum) {
                continue;
            }
            for (prefix_length, suffix_length) in [(*left, *right), (*right, *left)] {
                let prefix_initialized = self.state.range_states.iter().any(|(identity, range)| {
                    self.same_allocation(object, identity.object)
                        && range.initialized
                        && range.zero_start
                        && range.element == element
                        && self.same_scalar(identity.length, prefix_length)
                });
                let suffix_initialized = self.state.range_states.iter().any(|(identity, range)| {
                    self.same_allocation(object, identity.object)
                        && range.initialized
                        && range.element == element
                        && self.same_scalar(identity.start, prefix_length)
                        && self.same_scalar(identity.length, suffix_length)
                });
                if prefix_initialized && suffix_initialized {
                    return true;
                }
            }
        }
        let Some(&(left, right)) = self.state.exact_additions.get(&count) else {
            return false;
        };
        for (prefix, suffix) in [(left, right), (right, left)] {
            let initialized_suffix = self.state.range_states.iter().any(|(identity, range)| {
                self.same_allocation(object, identity.object)
                    && range.initialized
                    && range.element == element
                    && self.same_scalar(identity.start, prefix)
                    && self.same_scalar(identity.length, suffix)
            });
            if initialized_suffix
                && self.scalar_prefix_initialized_scan(
                    steps,
                    object,
                    prefix,
                    element,
                    budget - 1,
                )
            {
                return true;
            }
            if self.state.constants.get(&suffix) != Some(&1) {
                continue;
            }
            let slot = self.state.constants.get(&prefix).map_or(SlotIdentity::Dynamic(prefix), |n| SlotIdentity::Constant(*n));
            if self.checked_scalar_slot_stored(object, slot)
                && self.scalar_prefix_initialized_scan(
                    steps,
                    object,
                    prefix,
                    element,
                    budget - 1,
                )
            {
                return true;
            }
        }
        false
    }

    pub(super) fn scalar_slot_initialized(
        &self,
        object: ValueId,
        slot: SlotIdentity,
        element: crate::layout::LayoutId,
    ) -> bool {
        if self.checked_scalar_slot_stored(object, slot) {
            return true;
        }
        self.state.range_states.iter().any(|(identity, range)| {
            if !self.same_allocation(object, identity.object)
                || !range.initialized
                || range.element != element
            {
                return false;
            }
            match slot {
                SlotIdentity::Dynamic(index) => {
                    (!range.zero_start
                        && self.same_scalar(index, identity.start)
                        && self.state.constants.get(&identity.length) == Some(&1))
                        || (range.zero_start && self.strict_less(index, identity.length))
                }
                SlotIdentity::Constant(index) => (if range.zero_start { Some(&0) } else { self.state.constants.get(&identity.start) })
                    .zip(self.state.constants.get(&identity.length))
                    .is_some_and(|(start, length)| {
                        index >= *start && index - *start < *length
                    }),
            }
        })
    }

    pub(super) fn require_initialized_scalar_range(
        &self,
        range: CheckedRange,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        self.require_scalar_range_element(range.element, operation)?;
        if self.is_zero(range.length)
            || self.live_object(range.object)?.initialized_owned_slots
            || self.state.static_data_objects.contains(&range.object)
            || self.state.range_states.iter().any(|(identity, state)| {
                self.same_allocation(range.object, identity.object)
                    && self.range_start_matches(identity, state, range.start)
                    && self.same_scalar(range.length, identity.length)
                    && state.element == range.element
                    && state.initialized
            })
            || (self.is_zero(range.start)
                && self.scalar_prefix_initialized(range.object, range.length, range.element))
        {
            return Ok(());
        }
        if let Some((start, length)) = self
            .state
            .constants
            .get(&range.start)
            .zip(self.state.constants.get(&range.length))
        {
            let end = start.checked_add(*length).ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: range.object,
            })?;
            if (*start..end).all(|index| {
                self.scalar_slot_initialized(
                    range.object,
                    SlotIdentity::Constant(index),
                    range.element,
                )
            }) {
                return Ok(());
            }
        }
        self.unsupported(operation)
    }

    pub(super) fn mark_initialized_scalar_range(&mut self, range: CheckedRange) {
        self.state.range_states.insert(
            RangeIdentity {
                object: range.object,
                start: range.start,
                length: range.length,
            },
            RangeState {
                element: range.element,
                ownership: range.ownership,
                allocation_class: range.allocation_class,
                initialized: true,
                zero_width: self.is_zero(range.length),
                zero_start: self.is_zero(range.start),
            },
        );
        let complete_prefix = self.is_zero(range.start)
            && self.state.objects.get(&range.object).is_some_and(|object| {
                matches!(object.extent, Extent::Counted { count, .. }
                    if self.same_scalar(count, range.length))
            });
        if complete_prefix {
            let identity = self.state.objects[&range.object].identity;
            for object in self.state.objects.values_mut() {
                if object.identity == identity {
                    object.initialized_owned_slots = true;
                }
            }
        }
    }

    pub(super) fn move_scalar_initialization(&mut self, moved: CheckedRange) -> Result<(), InitialCheckError> {
        let state = self.live_object(moved.object)?.clone();
        let full_prefix = self.is_zero(moved.start)
            && matches!(state.extent, Extent::Counted { count, .. } if self.same_scalar(count, moved.length));
        if state.initialized_owned_slots && !full_prefix {
            return self.unsupported("MoveRange(partial scalar summary)");
        }
        let moved_constants = self.state.constants.get(&moved.start).copied()
            .zip(self.state.constants.get(&moved.length).copied());
        let ranges = self.state.range_states.iter()
            .filter(|(identity, range)| self.same_allocation(moved.object, identity.object)
                && range.initialized && !range.zero_width)
            .map(|(identity, range)| (*identity, *range)).collect::<Vec<_>>();
        let mut remove_ranges = Vec::new();
        for (identity, _) in ranges {
            if self.same_scalar(identity.start, moved.start)
                && self.same_scalar(identity.length, moved.length)
            {
                remove_ranges.push(identity);
                continue;
            }
            let disjoint = moved_constants.zip(
                self.state.constants.get(&identity.start).copied()
                    .zip(self.state.constants.get(&identity.length).copied()),
            ).is_some_and(|((moved_start, moved_len), (start, len))| {
                moved_start.checked_add(moved_len).zip(start.checked_add(len))
                    .is_some_and(|(moved_end, end)| moved_end <= start || end <= moved_start)
            });
            if !disjoint {
                return self.unsupported("MoveRange(partial scalar range fact)");
            }
        }
        let slots = self.state.initialized_scalar_slots.iter()
            .filter(|(object, _)| self.same_allocation(moved.object, *object))
            .copied().collect::<Vec<_>>();
        let mut remove_slots = Vec::new();
        for slot in slots {
            let inside = match (slot.1, moved_constants) {
                (SlotIdentity::Constant(index), Some((start, length))) => {
                    index >= start && index.checked_sub(start).is_some_and(|offset| offset < length)
                }
                (SlotIdentity::Dynamic(index), _) if self.same_scalar(index, moved.start)
                    && self.state.constants.get(&moved.length) == Some(&1) => true,
                (SlotIdentity::Dynamic(_), _) => return self.unsupported("MoveRange(partial dynamic scalar slot)"),
                (SlotIdentity::Constant(_), None) => return self.unsupported("MoveRange(partial dynamic scalar range)"),
            };
            if inside { remove_slots.push(slot); }
        }
        for identity in remove_ranges { self.state.range_states.remove(&identity); }
        for slot in remove_slots { self.state.initialized_scalar_slots.remove(&slot); }
        for object in self.state.objects.values_mut() {
            if object.identity == state.identity { object.initialized_owned_slots = false; }
        }
        Ok(())
    }

    /// Completeness describes the logical prefix, not arbitrary capacity-tail
    /// writes. Keep that evidence, but account for every separately held edge
    /// before accepting it. Facts follow allocation identity across SSA moves.
    pub(super) fn require_owned_edges_within_prefix(
        &self,
        object: ValueId,
        state: &ObjectState,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout() else {
            return Ok(());
        };
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(());
        }
        let count = match state.extent {
            Extent::Counted { count, .. } => Some(count),
            Extent::HeaderPrefix { .. } => self.state.header_values.get(&(object, HeaderField::Length)).copied(),
            _ => None,
        };
        for ((container, slot), stored) in &self.state.stored_objects {
            if !self.same_allocation(object, *container) || stored.ownership == Ownership::Consumed {
                continue;
            }
            // Unknown count or slot bounds grant no containment proof. For a
            // constant-keyed slot, reuse an existing scalar/bounds fact rather
            // than inventing a new scalar or treating capacity as length.
            let inside = count.is_some_and(|count| match slot {
                SlotIdentity::Dynamic(index) => self.strict_less(*index, count),
                SlotIdentity::Constant(index) => {
                    self.state.constants.get(&count).is_some_and(|count| index < count)
                        || self.state.constants.iter().any(|(value, constant)|
                            constant == index && self.strict_less(*value, count))
                }
            });
            if !inside { return self.unsupported(operation); }
        }
        for (identity, range) in &self.state.range_states {
            if !self.same_allocation(object, identity.object)
                || !range.initialized || range.ownership == Ownership::Consumed
            {
                // A moved-out range retains its extent as history, not as
                // live child ownership. It must not prevent source cleanup.
                continue;
            }
            if range.zero_width || self.is_zero(identity.length) { continue; }
            // A checked zero-start prefix may have lost its original zero SSA
            // value at a join. Non-prefix ranges need the ordinary exact-end
            // judgment. Unknown bounds fail closed; no extent is guessed.
            let inside = count.is_some_and(|count| {
                if range.zero_start { self.less_equal(identity.length, count) }
                else { self.range_within(identity.start, identity.length, count) }
            });
            if !inside { return self.unsupported(operation); }
        }
        Ok(())
    }

    pub(super) fn has_detached_owned_field(&self, object: ValueId) -> Result<bool, InitialCheckError> {
        let state = self.live_object(object)?;
        // A checked take may temporarily clear a nonnullable field while the
        // parent stays local. It cannot cross any ownership or destructor
        // boundary until the field is restored, even if the record had been
        // summarized as complete before the take.
        let descriptor = self.descriptor(state.layout)?;
        for (index, field) in descriptor.fields().iter().enumerate() {
            let owned = matches!(field.kind(), crate::layout::FieldKind::Pointer {
                slot: crate::layout::PointerSlot::Owned, ..
            }) || matches!(field.kind(), crate::layout::FieldKind::Inline(slot)
                if self.descriptor(slot).is_ok_and(|slot| matches!(slot.kind(), LayoutKind::RuntimePointerValueSlot {
                    semantics: crate::layout::PointerSlot::Owned, ..
                })));
            if owned && self.nullable_slot_absent(object, SlotIdentity::Constant(index as u64)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn require_owned_slots_initialized(
        &self,
        object: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        if self.state.static_data_objects.contains(&object) {
            return Ok(());
        }
        let state = self.live_object(object)?;
        let allocation_identity = state.identity;
        if self.has_detached_owned_field(object)? {
            return self.unsupported(operation);
        }
        self.require_owned_edges_within_prefix(object, state, operation)?;
        if state.initialized_owned_slots {
            return Ok(());
        }
        // A closure record leaves the function that built it (a call
        // argument, a return, a stored child, a drop) only with its code
        // slot stored.
        if self.closure_layouts.contains_key(&state.layout)
            && !self.checked_scalar_slot_stored(object, SlotIdentity::Constant(0))
        {
            return Err(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object,
            });
        }
        let descriptor = self.descriptor(state.layout)?;
        if !self.unit_fields_initialized(object, state.layout)? {
            return self.unsupported(operation);
        }
        let Some(element) = descriptor.kind().packed_element_layout() else {
            // A fixed layout with owned pointer fields is initialized once
            // every field holds a stored reference (the summary flag above,
            // or the per-slot facts before consolidation).
            let pointer_fields = self.fixed_pointer_fields(state.layout)?;
            if pointer_fields.iter().all(|(index, _)| {
                let slot = SlotIdentity::Constant(*index);
                self.stored_slot_by_identity(object, slot).is_some()
                    || self.nullable_slot_absent(object, slot)
            }) {
                return Ok(());
            }
            return self.unsupported(operation);
        };
        // Scalar storage has no child reference tokens to account for. In
        // particular RawByte and BoolValueSlot are storage layouts, not
        // first-class SafeWIR scalar values. Do not interpret them as values.
        if self.descriptor(element)?.kind() == &LayoutKind::Scalar(ScalarKind::Unit) {
            let Extent::Counted { count, .. } = state.extent else { return self.unsupported(operation) };
            return if self.scalar_prefix_initialized(object, count, element) { Ok(()) } else { self.unsupported(operation) };
        }
        if matches!(self.descriptor(element)?.kind(), LayoutKind::Scalar(_)) {
            // Scalar storage has no child ownership to release. Partially
            // initialized scalar allocations may therefore be destroyed, but
            // they may not cross an ABI or ownership-transfer boundary. Those
            // boundaries use require_complete_value_initialized below.
            return Ok(());
        }
        let Extent::Counted { count, .. } = state.extent else {
            return self.unsupported(operation);
        };
        if self.is_zero(count) {
            return Ok(());
        }
        if self.state.range_states.iter().any(|(identity, range)| {
            // Prefix facts follow SSA tokens through CFG edges and aliases,
            // while initialization belongs to backing storage. Resolve the
            // fact's token before comparing; comparing either raw token ids or
            // a token id directly with ObjectIdentity loses one of those cases.
            self.state.objects.get(&identity.object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
                && self.same_scalar(identity.length, count)
                && range.element == element
                && range.initialized
                && range.zero_start
        }) && !self.state.stored_objects.keys().any(|(stored_object, _)| {
            self.state.objects.get(stored_object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
        }) {
            return Ok(());
        }
        let Some(count) = self.state.constants.get(&count).copied() else {
            return self.unsupported(operation);
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let mut initialized_slots = SortedSet::new();
        for ((stored_object, slot), stored) in &self.state.stored_objects {
            if !self.state.objects.get(stored_object)
                .is_some_and(|candidate| candidate.identity == allocation_identity)
            {
                continue;
            }
            let SlotIdentity::Constant(index) = slot else {
                return self.unsupported(operation);
            };
            if *index >= count
                || stored.layout != pointee
                || stored.ownership == Ownership::Consumed
                || !Self::is_managed_storage(stored)
            {
                return self.unsupported(operation);
            }
            initialized_slots.insert(*index);
        }
        if u64::try_from(initialized_slots.len()).ok() == Some(count) {
            Ok(())
        } else {
            self.unsupported(operation)
        }
    }

    /// Require a complete logical value at an ABI publication or owned-value
    /// transfer. This is deliberately stronger than the destruction check:
    /// scalar storage has no children to clean up, but every byte in its
    /// logical prefix must be initialized before another context may read it.
    pub(super) fn require_complete_value_initialized(
        &self,
        object: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        self.require_owned_slots_initialized(object, operation)?;
        if self.state.static_data_objects.contains(&object) {
            return Ok(());
        }
        let state = self.live_object(object)?;
        if state.initialized_owned_slots {
            return Ok(());
        }
        let descriptor = self.descriptor(state.layout)?;
        let Some(element) = descriptor.kind().packed_element_layout() else {
            if matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. })
                && !state.initialized_owned_slots
                && !self.fixed_value_fields_initialized(object, state.layout)?
            {
                return self.unsupported(operation);
            }
            return Ok(());
        };
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::Scalar(_)) {
            return Ok(());
        }
        let Extent::Counted { count, .. } = state.extent else {
            return self.unsupported(operation);
        };
        if self.scalar_prefix_initialized(object, count, element) {
            Ok(())
        } else {
            self.unsupported(operation)
        }
    }

    pub(super) fn require_destination_range_empty(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
        if !self.is_zero(range.start)
            || self.state.stored_objects.keys().any(|(object, _)| *object == range.object)
            || self.state.range_states.iter().any(|(identity, state)| {
                identity.object == range.object
                    && state.initialized
                    && !self.is_zero(identity.length)
            })
            || !self.state.range_states.iter().any(|(identity, state)| {
                identity.object == range.object
                    && state.initialized
                    && state.zero_start
                    && self.is_zero(identity.length)
            })
        {
            return self.unsupported("MoveRange(destination-initialized)");
        }
        Ok(())
    }

    pub(super) fn clone_range(
        &mut self,
        destination: &Place,
        source: &Place,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        let destination = self.check_typed_range(destination, AccessMode::Write, "CloneRange")?;
        let source = self.check_typed_range(source, AccessMode::Read, "CloneRange")?;
        self.check_range_pair(destination, source, count, "CloneRange")?;

        if destination.ownership != Ownership::Unique
            || !self.is_sole_live_token(destination.object, destination.identity)
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
        }
        if self.value_type_for_layout(destination.element)?.is_memory_bearing() {
            // Cloning a pointer-bearing prefix retains every element (the
            // backend traps on a zero or saturated count, so a partial clone
            // never survives). Both prefixes then share their elements.
            let cloned = self.require_initialized_range(
                source,
                destination.element,
                "CloneRange(uninitialized-source)",
            )?;
            self.require_destination_range_empty(destination)?;
            self.invalidate_affine_list_root(source.object);
            self.invalidate_affine_list_root(destination.object);
            // A zero-width prefix fact carries no class of its own. The
            // destination is a fresh reference-counted list; the source is
            // any managed object (a parameter's class is unknown but its
            // elements are managed, and the retain loop respects the static
            // boundary).
            let source_state = self.live_object(source.object)?.clone();
            if !Self::is_managed_storage(&source_state)
                || destination.allocation_class != Some(AllocationClass::ReferenceCounted)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: source.object,
                });
            }
            let shared = self.state.stored_objects.keys()
                .filter(|(object, _)| self.same_allocation(source.object, *object))
                .copied()
                .collect::<Vec<_>>();
            for key in shared {
                self.state.stored_objects.get_mut(&key).expect("key exists").ownership = Ownership::Shared;
            }
            let shared_ranges = self.state.range_states.keys()
                .filter(|identity| self.same_allocation(source.object, identity.object))
                .copied()
                .collect::<Vec<_>>();
            for identity in shared_ranges {
                self.state.range_states.get_mut(&identity).expect("range exists").ownership = Ownership::Shared;
            }
            {
                let destination_identity = self.state.objects.get(&destination.object).map(|state| state.identity);
                let MemoryState { objects, range_states, .. } = &mut self.state;
                range_states.retain(|identity, _| {
                    objects.get(&identity.object).map(|state| state.identity) != destination_identity
                });
            }
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: destination.length },
                RangeState {
                    element: destination.element,
                    ownership: Ownership::Shared,
                    allocation_class: cloned.allocation_class.or(source.allocation_class),
                    initialized: true,
                    zero_width: self.is_zero(count),
                    zero_start: self.is_zero(destination.start),
                },
            );
            return Ok(());
        }
        self.require_scalar_range_element(destination.element, "CloneRange")?;
        self.require_initialized_scalar_range(source, "CloneRange(uninitialized scalar source)")?;
        self.mark_initialized_scalar_range(destination);
        Ok(())
    }

    pub(super) fn check_typed_range(
        &mut self,
        place: &Place,
        required_access: AccessMode,
        operation: &'static str,
    ) -> Result<CheckedRange, InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return self.unsupported("range(non-slice)");
        };
        if place.access() != required_access {
            return self.unsupported("range(access)");
        }
        self.promote_header_extent(place.object())?;
        let state = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), state.layout)?;
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted {
            count: logical_count,
            capacity,
            stride_bytes,
            ..
        } = state.extent
        else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        let descriptor = self.descriptor(state.layout)?;
        if matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { .. })
            && !self.is_raw_byte_buffer(state.layout)?
        {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(non-raw RuntimeBuffer)",
                "CloneRange" => "CloneRange(non-raw RuntimeBuffer)",
                "CopyBytes" => "CopyBytes(non-raw RuntimeBuffer)",
                _ => "range(non-raw RuntimeBuffer)",
            });
        }
        let Some(element) = descriptor.kind().packed_element_layout() else {
            return self.unsupported("range(non-packed-layout)");
        };
        // A pointer transfer names the value stored in the authenticated slot,
        // not a reference to the containing list. Scalar range copies retain
        // their collection-view type; they carry no child ownership tokens.
        let expected = if operation == "MoveRange"
            && matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. })
        {
            ValueType::Ref(self.owned_pointer_slot_pointee(element)?)
        } else {
            ValueType::Ref(state.layout)
        };
        if place.value_type() != expected {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected,
                actual: place.value_type(),
            });
        }
        let LayoutSize::Dynamic { stride, .. } = descriptor.size() else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        if stride != stride_bytes {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        }
        let bound = if required_access == AccessMode::Write {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Capacity))
                .copied()
                .unwrap_or(capacity)
        } else {
            self.state
                .header_values
                .get(&(place.object(), HeaderField::Length))
                .copied()
                .unwrap_or(logical_count)
        };
        if !self.range_within_for_owner(place.object(), *start, *length, bound) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: bound,
            });
        }
        let _ = operation;
        Ok(CheckedRange {
            object: place.object(),
            start: *start,
            length: *length,
            layout: state.layout,
            element,
            identity: state.identity,
            ownership: state.ownership,
            allocation_class: state.allocation_class,
        })
    }

    pub(super) fn check_range_pair(
        &self,
        destination: CheckedRange,
        source: CheckedRange,
        count: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        if destination.layout != source.layout
            || destination.element != source.element
            || destination.length != count
            || source.length != count
        {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(incompatible-range)",
                "CloneRange" => "CloneRange(incompatible-range)",
                _ => "range(incompatible-range)",
            });
        }
        // There is no overlap mode in the v1 operation. Distinct allocation
        // identities are therefore required; same-root aliases are rejected
        // even when a caller claims that their slices do not overlap.
        if destination.identity == source.identity {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(overlap)",
                "CloneRange" => "CloneRange(overlap)",
                _ => "range(overlap)",
            });
        }
        Ok(())
    }

    pub(super) fn require_scalar_range_element(
        &self,
        element: crate::layout::LayoutId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        match self.descriptor(element)?.kind() {
            LayoutKind::Scalar(ScalarKind::RawByte) => return Ok(()),
            LayoutKind::Scalar(ScalarKind::V128 | ScalarKind::BoolValueSlot) => {
                return self.unsupported(match operation {
                    "MoveRange" => "MoveRange(unmapped scalar element)",
                    "CloneRange" => "CloneRange(unmapped scalar element)",
                    _ => "range(unmapped scalar element)",
                });
            }
            _ => {}
        }
        let element_type = self.value_type_for_layout(element)?;
        if element_type.is_memory_bearing() {
            return self.unsupported(match operation {
                "MoveRange" => "MoveRange(pointer-bearing element)",
                "CloneRange" => "CloneRange(pointer-bearing element)",
                _ => "range(pointer-bearing element)",
            });
        }
        Ok(())
    }

    pub(super) fn range_within(&self, start: ValueId, length: ValueId, count: ValueId) -> bool {
        let empty_at_or_before_end = self.is_zero(length) && self.less_equal(start, count);
        let from_body_start = self.state.constants.get(&start) == Some(&0)
            && self.less_equal(length, count);
        let guarded_exact_end = self.state.exact_additions.iter().any(
            |(end, (left, right))| {
                ((*left == start && *right == length) || (*left == length && *right == start))
                    && self.less_equal(*end, count)
            },
        );
        let constant_range = self
            .state
            .constants
            .get(&start)
            .zip(self.state.constants.get(&length))
            .zip(self.state.constants.get(&count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        empty_at_or_before_end || from_body_start || guarded_exact_end || constant_range
    }

    /// Scalar guards are only usable for the owner generation from which
    /// their dynamic bound was observed.  This keeps the current checker
    /// facts available for a later optimization without granting one today.
    pub(super) fn range_within_for_owner(
        &self,
        object: ValueId,
        start: ValueId,
        length: ValueId,
        count: ValueId,
    ) -> bool {
        if !self.bound_is_current(object, count) {
            return false;
        }
        // Up to three owner-scoped questions of one unchanged state: build
        // their steps at most once.
        let steps = std::cell::OnceCell::new();
        let less_equal = |left: ValueId, right: ValueId| {
            steps.get_or_init(|| self.owner_steps(object)).as_ref()
                .is_some_and(|steps| self.less_equal_with(steps, left, right))
        };
        let empty_at_or_before_end = self.is_zero(length) && less_equal(start, count);
        let from_body_start = self.state.constants.get(&start) == Some(&0)
            && less_equal(length, count);
        let guarded_exact_end = self.state.exact_additions.iter().any(|(end, (left, right))| {
            ((*left == start && *right == length) || (*left == length && *right == start))
                && less_equal(*end, count)
        });
        let constant_range = self
            .state
            .constants
            .get(&start)
            .zip(self.state.constants.get(&length))
            .zip(self.state.constants.get(&count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        empty_at_or_before_end || from_body_start || guarded_exact_end || constant_range
    }

    pub(super) fn check_byte_slice(
        &mut self,
        place: &Place,
        required_access: AccessMode,
    ) -> Result<ValueId, InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return self.unsupported("CopyBytes(non-slice)");
        };
        if place.access() != required_access {
            return self.unsupported("CopyBytes(access)");
        }
        self.promote_header_extent(place.object())?;
        let object = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), object.layout)?;
        if place.value_type() != ValueType::Ref(object.layout)
            || !self.is_raw_byte_buffer(object.layout)?
        {
            return self.unsupported("CopyBytes(non-byte layout)");
        }
        if required_access == AccessMode::Write
            && (object.ownership != Ownership::Unique
                || self.state.static_data_objects.contains(&place.object()))
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted { count, capacity, stride_bytes: 1, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        let bound = if required_access == AccessMode::Write {
            self.state.header_values.get(&(place.object(), HeaderField::Capacity))
                .copied().unwrap_or(capacity)
        } else {
            self.state.header_values.get(&(place.object(), HeaderField::Length))
                .copied().unwrap_or(count)
        };
        if !self.range_within_for_owner(place.object(), *start, *length, bound) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: bound,
            });
        }
        Ok(*length)
    }

    pub(super) fn is_raw_byte_buffer(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let LayoutKind::RuntimeBuffer { element } = self.descriptor(layout)?.kind() else {
            return Ok(false);
        };
        Ok(matches!(
            self.descriptor(*element)?.kind(),
            LayoutKind::Scalar(ScalarKind::RawByte)
        ))
    }

    pub(super) fn is_int_buffer(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let LayoutKind::PackedList { element, .. } = self.descriptor(layout)?.kind() else {
            return Ok(false);
        };
        Ok(matches!(self.descriptor(*element)?.kind(), LayoutKind::Scalar(ScalarKind::Int)))
    }
}
