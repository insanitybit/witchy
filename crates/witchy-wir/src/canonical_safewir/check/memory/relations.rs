use super::*;

impl<'a> MemoryChecker<'a> {
    pub(super) fn less_than(&self, left: ValueId, right: ValueId) -> bool {
        self.state.guards
            .iter()
            .any(|(operator, guarded_left, guarded_right)| {
                *operator == ComparisonOperator::LessUnsigned
                    && self.same_scalar(left, *guarded_left)
                    && self.same_scalar(right, *guarded_right)
            })
    }

    pub(super) fn less_than_for_owner(&self, object: ValueId, left: ValueId, right: ValueId) -> bool {
        if !self.bound_is_current(object, right) {
            return false;
        }
        // A literal index and literal extent are arithmetic facts, not an
        // observation of mutable storage. They need no dynamic guard (and a
        // later extent mutation cannot make a literal right-hand extent look
        // current because `bound_is_current` still authenticates the lane).
        if self.constant_relation(left, right, |left, right| left < right) {
            return true;
        }
        let Ok(generation) = self.current_generation(object) else { return false };
        self.state.guards.iter().any(|(operator, guarded_left, guarded_right)| {
            *operator == ComparisonOperator::LessUnsigned
                && self.same_scalar(left, *guarded_left)
                && self.same_scalar(right, *guarded_right)
                && self.state.guard_generations
                    .get(&(*operator, *guarded_left, *guarded_right))
                    .is_some_and(|facts| facts.contains(&generation))
        })
    }

    pub(super) fn strict_less(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left < right)
            || self.less_than(left, right)
    }

    pub(super) fn same_scalar(&self, left: ValueId, right: ValueId) -> bool {
        Self::same_scalar_in(&self.state, left, right)
    }

    pub(super) fn same_scalar_in(facts: &MemoryState, left: ValueId, right: ValueId) -> bool {
        // Discharge the scalar-only alternatives before materializing header
        // relations. The remaining alternatives are exactly same_scalar_with.
        if left == right || Self::constant_relation_in(facts, left, right, |left, right| left == right) {
            return true;
        }
        let header_lengths = Self::header_length_pairs(facts);
        header_lengths.contains(&(left, right)) || header_lengths.contains(&(right, left))
    }

    /// Every `(loaded, count)` pair where `loaded` is a `Length` header load
    /// of an object whose extent is counted by `count`: the header loads that
    /// prove a logical length. A capacity load is deliberately absent. Shared
    /// meets may retain only capacity >= count, not equality with the physical
    /// capacity header, so a capacity load must never become a logical-length
    /// proof.
    ///
    /// Building the set is one pass over `header_values`. Callers that answer
    /// many `same_scalar` questions against one state (the `less_equal_in`
    /// worklist, the full-prefix scan over objects) build it once instead of
    /// rescanning every header load per question, which made a straight-line
    /// program with many string temporaries take minutes to check.
    pub(super) fn header_length_pairs(facts: &MemoryState) -> SortedSet<(ValueId, ValueId)> {
        facts
            .header_values
            .iter()
            .filter_map(|((object, field), loaded)| {
                if *field != HeaderField::Length {
                    return None;
                }
                let Extent::Counted { count, .. } = facts.objects.get(object)?.extent else {
                    return None;
                };
                Some((*loaded, count))
            })
            .collect()
    }

    /// `same_scalar_in` with the header-length pairs already collected.
    pub(super) fn same_scalar_with(
        facts: &MemoryState,
        header_lengths: &SortedSet<(ValueId, ValueId)>,
        left: ValueId,
        right: ValueId,
    ) -> bool {
        left == right
            || facts.constants.get(&left).zip(facts.constants.get(&right))
                .is_some_and(|(left, right)| left == right)
            || header_lengths.contains(&(left, right))
            || header_lengths.contains(&(right, left))
    }

    /// A zero-start range is semantically rooted at zero. Its SSA start may
    /// be only a placeholder (for example its count), never a second valid
    /// nonzero start. Other ranges match their actual checked start value.
    pub(super) fn range_start_matches(&self, identity: &RangeIdentity, state: &RangeState, start: ValueId) -> bool {
        if state.zero_start { self.is_zero(start) }
        else { self.same_scalar(identity.start, start) }
    }

    pub(super) fn is_zero(&self, value: ValueId) -> bool {
        self.state.constants.get(&value) == Some(&0)
    }

    pub(super) fn require_supported_runtime_buffer_access(
        &self,
        place: &Place,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        let layout = self.live_object(place.object())?.layout;
        if matches!(self.descriptor(layout)?.kind(), LayoutKind::RuntimeBuffer { .. })
            && !self.is_raw_byte_buffer(layout)?
        {
            return self.unsupported(operation);
        }
        Ok(())
    }

    /// Return the maximum integer encodable by a physically narrowed scalar
    /// lane. This is checked before initialization is recorded, so a refused
    /// store cannot authenticate bytes the backend would truncate.
    pub(super) fn scalar_store_maximum(&self, place: &Place) -> Result<Option<u64>, InitialCheckError> {
        let object = self.live_object(place.object())?;
        let descriptor = self.descriptor(object.layout)?;
        let kind = match place.projection() {
            Projection::Element { .. } => match descriptor.kind().packed_element_layout() {
                Some(layout) => self.scalar_kind(layout)?,
                None => None,
            },
            Projection::Field { index } => {
                if let LayoutKind::Scalar(kind) = descriptor.kind() {
                    if *kind != ScalarKind::Int || *index != 0 {
                        return self.invalid_projection(place.object());
                    }
                    Some(*kind)
                } else {
                    let field = descriptor.fields().get(*index as usize).ok_or(
                        InitialCheckError::InvalidProjection {
                            function: self.function.id,
                            object: place.object(),
                        },
                    )?;
                    match field.kind() {
                        FieldKind::Scalar(kind) => Some(kind),
                        FieldKind::Inline(layout) => self.scalar_kind(layout)?,
                        FieldKind::Pointer { .. } => None,
                    }
                }
            }
            Projection::Header(_) | Projection::Slice { .. } => None,
        };
        Ok(match kind {
            Some(ScalarKind::RawByte | ScalarKind::Tag8) => Some(255),
            Some(ScalarKind::Tag16) => Some(65_535),
            _ => None,
        })
    }

    pub(super) fn scalar_kind(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<Option<ScalarKind>, InitialCheckError> {
        Ok(match self.descriptor(layout)?.kind() {
            LayoutKind::Scalar(kind) => Some(*kind),
            LayoutKind::RuntimeValueSlot { value } => match self.descriptor(*value)?.kind() {
                LayoutKind::Scalar(kind) => Some(*kind),
                _ => None,
            },
            _ => None,
        })
    }

    /// Whether the path proves `value <= bound`: a constant, or an unsigned
    /// guard against a constant no larger than the bound.
    pub(super) fn proven_at_most(&self, value: ValueId, bound: u64) -> bool {
        if self.state.constants.get(&value).is_some_and(|constant| *constant <= bound)
            || self.state.scalar_upper_bounds.get(&value).is_some_and(|upper| *upper <= bound)
        {
            return true;
        }
        self.state.guards.iter().any(|(operator, left, right)| {
            self.same_scalar(value, *left)
                && match operator {
                    ComparisonOperator::LessEqualUnsigned => self.state.constants.get(right).is_some_and(|limit| *limit <= bound),
                    ComparisonOperator::LessUnsigned => self.state.constants.get(right).is_some_and(|limit| *limit <= bound + 1),
                    _ => false,
                }
        })
    }

    pub(super) fn less_equal(&self, left: ValueId, right: ValueId) -> bool {
        Self::less_equal_in(&self.state, left, right)
    }

    /// The upward steps usable for `object`: only guards established under
    /// its current owner generation count. `None` when the owner is gone,
    /// which answers every question false.
    pub(super) fn owner_steps(&self, object: ValueId) -> Option<UpperSteps> {
        let generation = self.current_generation(object).ok()?;
        Some(UpperSteps::new(&self.state, |guard| {
            self.state.guard_generations.get(guard).is_some_and(|facts| facts.contains(&generation))
        }))
    }

    pub(super) fn less_equal_in(facts: &MemoryState, left: ValueId, right: ValueId) -> bool {
        // These are already sufficient at the search's first visited value.
        // They need no guard/addition graph and do not authenticate any owner.
        if left == right || Self::constant_relation_in(facts, left, right, |left, right| left <= right)
            || facts.constants.get(&left) == Some(&0)
        {
            return true;
        }
        Self::less_equal_in_with(facts, &UpperSteps::new(facts, |_| true), left, right)
    }

    /// `less_equal` against steps built from the current state, for callers
    /// that ask many questions of one unchanged state.
    pub(super) fn less_equal_with(&self, steps: &UpperSteps, left: ValueId, right: ValueId) -> bool {
        Self::less_equal_in_with(&self.state, steps, left, right)
    }

    pub(super) fn less_equal_in_with(facts: &MemoryState, steps: &UpperSteps, left: ValueId, right: ValueId) -> bool {
        steps.reaches(facts, left, |value| {
            Self::same_scalar_with(facts, &steps.header_lengths, value, right)
                || Self::constant_relation_in(facts, value, right, |left, right| left <= right)
                || facts.constants.get(&value) == Some(&0)
        })
    }

    /// A wrapping addition is exact only when the current path proves that its
    /// mathematical sum fits the `Size` domain.  The operation remains a wrap
    /// in the IR; this merely permits consumers of `exact_additions` to use the
    /// relation when the wrap is unreachable.
    pub(super) fn proven_nonwrapping_size_add(&self, left: ValueId, right: ValueId) -> bool {
        if !matches!(value_type(self.function, left), Ok(ValueType::Size))
            || !matches!(value_type(self.function, right), Ok(ValueType::Size))
        {
            return false;
        }
        let maximum = u64::from(u32::MAX);
        if let (Some(left), Some(right)) = (
            self.state.constants.get(&left).copied(),
            self.state.constants.get(&right).copied(),
        ) {
            return left.checked_add(right).is_some_and(|sum| sum <= maximum);
        }
        let Some((value, delta)) = self.state.constants.get(&left).copied()
            .map(|delta| (right, delta))
            .or_else(|| self.state.constants.get(&right).copied().map(|delta| (left, delta)))
        else {
            return false;
        };
        self.state.guards.iter().any(|(operator, guarded, bound)| {
            if *guarded != value {
                return false;
            }
            let Some(bound) = self.state.constants.get(bound).copied() else {
                return false;
            };
            match operator {
                ComparisonOperator::LessUnsigned => bound
                    .checked_sub(1)
                    .and_then(|maximum_value| maximum_value.checked_add(delta))
                    .is_some_and(|sum| sum <= maximum),
                ComparisonOperator::LessEqualUnsigned => bound
                    .checked_add(delta)
                    .is_some_and(|sum| sum <= maximum),
                _ => false,
            }
        })
    }

    pub(super) fn allocation_fits(&self, capacity: ValueId, base: u32, stride: u32) -> bool {
        Self::allocation_fits_in(&self.state, capacity, base, stride)
    }

    /// A shared counted phi can lose the predecessor generation attached to
    /// its scalar count when only one candidate was written.  A fresh length
    /// header load may re-establish that fact, but only after the lowerer has
    /// checked the raw allocation limit and guarded the exact loaded value.
    /// This deliberately does not trust aliases, arithmetic equalities, or an
    /// old header value that survived an owner-generation advance.
    pub(super) fn authenticate_fresh_counted_header_guard(
        &mut self,
        comparison: Comparison,
    ) -> Result<(), InitialCheckError> {
        if comparison.operator != ComparisonOperator::LessEqualUnsigned
            || comparison.left != comparison.right
        {
            return Ok(());
        }
        let mut authenticated = Vec::new();
        for ((object, field), header) in &self.state.header_values {
            if *field != HeaderField::Length || *header != comparison.right {
                continue;
            }
            // Header facts can outlive a released sibling token.  Such a
            // stale entry is never a fresh observation for this guard.
            let Ok(state) = self.live_object(*object) else { continue };
            let Extent::Counted { .. } = state.extent else { continue };
            let LayoutSize::Dynamic { base, stride } = self.descriptor(state.layout)?.size() else {
                continue;
            };
            if !self.is_raw_byte_buffer(state.layout)? {
                continue;
            }
            let Ok(generation) = self.current_generation(*object) else { continue };
            if self.state
                .fresh_header_lengths
                .get(header)
                .is_some_and(|facts| facts.contains(&generation))
                && self.allocation_fits(*header, base, stride)
            {
                authenticated.push(generation);
            }
        }
        for generation in authenticated {
            self.state.bound_generations
                .entry(comparison.right)
                .or_default()
                .insert(generation);
            self.state.guard_generations
                .entry((comparison.operator, comparison.left, comparison.right))
                .or_default()
                .insert(generation);
        }
        Ok(())
    }

    pub(super) fn reauthenticate_raw_header_extent(&mut self, object: ValueId, header: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if !matches!(state.extent, Extent::HeaderPrefix { .. } | Extent::Counted { .. }) {
            return Err(InitialCheckError::UnsupportedMemoryOperation { function: self.function.id, operation: "ReauthenticateRawHeaderExtent(non-raw-extent)" });
        }
        let LayoutSize::Dynamic { base, stride } = self.descriptor(state.layout)?.size() else {
            return Err(InitialCheckError::UnsupportedMemoryOperation { function: self.function.id, operation: "ReauthenticateRawHeaderExtent(fixed)" });
        };
        if !self.is_raw_byte_buffer(state.layout)?
            || self.state.header_values.get(&(object, HeaderField::Length)) != Some(&header)
            || !self.current_generation(object).is_ok_and(|generation| self.state.fresh_header_lengths.get(&header).is_some_and(|facts| facts.contains(&generation)))
            || !self.allocation_fits(header, base, stride)
            || !self.state.guards.contains(&(ComparisonOperator::LessEqualUnsigned, header, header)) {
            return Err(InitialCheckError::UnsupportedMemoryOperation { function: self.function.id, operation: "ReauthenticateRawHeaderExtent(unproved)" });
        }
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes: base, count: header, capacity: header, stride_bytes: stride,
        };
        Ok(())
    }

    pub(super) fn allocation_fits_in(facts: &MemoryState, capacity: ValueId, base: u32, stride: u32) -> bool {
        if stride == 0 {
            return false;
        }
        let maximum = (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride);
        facts.constants
            .get(&capacity)
            .is_some_and(|capacity| *capacity <= maximum)
            || facts.scalar_upper_bounds.get(&capacity).is_some_and(|upper| *upper <= maximum)
            || facts.guards.iter().any(|(operator, left, right)| {
                *operator == ComparisonOperator::LessEqualUnsigned
                    && *left == capacity
                    && facts.constants.get(right).copied() == Some(maximum)
            })
    }

    pub(super) fn constant_relation(
        &self,
        left: ValueId,
        right: ValueId,
        predicate: impl FnOnce(u64, u64) -> bool,
    ) -> bool {
        Self::constant_relation_in(&self.state, left, right, predicate)
    }

    pub(super) fn constant_relation_in(
        facts: &MemoryState,
        left: ValueId,
        right: ValueId,
        predicate: impl FnOnce(u64, u64) -> bool,
    ) -> bool {
        facts.constants
            .get(&left)
            .copied()
            .zip(facts.constants.get(&right).copied())
            .is_some_and(|(left, right)| predicate(left, right))
    }

    pub(super) fn descriptor(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<&LayoutDescriptor, InitialCheckError> {
        let physical = self.layouts.resolve_nominal(layout)
            .map_err(|_| InitialCheckError::UnknownLayout(layout))?;
        self.layouts
            .get(physical)
            .ok_or(InitialCheckError::UnknownLayout(layout))
    }

    pub(super) fn value_type_for_layout(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<ValueType, InitialCheckError> {
        match self.descriptor(layout)?.kind() {
            LayoutKind::Scalar(kind) => self.value_type_for_scalar(*kind),
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned | PointerSlot::OwnedNullable,
            } => Ok(ValueType::Ref(*pointee)),
            LayoutKind::RuntimePointerValueSlot { .. } => {
                self.unsupported("borrowed or raw pointer slot")
            }
            _ => Ok(ValueType::Ref(layout)),
        }
    }

    /// A range records its storage-slot layout, while its stored token has the
    /// layout of the object referenced by an owned slot. Borrowed and raw slots
    /// never carry transferable v1 ownership tokens.
    pub(super) fn owned_pointer_slot_pointee(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<crate::layout::LayoutId, InitialCheckError> {
        match self.descriptor(layout)?.kind() {
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned,
            } => Ok(*pointee),
            LayoutKind::RuntimePointerValueSlot { .. } => {
                self.unsupported("borrowed or raw pointer slot")
            }
            _ => self.unsupported("owned pointer slot"),
        }
    }

    /// The value type of a closure object's callable lane: the signature the
    /// module registered for that exact layout. Fail-closed -- an unregistered
    /// layout refuses rather than inventing a signature.
    pub(super) fn closure_slot_type(
        &self,
        object: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<ValueType, InitialCheckError> {
        self.closure_layouts
            .get(&layout)
            .map(|signature| ValueType::Callable(*signature))
            .ok_or(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object,
            })
    }

    pub(super) fn value_type_for_field(&self, field: FieldKind) -> Result<ValueType, InitialCheckError> {
        match field {
            FieldKind::Scalar(kind) => self.value_type_for_scalar(kind),
            FieldKind::Inline(layout) => self.value_type_for_layout(layout),
            FieldKind::Pointer { pointee, .. } => Ok(ValueType::Ref(pointee)),
        }
    }

    pub(super) fn value_type_for_scalar(&self, kind: ScalarKind) -> Result<ValueType, InitialCheckError> {
        match kind {
            ScalarKind::Unit => Ok(ValueType::Unit),
            ScalarKind::Bool => Ok(ValueType::Bool),
            ScalarKind::Int | ScalarKind::Duration => Ok(ValueType::Int),
            ScalarKind::Float => Ok(ValueType::Float),
            ScalarKind::U32 | ScalarKind::Tag8 | ScalarKind::Tag16 | ScalarKind::Tag32 => {
                Ok(ValueType::Size)
            }
            ScalarKind::V128 => Ok(ValueType::V128),
            ScalarKind::RawByte | ScalarKind::BoolValueSlot | ScalarKind::Callable => {
                self.unsupported("layout scalar")
            }
        }
    }

    pub(super) fn require_ref_type(
        &self,
        value: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(value, ValueType::Ref(layout))
    }

    pub(super) fn require_type(&self, value: ValueId, expected: ValueType) -> Result<(), InitialCheckError> {
        let actual = value_type(self.function, value)?;
        if actual != expected {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value,
                expected,
                actual,
            });
        }
        Ok(())
    }

    pub(super) fn invalid_projection<T>(&self, object: ValueId) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::InvalidProjection {
            function: self.function.id,
            object,
        })
    }

    pub(super) fn unsupported<T>(&self, operation: &'static str) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation,
        })
    }
}
