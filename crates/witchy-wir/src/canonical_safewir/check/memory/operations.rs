use super::*;

impl<'a> MemoryChecker<'a> {
    /// A direct unsigned Size comparison is true on its BranchIf true edge.
    /// Record only the generations attached to that comparison's bound; a
    /// stale count cannot become current merely because its Bool is true.
    pub(crate) fn assume_true_bounds_comparison(
        &mut self,
        condition: ValueId,
    ) -> Result<(), InitialCheckError> {
        let Some(comparison) = self.state.comparisons.get(&condition).copied() else {
            return Ok(());
        };
        if comparison.operator != ComparisonOperator::LessUnsigned
            || value_type(self.function, comparison.left)? != ValueType::Size
            || value_type(self.function, comparison.right)? != ValueType::Size
        {
            return Ok(());
        }
        let relation = (comparison.operator, comparison.left, comparison.right);
        self.state.guards.insert(relation);
        let generations = self.state.bound_generations
            .get(&comparison.left)
            .into_iter()
            .chain(self.state.bound_generations.get(&comparison.right))
            .flat_map(|facts| facts.iter().copied())
            .chain(self.fresh_header_generations(comparison.right))
            .collect::<SortedSet<_>>();
        self.state.guard_generations.insert(relation, generations);
        Ok(())
    }

    pub(super) fn consume_call_move(
        &mut self,
        source: ValueId,
        parameter: ParameterOwnership,
    ) -> Result<(), InitialCheckError> {
        if self.is_borrowed_parameter(source) {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: source });
        }
        let state = self.live_object(source)?;
        let permitted = match parameter {
            ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack => state.ownership == Ownership::Unique,
            ParameterOwnership::Shared | ParameterOwnership::WriteBack => {
                matches!(state.ownership, Ownership::Unique | Ownership::Shared)
            }
            ParameterOwnership::Value => {
                matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    && self.is_shared_immutable_storage(source, state)
                    && !self.is_sole_live_token(source, state.identity)
            }
        };
        if !permitted {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        if matches!(parameter, ParameterOwnership::Shared | ParameterOwnership::WriteBack | ParameterOwnership::Value)
            && self.is_shared_immutable_storage(source, state) {
            self.invalidate_full_current_prefixes(source);
            self.state.objects
                .get_mut(&source)
                .expect("live object exists")
                .ownership = Ownership::Consumed;
            return Ok(());
        }
        // Unique ABI parameters remain concrete RC authority only.
        if state.allocation_class != Some(AllocationClass::ReferenceCounted) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: source,
            });
        }
        if matches!(parameter, ParameterOwnership::Unique | ParameterOwnership::UniqueWriteBack)
            && !self.is_sole_live_token(source, state.identity)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        self.invalidate_full_current_prefixes(source);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    /// The exact one-parameter `Value` alias contract may carry a sole managed
    /// token. Its caller-side move is token-conserving because the only result
    /// is re-established from the pre-consumption state as the matching
    /// `AliasesParameter` lane in `check_call`; this grants no write authority.
    pub(super) fn consume_exact_moved_value_alias(&mut self, source: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?;
        if matches!(state.ownership, Ownership::Unique | Ownership::Shared)
            && self.is_shared_immutable_storage(source, state)
            && self.is_sole_live_token(source, state.identity)
        {
            self.invalidate_full_current_prefixes(source);
            self.state
                .objects
                .get_mut(&source)
                .expect("live object exists")
                .ownership = Ownership::Consumed;
            return Ok(());
        }
        self.consume_call_move(source, ParameterOwnership::Value)
    }

    /// The sole non-RC call transfer: `check_call` invokes this only after
    /// proving the exact projected exclusive carrier and its hidden root lane
    /// for the same WriteBack parameter.  It moves the existing Bump token;
    /// it does not manufacture allocation or storage provenance.
    pub(super) fn consume_authenticated_projected_bump_write_back(
        &mut self,
        source: ValueId,
    ) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?;
        if !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
            || state.allocation_class != Some(AllocationClass::Bump)
        {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: source,
            });
        }
        self.invalidate_full_current_prefixes(source);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    /// Returns true when this operation belongs to the memory judgment.
    pub(crate) fn check(&mut self, operation: &Operation) -> Result<bool, InitialCheckError> {
        if !self.state.pending_affine_field_borrow.is_empty() {
            let borrowed_field_read = matches!(operation, Operation::Load { place, transfer, .. }
                if self.state.pending_affine_field_borrow.iter().any(|(borrowed, _)| place.object() == *borrowed)
                    && ((*transfer == LoadTransfer::CopyScalar && !place.value_type().is_memory_bearing())
                        || *transfer == LoadTransfer::BorrowAffine));
            if !borrowed_field_read {
                self.finish_affine_field_borrow();
            }
        }
        if self.state.pending_affine_borrow.is_some()
            && !matches!(operation, Operation::Call { .. })
        {
            return self.unsupported("Dictionary(borrowed affine child must enter the next call)");
        }
        match operation {
            Operation::Dictionary(dictionary) => {
                self.check_dictionary(dictionary)?;
                Ok(true)
            }
            Operation::ClosureOwner(owner) => {
                use crate::canonical_safewir::closure_owner_contract::OwnerOperation;
                match owner {
                    OwnerOperation::Construct { constructor, captures, .. } => {
                        let declared = self.affine_abi.and_then(|proofs| proofs.owner_captures.get(constructor));
                        for (index, capture) in captures.iter().enumerate() {
                            if matches!(value_type(self.function, *capture)?, ValueType::Ref(_)) {
                                self.require_complete_value_initialized(*capture, "Construct(uninitialized managed capture)")?;
                                self.live_object(*capture)?;
                                if declared.and_then(|captures| captures.get(index))
                                    .is_some_and(|(transfer, _)| *transfer ==
                                        crate::canonical_safewir::closure_owner_contract::CaptureTransfer::MoveManaged)
                                {
                                    let layout = match value_type(self.function, *capture)? {
                                        ValueType::Ref(layout) => layout,
                                        _ => unreachable!(),
                                    };
                                    let plan = self.affine_abi.and_then(|proofs| proofs.layouts.get(&layout))
                                        .ok_or(InitialCheckError::UnsupportedMemoryOperation {
                                            function: self.function.id, operation: "Construct lacks affine child proof",
                                        })?;
                                    if !self.require_affine_plan(*capture, plan)? {
                                        return self.unsupported("Construct(unproved affine child)");
                                    }
                                    self.consume_call_move(*capture, ParameterOwnership::Unique)?;
                                } else {
                                    self.share_allocation_tokens(*capture)?;
                                }
                            }
                        }
                    }
                    OwnerOperation::Deconstruct { constructor, environment, .. } => {
                        let plan = self.affine_abi.and_then(|proofs| proofs.owner_environments.get(constructor))
                            .ok_or(InitialCheckError::UnsupportedMemoryOperation {
                                function: self.function.id, operation: "Deconstruct lacks source proof",
                            })?.clone();
                        let ValueType::Ref(layout) = value_type(self.function, *environment)? else {
                            return self.unsupported("Deconstruct(non-reference environment)");
                        };
                        let extent = self.parameter_extent(layout)?;
                        self.state.objects.insert(*environment, ObjectState {
                            identity: *environment, owner_generation: 0, layout, extent,
                            ownership: Ownership::Unique,
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: false, initialized_owned_slots: true,
                        });
                        self.seed_affine_plan(*environment, &plan);
                    }
                    OwnerOperation::ReadEnvironmentField { destination, .. }
                        if matches!(value_type(self.function, *destination)?, ValueType::Ref(_)) => {
                        let ValueType::Ref(layout) = value_type(self.function, *destination)? else { unreachable!() };
                        let extent = self.parameter_extent(layout)?;
                        self.state.objects.insert(*destination, ObjectState {
                            identity: *destination, owner_generation: 0, layout, extent, ownership: Ownership::Shared,
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: true, initialized_owned_slots: true,
                        });
                    }
                    OwnerOperation::CloneEnvironment { destination, .. } => {
                        let ValueType::Ref(layout) = value_type(self.function, *destination)? else {
                            return self.unsupported("CloneEnvironment(non-reference)");
                        };
                        // The owner registry check establishes the exact
                        // environment layout before this memory judgment.
                        let extent = self.parameter_extent(layout)?;
                        self.state.objects.insert(*destination, ObjectState {
                            identity: *destination, owner_generation: 0, layout, extent, ownership: Ownership::Shared,
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: true, initialized_owned_slots: true,
                        });
                    }
                    // A slice's retained backing root is admitted exactly as a
                    // shared `List` parameter: a shared, immutable RC object
                    // with its parameter extent, so no store can reach it and
                    // its header is authenticated before any element read.
                    OwnerOperation::ReadSharedSlice {
                        destination,
                        field: crate::canonical_safewir::closure_owner_contract::SharedSliceField::Root,
                        ..
                    } => {
                        let ValueType::Ref(layout) = value_type(self.function, *destination)? else {
                            return self.unsupported("ReadSharedSlice(root non-reference)");
                        };
                        let extent = self.parameter_extent(layout)?;
                        self.state.objects.insert(*destination, ObjectState {
                            identity: *destination, owner_generation: 0, layout, extent, ownership: Ownership::Shared,
                            allocation_class: Some(AllocationClass::ReferenceCounted),
                            shared_immutable: true, initialized_owned_slots: true,
                        });
                    }
                    OwnerOperation::ReadCapability { .. }
                    | OwnerOperation::ReadEnvironmentField { .. }
                    | OwnerOperation::ReadOwner { .. }
                    | OwnerOperation::ReadSharedPlace { .. }
                    | OwnerOperation::ReadSharedSlice { .. }
                    | OwnerOperation::ReadTag { .. }
                    | OwnerOperation::ReadCallable { .. }
                    | OwnerOperation::Clone { .. }
                    | OwnerOperation::Drop { .. }
                    | OwnerOperation::NewList { .. }
                    | OwnerOperation::ListGet { .. }
                    | OwnerOperation::ListLength { .. }
                    | OwnerOperation::CloneList { .. }
                    | OwnerOperation::ListPush { .. }
                    | OwnerOperation::ListSet { .. }
                    | OwnerOperation::DropList { .. } => {}
                }
                Ok(true)
            }
            Operation::ConstInt { destination, value } => {
                if let Ok(value) = u64::try_from(*value) {
                    self.state.constants.insert(*destination, value);
                }
                Ok(false)
            }
            Operation::ConstSize { destination, value } => {
                self.state.constants.insert(*destination, *value);
                Ok(false)
            }
            Operation::NarrowSize { destination, value } => {
                if let Some(value) = self.state.constants.get(value).copied()
                    && value <= u64::from(u32::MAX)
                {
                    self.state.constants.insert(*destination, value);
                }
                Ok(false)
            }
            Operation::Arithmetic {
                destination,
                operator: ArithmeticOperator::Add,
                left,
                right,
                overflow: OverflowBehavior::Trap,
            } => {
                self.state.exact_additions.insert(*destination, (*left, *right));
                if let Some(sum) = self.state.constants.get(left).copied()
                    .zip(self.state.constants.get(right).copied())
                    .and_then(|(left, right)| left.checked_add(right))
                    .filter(|sum| *sum <= u64::from(u32::MAX))
                {
                    self.state.constants.insert(*destination, sum);
                }
                Ok(false)
            }
            Operation::Arithmetic {
                destination,
                operator: ArithmeticOperator::Add,
                left,
                right,
                overflow: OverflowBehavior::Wrap,
            } => {
                if self.proven_nonwrapping_size_add(*left, *right) {
                    self.state.exact_additions.insert(*destination, (*left, *right));
                }
                Ok(false)
            }
            Operation::Compare { destination, operator, left, right } => {
                self.state.comparisons.insert(
                    *destination,
                    Comparison {
                        operator: *operator,
                        left: *left,
                        right: *right,
                    },
                );
                Ok(false)
            }
            Operation::Guard { condition, kind } => {
                let comparison = self.state.comparisons.get(condition).copied().ok_or(
                    InitialCheckError::InvalidGuard {
                        function: self.function.id,
                        condition: *condition,
                        kind: *kind,
                    },
                )?;
                if matches!(*kind, GuardKind::Bounds | GuardKind::Arithmetic) {
                    if !matches!(
                        comparison.operator,
                        ComparisonOperator::LessUnsigned
                            | ComparisonOperator::LessEqualUnsigned
                    ) {
                        return Err(InitialCheckError::InvalidGuard {
                            function: self.function.id,
                            condition: *condition,
                            kind: *kind,
                        });
                    }
                    if value_type(self.function, comparison.left)? != ValueType::Size
                        || value_type(self.function, comparison.right)? != ValueType::Size
                    {
                        return Err(InitialCheckError::InvalidGuard {
                            function: self.function.id,
                            condition: *condition,
                            kind: *kind,
                        });
                    }
                    self.state.guards
                        .insert((comparison.operator, comparison.left, comparison.right));
                    let generations = self.state.bound_generations
                        .get(&comparison.left)
                        .into_iter()
                        .chain(self.state.bound_generations.get(&comparison.right))
                        .flat_map(|facts| facts.iter().copied())
                        .chain(self.fresh_header_generations(comparison.right))
                        .collect::<SortedSet<_>>();
                    self.state.guard_generations.insert(
                        (comparison.operator, comparison.left, comparison.right),
                        generations,
                    );
                    self.authenticate_fresh_counted_header_guard(comparison)?;
                }
                Ok(false)
            }
            Operation::ReauthenticateRawHeaderExtent { object, header } => {
                self.reauthenticate_raw_header_extent(*object, *header)?;
                Ok(false)
            }
            Operation::Alloc { destination, layout, extent, class } => {
                self.allocate(*destination, *layout, extent, *class)?;
                Ok(true)
            }
            Operation::StaticData { destination, layout, extent, .. } => {
                self.static_data(*destination, *layout, extent)?;
                Ok(true)
            }
            Operation::Load { destination, place, transfer } => {
                self.load(*destination, place, *transfer)?;
                if matches!(
                    (place.projection(), transfer),
                    (Projection::Header(HeaderField::Length), LoadTransfer::CopyScalar)
                ) {
                    // Ordinary header loads can appear while an unrelated
                    // release has retired the token.  They do not become a
                    // fresh extent observation; the later slice check still
                    // rejects that dead object through `live_object`.
                    if let Ok(generation) = self.current_generation(place.object()) {
                        self.state
                            .fresh_header_lengths
                            .entry(*destination)
                            .or_default()
                            .insert(generation);
                    }
                }
                if matches!(transfer, LoadTransfer::MoveUnique | LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField) {
                    self.invalidate_full_current_prefixes(place.object());
                }
                if matches!(transfer, LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField) {
                    self.invalidate_owner_generation(place.object())?;
                }
                Ok(true)
            }
            Operation::InitNullableOwned { place } => {
                self.init_nullable_owned(place)?;
                self.invalidate_full_current_prefixes(place.object());
                Ok(true)
            }
            Operation::Store { place, value, transfer } => {
                self.store(place, *value, *transfer)?;
                self.invalidate_full_current_prefixes(place.object());
                self.rebase_extent_generation(place.object())?;
                if *transfer == StoreTransfer::MoveUnique {
                    self.invalidate_full_current_prefixes(*value);
                }
                Ok(true)
            }
            Operation::UpdateCount { object, count } => {
                self.update_count(*object, *count)?;
                self.invalidate_full_current_prefixes(*object);
                self.invalidate_owner_generation(*object)?;
                Ok(true)
            }
            Operation::MoveValue { destination, source }
                if value_type(self.function, *source)?.is_memory_bearing() =>
            {
                self.move_value(*destination, *source)?;
                self.rebase_extent_generation(*destination)?;
                Ok(true)
            }
            Operation::MoveValue { destination, source } => {
                if let Some(value) = self.state.constants.get(source).copied() {
                    self.state.constants.insert(*destination, value);
                }
                Ok(false)
            }
            Operation::CloneRef { destination, source } => {
                self.clone_ref(*destination, *source)?;
                Ok(true)
            }
            Operation::Release { object } => {
                self.release(*object)?;
                Ok(true)
            }
            Operation::DropRef { object } => {
                self.drop_ref(*object)?;
                Ok(true)
            }
            Operation::Reclaim { object } => {
                // Reclaim consumes the token; invalidate its facts before
                // that transition so no later malformed alias can reuse them.
                self.invalidate_owner_generation(*object)?;
                self.reclaim(*object)?;
                self.invalidate_full_current_prefixes(*object);
                Ok(true)
            }
            Operation::CopyBytes { destination, source, length } => {
                self.copy_bytes(destination, source, *length)?;
                self.invalidate_full_current_prefixes(destination.object());
                self.rebase_extent_generation(destination.object())?;
                Ok(true)
            }
            Operation::MoveRange { destination, source, count } => {
                self.move_range(destination, source, *count)?;
                self.invalidate_full_current_prefixes(source.object());
                self.invalidate_full_current_prefixes(destination.object());
                self.rebase_extent_generation(source.object())?;
                self.rebase_extent_generation(destination.object())?;
                Ok(true)
            }
            Operation::CloneRange { destination, source, count } => {
                self.clone_range(destination, source, *count)?;
                self.invalidate_full_current_prefixes(destination.object());
                self.rebase_extent_generation(destination.object())?;
                Ok(true)
            }
            Operation::Retain { .. } => Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: operation_name(operation),
            }),
            _ => Ok(false),
        }
    }

    pub(super) fn allocate(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
        class: AllocationClass,
    ) -> Result<(), InitialCheckError> {
        if class != AllocationClass::ReferenceCounted {
            return Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: match class {
                    AllocationClass::Bump => "Alloc(Bump)",
                    AllocationClass::Scratch => "Alloc(Scratch)",
                    AllocationClass::ReferenceCounted => unreachable!(),
                },
            });
        }
        self.require_ref_type(destination, layout)?;
        let descriptor = self.descriptor(layout)?;
        self.check_extent(destination, extent, descriptor, false)?;
        let pointer_element = descriptor
            .kind()
            .packed_element_layout()
            .filter(|element| {
                self.value_type_for_layout(*element)
                    .is_ok_and(|value_type| value_type.is_memory_bearing())
            });
        let fresh_owned_list = matches!(descriptor.kind(), LayoutKind::PackedList { element, .. }
            if matches!(self.descriptor(*element).map(|slot| slot.kind()),
                Ok(LayoutKind::RuntimePointerValueSlot { semantics: PointerSlot::Owned, .. })))
            && class == AllocationClass::ReferenceCounted;
        let initialized_empty_scalar_prefix = descriptor
            .kind()
            .packed_element_layout()
            .is_some_and(|element| {
                matches!(
                    self.descriptor(element).map(|descriptor| descriptor.kind()),
                    Ok(LayoutKind::Scalar(_))
                ) && matches!(extent, Extent::Counted { count, .. } if self.is_zero(*count))
            });
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                owner_generation: 0,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Unique,
                allocation_class: Some(class),
                shared_immutable: false,
                initialized_owned_slots: initialized_empty_scalar_prefix,
            },
        );
        self.attach_affine_plan(destination, layout);
        if fresh_owned_list {
            self.state.affine_list_roots.insert(destination);
        }
        self.bind_extent_generation(destination)?;
        self.bind_preallocation_extent_guards(destination)?;
        if let Some(element) = pointer_element {
            let Extent::Counted { count, .. } = extent else {
                return self.unsupported("Alloc(pointer collection without counted extent)");
            };
            if self.is_zero(*count) {
                self.state.range_states.insert(
                    RangeIdentity { object: destination, start: *count, length: *count },
                    RangeState {
                        element,
                        ownership: Ownership::Unique,
                        allocation_class: None,
                        initialized: true,
                        zero_width: true,
                        zero_start: true,
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn static_data(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
    ) -> Result<(), InitialCheckError> {
        self.require_ref_type(destination, layout)?;
        // Static bytes cannot authenticate a closure table index. Closure
        // records must initialize their code lane through the checked path.
        if self.closure_layouts.contains_key(&layout) {
            return Err(InitialCheckError::InvalidClosureSlot {
                function: self.function.id,
                object: destination,
            });
        }
        let descriptor = self.descriptor(layout)?;
        self.check_extent(destination, extent, descriptor, true)?;
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                owner_generation: 0,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Shared,
                allocation_class: None,
                shared_immutable: false,
                initialized_owned_slots: false,
            },
        );
        self.state.static_data_objects.insert(destination);
        self.bind_extent_generation(destination)?;
        self.bind_preallocation_extent_guards(destination)?;
        Ok(())
    }

    pub(super) fn check_extent(
        &self,
        object: ValueId,
        extent: &Extent,
        descriptor: &LayoutDescriptor,
        allow_prefix: bool,
    ) -> Result<(), InitialCheckError> {
        let valid = match (extent, descriptor.size()) {
            (Extent::FixedBytes(actual), LayoutSize::Fixed(expected)) => {
                *actual == u64::from(expected)
            }
            (
                Extent::Counted {
                    header_bytes,
                    count,
                    capacity,
                    stride_bytes,
                },
                LayoutSize::Dynamic { base, stride },
            ) => {
                *header_bytes == base
                    && *stride_bytes == stride
                    && value_type(self.function, *count)? == ValueType::Size
                    && value_type(self.function, *capacity)? == ValueType::Size
                    && self.less_equal(*count, *capacity)
                    && self.allocation_fits(*capacity, base, stride)
            }
            (Extent::HeaderPrefix { minimum_bytes }, LayoutSize::Dynamic { base, .. }) => {
                allow_prefix && *minimum_bytes == base
            }
            _ => false,
        };
        if !valid {
            if let Extent::Counted { count, capacity, .. } = extent {
                if !self.less_equal(*count, *capacity) {
                    return Err(InitialCheckError::MissingCapacityGuard {
                        function: self.function.id,
                        count: *count,
                        capacity: *capacity,
                    });
                }
                if let LayoutSize::Dynamic { base, stride } = descriptor.size() {
                    if !self.allocation_fits(*capacity, base, stride) {
                        let maximum = (u64::from(u32::MAX) - u64::from(base))
                            / u64::from(stride);
                        return Err(InitialCheckError::MissingAllocationSizeGuard {
                            function: self.function.id,
                            capacity: *capacity,
                            maximum,
                        });
                    }
                }
            }
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            });
        }
        Ok(())
    }

    /// One atomic ownership transition. Live prefix facts are replaced only
    /// after proving completeness and absence of unpublished owned edges.
    /// Moved-out history is not a live edge. Unknown bounds grant no authority.
    pub(super) fn take_last(&mut self, destination: ValueId, place: &Place, expected: ValueType) -> Result<(), InitialCheckError> {
        self.require_type(destination, expected)?;
        let Projection::Element { index } = place.projection() else { return self.unsupported("TakeLast(non-element)"); };
        let object = place.object();
        let state = self.live_object(object)?.clone();
        if state.ownership != Ownership::Unique
            || state.allocation_class != Some(AllocationClass::ReferenceCounted)
            || !self.is_sole_live_token(object, state.identity)
        { return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object }); }
        let exclusive_element = matches!(expected, ValueType::Ref(_)) && self.exclusive_list_elements(object)?;
        if matches!(expected, ValueType::Ref(_)) && !exclusive_element {
            return self.unsupported("TakeLast(reference without exclusive element proof)");
        }
        let Extent::Counted { header_bytes, count, capacity, stride_bytes } = state.extent else {
            return self.unsupported("TakeLast(uncounted)");
        };
        let Some(element) = self.descriptor(state.layout)?.kind().packed_element_layout() else {
            return self.unsupported("TakeLast(non-list)");
        };
        // check_place already requires index < current logical count. The
        // unique SSA definition must additionally identify that exact tail.
        let last = self.constant_relation(*index, count, |index, count| index.checked_add(1) == Some(count))
            || self.function.blocks.iter().flat_map(|block| &block.operations).any(|operation| {
                matches!(operation, Operation::Arithmetic { destination, operator: ArithmeticOperator::Subtract, left, right, .. }
                    if destination == index && self.same_scalar(*left, count) && self.state.constants.get(right) == Some(&1))
            });
        if !last { return self.unsupported("TakeLast(not-last)"); }
        self.require_complete_value_initialized(object, "TakeLast(uninitialized prefix)")?;
        if !matches!(expected, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::Unit | ValueType::Ref(_)) {
            return self.unsupported("TakeLast(element type)");
        }
        let containers = self.state.objects.iter().filter_map(|(value, candidate)|
            (candidate.identity == state.identity).then_some(*value)).collect::<SortedSet<_>>();
        self.state.stored_objects.retain(|(container, _), _| !containers.contains(container));
        self.state.range_states.retain(|range, _| !containers.contains(&range.object));
        self.state.initialized_scalar_slots.retain(|(container, _)| !containers.contains(container));
        for candidate in self.state.objects.values_mut().filter(|candidate| candidate.identity == state.identity) {
            candidate.extent = Extent::Counted { header_bytes, count: *index, capacity, stride_bytes };
            candidate.initialized_owned_slots = true;
        }
        if matches!(expected, ValueType::Ref(_)) {
            // Remaining slots are live owning edges, not vacant capacity.
            // Preserve shared children through stores, moves and publication.
            self.state.range_states.insert(
                RangeIdentity { object, start: *index, length: *index },
                RangeState { element, ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    initialized: true, zero_width: self.is_zero(*index), zero_start: true },
            );
        } else {
            // Taking the tail transfers only that scalar lane. Every slot
            // below the new count remains initialized and may be republished
            // after the caller initializes a replacement tail.
            self.state.range_states.insert(
                RangeIdentity { object, start: *index, length: *index },
                RangeState {
                    element,
                    ownership: state.ownership,
                    allocation_class: state.allocation_class,
                    initialized: true,
                    zero_width: self.is_zero(*index),
                    zero_start: true,
                },
            );
        }
        self.state.header_values.retain(|(container, field), _| !containers.contains(container) || *field != HeaderField::Length);
        self.state.header_values.insert((object, HeaderField::Length), *index);
        if let ValueType::Ref(layout) = expected {
            let extent = self.parameter_extent(layout)?;
            // Transfer one owned reference. Recursive child facts come only
            // from the source-bound list plan and checked element stores.
            self.state.objects.insert(destination, ObjectState {
                identity: destination, owner_generation: 0, layout, extent, ownership: Ownership::Unique,
                allocation_class: Some(AllocationClass::ReferenceCounted), shared_immutable: false, initialized_owned_slots: true,
            });
            let child_plan = self.state.affine_plans.get(&object).and_then(|plan| match plan {
                super::super::AffinePhysicalPlan::Sequence { element, child } if *element == layout => Some(child.as_ref().clone()),
                _ => None,
            });
            if let Some(child_plan) = &child_plan { self.seed_affine_plan(destination, child_plan); }
        }
        Ok(())
    }
    pub(super) fn load(
        &mut self,
        destination: ValueId,
        place: &Place,
        transfer: LoadTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Read {
            return self.unsupported("Load(access)");
        }
        if matches!(place.projection(), Projection::Header(HeaderField::Capacity)) {
            let object = place.object();
            let state = self.live_object(object)?.clone();
            if self.is_raw_byte_buffer(state.layout)? {
                self.promote_header_extent(object)?;
                let state = self.live_object(object)?.clone();
                let Extent::Counted { header_bytes: 4, count, stride_bytes: 1, .. } = state.extent else {
                    return self.unsupported("Load(raw allocation capacity without count)");
                };
                if transfer != LoadTransfer::CopyScalar
                    || place.value_type() != ValueType::Size
                    || state.ownership != Ownership::Unique
                    || state.allocation_class != Some(AllocationClass::ReferenceCounted)
                    || self.state.static_data_objects.contains(&object)
                    || !self.is_sole_live_token(object, state.identity)
                {
                    return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object });
                }
                self.require_ref_type(object, state.layout)?;
                self.require_type(destination, ValueType::Size)?;
                self.state.objects.get_mut(&object).expect("live owner exists").extent = Extent::Counted {
                    header_bytes: 4, count, capacity: destination, stride_bytes: 1,
                };
                // A prior call or loop lane may retain the conservative
                // logical-length capacity alias. The authenticated physical
                // capacity supersedes that alias for subsequent range checks.
                self.state.header_values.remove(&(object, HeaderField::Capacity));
                let generation = self.current_generation(object)?;
                self.state.bound_generations.entry(destination).or_default().insert(generation);
                return Ok(());
            }
        }
        self.require_supported_runtime_buffer_access(place, "Load(non-raw RuntimeBuffer)")?;
        let expected = self.check_place(place)?;
        if transfer == LoadTransfer::BorrowAffine {
            let ValueType::Ref(layout) = expected else {
                return self.unsupported("Load(affine field borrow requires reference)");
            };
            self.require_ref_type(destination, layout)?;
            let Projection::Field { index } = place.projection() else {
                return self.unsupported("Load(affine field borrow requires field)");
            };
            let parent = self.live_object(place.object())?.clone();
            if !parent.initialized_owned_slots {
                return self.unsupported("Load(affine field borrow requires initialized parent)");
            }
            let child_plan = self.state.affine_plans.get(&place.object()).and_then(|plan| match plan {
                super::super::AffinePhysicalPlan::NullableFields { fields, .. } => fields.iter()
                    .find(|(field, pointee, _)| field == index && *pointee == layout)
                    .map(|(_, _, child)| child.as_ref().clone()),
                _ => None,
            }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id, operation: "Load(affine field borrow lacks recursive proof)",
            })?;
            self.state.objects.insert(destination, ObjectState {
                identity: destination, owner_generation: parent.owner_generation,
                layout, extent: self.parameter_extent(layout)?, ownership: Ownership::Shared,
                allocation_class: None, shared_immutable: true, initialized_owned_slots: true,
            });
            self.seed_affine_plan(destination, &child_plan);
            self.state.pending_affine_field_borrow.push((destination, place.object()));
            return Ok(());
        }
        if expected == ValueType::V128 {
            return self.unsupported("Load(V128)");
        }
        if transfer == LoadTransfer::TakeLast {
            return self.take_last(destination, place, expected);
        }
        // A callable lane is memory-bearing in the ABI sense (it is not an
        // ordinary scalar at a call boundary) but it holds a table index, so
        // its load is a plain scalar copy like any other four-byte lane.
        if let ValueType::Callable(_) = expected {
            if transfer != LoadTransfer::CopyScalar {
                return self.unsupported("Load(closure slot transfer)");
            }
            // The slot must have been stored on every path: a fresh record's
            // code slot holds nothing a call may dispatch through.
            if !self.checked_scalar_slot_initialized(place)? {
                return Err(InitialCheckError::InvalidClosureSlot {
                    function: self.function.id,
                    object: place.object(),
                });
            }
            self.require_type(destination, expected)?;
            return Ok(());
        }
        if !expected.is_memory_bearing() {
            if transfer != LoadTransfer::CopyScalar {
                return self.unsupported("Load(scalar transfer)");
            }
            self.require_type(destination, expected)?;
            if let Projection::Header(field) = place.projection() {
                self.state
                    .header_values
                    .insert((place.object(), *field), destination);
            } else if !self.checked_scalar_slot_initialized(place)? {
                return self.unsupported("Load(uninitialized scalar)");
            }
            return Ok(());
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Load(non-linear reference)");
        };
        self.require_ref_type(destination, layout)?;
        let key = self.reference_slot(place)?;
        let nullable = self.nullable_fixed_field(place)?.is_some();
        if !nullable && self.nullable_slot_absent(key.0, key.1) {
            return self.unsupported("Load(detached owned field)");
        }
        let owned_field_take = transfer == LoadTransfer::TakeOwnedField;
        if owned_field_take && (nullable || self.owned_fixed_field(place)? != Some(layout)) {
            return self.unsupported("Load(nonnullable owned field take)");
        }
        if !owned_field_take && nullable != matches!(transfer, LoadTransfer::ClonePresent | LoadTransfer::TakeNullableUnique) {
            return self.unsupported("Load(nullable transfer mismatch)");
        }
        if matches!(transfer, LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField) {
            let root = self.live_object(place.object())?.clone();
            if root.ownership != Ownership::Unique
                || root.shared_immutable
                || root.allocation_class != Some(AllocationClass::ReferenceCounted)
                || !self.is_sole_live_token(place.object(), root.identity)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
            let requires_present = matches!(self.state.affine_plans.get(&place.object()),
                Some(super::super::AffinePhysicalPlan::NullableFields { allow_absent: false, .. }));
            if (requires_present && !self.nullable_present_slot_by_identity(key.0, key.1))
                || self.nullable_slot_absent(key.0, key.1)
            {
                return self.unsupported("Load(take of absent nullable slot)");
            }
            let stored = self.stored_slot_by_identity(key.0, key.1).cloned();
            if transfer == LoadTransfer::TakeNullableUnique
                && !self.nullable_unique_slot_by_identity(key.0, key.1)
            {
                return self.unsupported("Load(nullable take without unique child proof)");
            }
            let child = if let Some(child) = stored { child } else {
                // Exact child identities are discarded when a complete
                // aggregate crosses a join. A unique take needs the separate
                // unique-slot proof; an owned take conservatively gets a
                // shared token when that proof is unavailable.
                let extent = match self.descriptor(layout)?.size() {
                    LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                    LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
                };
                ObjectState {
                    identity: destination, owner_generation: 0, layout, extent,
                    ownership: if transfer == LoadTransfer::TakeNullableUnique {
                        Ownership::Unique
                    } else {
                        Ownership::Shared
                    },
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                }
            };
            if !Self::is_managed_storage(&child)
                || !matches!(child.ownership, Ownership::Unique | Ownership::Shared)
                || (transfer == LoadTransfer::TakeNullableUnique && child.ownership != Ownership::Unique)
            {
                return self.unsupported("Load(nullable take of non-unique child)");
            }
            let aliases = self.state.stored_objects.keys().copied().filter(|(container, slot)| {
                *slot == key.1 && self.same_allocation(key.0, *container)
            }).collect::<Vec<_>>();
            for alias in aliases {
                self.state.stored_objects.remove(&alias);
            }
            self.clear_nullable_unique_slot_by_identity(key.0, key.1);
            self.clear_nullable_present_slot_by_identity(key.0, key.1);
            self.state.nullable_absent_slots.insert(key);
            self.state.objects.insert(destination, child);
            let child_plan = self.state.affine_plans.get(&place.object()).and_then(|plan| match plan {
                super::super::AffinePhysicalPlan::NullableFields { fields, .. } => fields.iter()
                    .find(|(field, pointee, _)| matches!(key.1, SlotIdentity::Constant(slot) if slot == u64::from(*field))
                        && *pointee == layout)
                    .map(|(_, _, child)| child.as_ref().clone()),
                _ => None,
            });
            if let Some(child_plan) = &child_plan { self.seed_affine_plan(destination, child_plan); }
            return Ok(());
        }
        if transfer == LoadTransfer::ClonePresent {
            let root = self.live_object(place.object())?.clone();
            let stored = self.stored_slot_by_identity(key.0, key.1).cloned();
            if !root.initialized_owned_slots && stored.is_none()
                && !self.nullable_slot_absent(key.0, key.1)
            {
                return self.unsupported("Load(uninitialized nullable slot)");
            }
            if !Self::is_managed_storage(&root) {
                return self.unsupported("Load(nullable unmanaged root)");
            }
            // ClonePresent's runtime presence check dominates both retain and
            // the result. Even a definitely absent slot is legal: that execution
            // traps, so it never reaches the continuation described here.
            let mut result = if let Some(stored) = stored {
                if !Self::is_managed_storage(&stored)
                    || !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                {
                    return self.unsupported("Load(nullable invalid child)");
                }
                stored
            } else {
                let extent = match self.descriptor(layout)?.size() {
                    LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                    LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
                };
                ObjectState {
                    identity: destination, owner_generation: 0, layout, extent, ownership: Ownership::Shared,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false, initialized_owned_slots: true,
                }
            };
            result.ownership = Ownership::Shared;
            // A recorded present child must no longer appear uniquely owned
            // through any alias of its parent after the clone.
            let aliases = self.state.stored_objects.keys().copied().filter(|(container, slot)| {
                *slot == key.1 && self.same_allocation(key.0, *container)
            }).collect::<Vec<_>>();
            for alias in aliases {
                self.state.stored_objects.get_mut(&alias).expect("recorded slot").ownership = Ownership::Shared;
            }
            self.clear_nullable_unique_slot_by_identity(key.0, key.1);
            self.state.objects.insert(destination, result);
            return Ok(());
        }
        let recorded = self.state.stored_objects.get(&key).cloned().or_else(|| {
            // CloneRef preserves allocation identity. A shared read may use
            // the initialized slot recorded under its original owning token,
            // even after that token was dropped. Do not extend move authority.
            (transfer == LoadTransfer::CloneShared)
                .then(|| self.stored_slot_by_identity(key.0, key.1).cloned())
                .flatten()
        });
        let mut stored = if let Some(stored) = recorded {
            stored
        } else {
            if transfer != LoadTransfer::CloneShared {
                return self.unsupported("Load(dynamic unique move)");
            }
            let index = match place.projection() {
                Projection::Element { index } => *index,
                // A complete fixed-layout object's pointer field holds a
                // reference the object owns; the load takes a shared clone.
                Projection::Field { index } => {
                    let state = self.live_object(place.object())?.clone();
                    if !state.initialized_owned_slots {
                        return self.unsupported("Load(uninitialized reference)");
                    }
                    let pointee = self
                        .fixed_pointer_fields(state.layout)?
                        .iter()
                        .find(|(field, _)| *field == u64::from(*index))
                        .map(|(_, pointee)| *pointee)
                        .ok_or_else(|| InitialCheckError::InvalidProjection {
                            function: self.function.id,
                            object: place.object(),
                        })?;
                    if !Self::is_managed_storage(&state) {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: place.object(),
                        });
                    }
                    let extent = match self.descriptor(pointee)?.size() {
                        LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                        LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
                    };
                    // The child was stored complete (`Store` requires it),
                    // so its own owned slots need no per-slot facts.
                    self.state.objects.insert(destination, ObjectState {
                        identity: destination,
                        owner_generation: 0,
                        layout: pointee,
                        extent,
                        ownership: Ownership::Shared,
                        allocation_class: Some(AllocationClass::ReferenceCounted),
                        shared_immutable: false,
                        initialized_owned_slots: true,
                    });
                    return Ok(());
                }
                _ => return self.unsupported("Load(uninitialized reference)"),
            };
            // A managed parameter's prefix is seeded from its contract once
            // its runtime count is known.
            if let (Extent::Counted { count, .. }, Some(element)) = (
                self.live_object(place.object())?.extent.clone(),
                self.descriptor(self.live_object(place.object())?.layout)?.kind().packed_element_layout(),
            ) {
                let _ = self.seed_managed_prefix(place.object(), count, element)?;
            }
            let range = self.state.range_states.iter().find_map(|(identity, state)| {
                (self.same_allocation(identity.object, place.object())
                    && state.initialized
                    && !state.zero_width
                    && state.zero_start
                    && self.strict_less(index, identity.length))
                    .then_some((*identity, *state))
            }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "Load(uninitialized reference)",
            })?;
            if !matches!(range.1.ownership, Ownership::Unique | Ownership::Shared)
                || range.1.allocation_class != Some(AllocationClass::ReferenceCounted)
            {
                if std::env::var_os("WITCHY_CANONICAL_PLAN_DEBUG").is_some() {
                    eprintln!("canonical element load refused: object {:?} prefix {:?} {:?}", place.object(), range.0, range.1);
                }
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
            self.state.range_states.get_mut(&range.0).expect("range exists").ownership = Ownership::Shared;
            let extent = match self.descriptor(layout)?.size() {
                LayoutSize::Fixed(bytes) => Extent::FixedBytes(u64::from(bytes)),
                LayoutSize::Dynamic { base, .. } => Extent::HeaderPrefix { minimum_bytes: base },
            };
            // Every element of an initialized prefix was stored complete
            // (`Store` requires it), so the loaded element is complete.
            let stored = ObjectState {
                identity: destination,
                owner_generation: 0,
                layout,
                extent,
                ownership: Ownership::Shared,
                allocation_class: range.1.allocation_class,
                shared_immutable: false,
                initialized_owned_slots: true,
            };
            self.state.objects.insert(destination, stored);
            return Ok(());
        };
        match transfer {
            LoadTransfer::BorrowAffine => unreachable!("handled before ordinary reference loads"),
            LoadTransfer::ClonePresent | LoadTransfer::TakeLast | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField => unreachable!("handled before ordinary reference loads"),
            LoadTransfer::CloneShared => {
                let affine_child = match (self.state.affine_plans.get(&place.object()), place.projection()) {
                    (Some(super::super::AffinePhysicalPlan::NullableFields { fields, .. }), Projection::Field { index }) =>
                        fields.iter().any(|(field, _, _)| field == index),
                    (Some(super::super::AffinePhysicalPlan::Sequence { .. }), Projection::Element { .. }) => true,
                    _ => false,
                };
                if affine_child { return self.unsupported("Load(cannot clone affine child)"); }
                if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                    || !Self::is_managed_storage(&stored)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: place.object(),
                    });
                }
                stored.ownership = Ownership::Shared;
                // Every recorded name for this physical slot must lose its
                // unique-child claim when the load retains the child.
                let aliases = self.state.stored_objects.keys().copied()
                    .filter(|(container, slot)| {
                        *slot == key.1 && self.same_allocation(key.0, *container)
                    }).collect::<Vec<_>>();
                for alias in aliases {
                    self.state.stored_objects.get_mut(&alias)
                        .expect("recorded slot").ownership = Ownership::Shared;
                }
                if matches!(place.projection(), Projection::Element { .. }) {
                    self.invalidate_affine_list_root(place.object());
                }
                self.state.objects.insert(destination, stored);
                Ok(())
            }
            LoadTransfer::MoveUnique => {
                if self.live_object(place.object())?.ownership != Ownership::Unique
                    || stored.ownership != Ownership::Unique
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: place.object(),
                    });
                }
                self.state.stored_objects.remove(&key);
                self.state.objects.insert(destination, stored);
                Ok(())
            }
            LoadTransfer::CopyScalar => self.unsupported("Load(reference copy)"),
        }
    }

    pub(super) fn store(
        &mut self,
        place: &Place,
        value: ValueId,
        transfer: StoreTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Write {
            return self.unsupported("Store(access)");
        }
        if matches!(place.projection(), Projection::Header(_)) {
            return self.unsupported("Store(header)");
        }
        if self.state.static_data_objects.contains(&place.object()) {
            return Err(InitialCheckError::WritableStaticData {
                function: self.function.id,
                object: place.object(),
            });
        }
        let object = self.live_object(place.object())?;
        let authenticated_projected_scalar_write = object.ownership == Ownership::Shared
            && object.identity == place.object()
            && object.allocation_class == Some(AllocationClass::ReferenceCounted)
            && !object.shared_immutable
            && matches!(place.projection(), Projection::Field { index }
                if place.value_type() == ValueType::Int
                    && self.state.initialized_scalar_slots.contains(&(
                        place.object(), SlotIdentity::Constant(u64::from(*index)),
                    )));
        if object.ownership != Ownership::Unique && !authenticated_projected_scalar_write {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        self.require_supported_runtime_buffer_access(place, "Store(non-raw RuntimeBuffer)")?;
        let expected = self.check_place(place)?;
        if expected == ValueType::V128 {
            return self.unsupported("Store(V128)");
        }
        // The only producer of a `Callable` value is `FunctionValue`, so a
        // closure slot store copies a table index into the lane.
        if let ValueType::Callable(_) = expected {
            if transfer != StoreTransfer::CopyScalar {
                return self.unsupported("Store(closure slot transfer)");
            }
            self.require_type(value, expected)?;
            let slot = self.reference_slot(place)?;
            self.state.initialized_scalar_slots.insert(slot);
            self.consolidate_fixed_fields(place.object())?;
            return Ok(());
        }
        if !expected.is_memory_bearing() {
            if transfer != StoreTransfer::CopyScalar {
                return self.unsupported("Store(scalar transfer)");
            }
            self.require_type(value, expected)?;
            if let Some(maximum) = self.scalar_store_maximum(place)?
                && !self.proven_at_most(value, maximum)
            {
                return Err(InitialCheckError::MissingBoundsGuard {
                    function: self.function.id,
                    index: value,
                    count: value,
                });
            }
            let slot = self.reference_slot(place)?;
            self.state.initialized_scalar_slots.insert(slot);
            self.consolidate_fixed_fields(place.object())?;
            return Ok(());
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Store(non-linear reference)");
        };
        self.require_ref_type(value, layout)?;
        // A stored reference is complete: its own owned slots are initialized,
        // so a later load or drop of it needs no per-slot facts.
        self.require_complete_value_initialized(value, "Store(uninitialized owned slot)")?;
        let affine_child = self.state.affine_plans.get(&place.object()).and_then(|plan| match (plan, place.projection()) {
            (super::super::AffinePhysicalPlan::Sequence { element, child }, Projection::Element { .. })
                if *element == layout => Some(child.as_ref().clone()),
            (super::super::AffinePhysicalPlan::NullableFields { fields, .. }, Projection::Field { index }) =>
                fields.iter().find(|(field, pointee, _)| *field == *index && *pointee == layout)
                    .map(|(_, _, child)| child.as_ref().clone()),
            _ => None,
        });
        if let Some(child) = &affine_child
            && (transfer != StoreTransfer::MoveUnique || !self.require_affine_plan(value, child)?)
        {
            return self.unsupported("Store(affine child without recursive move proof)");
        }
        let key = self.reference_slot(place)?;
        if self.stored_slot_by_identity(key.0, key.1).is_some()
            || (matches!(place.projection(), Projection::Field { .. })
                && self.live_object(place.object())?.initialized_owned_slots
                && !self.nullable_slot_absent(key.0, key.1))
        {
            return self.unsupported("Store(overwrite reference)");
        }
        if self.same_allocation(place.object(), value) {
            return self.unsupported("Store(self-owned edge)");
        }
        if let Projection::Element { index } = place.projection() {
            let root = self.live_object(place.object())?;
            if root.initialized_owned_slots {
                let Extent::Counted { count, .. } = root.extent else {
                    return self.unsupported("Store(unknown initialized prefix)");
                };
                if !self.less_equal(count, *index) {
                    return self.unsupported("Store(overwrite initialized prefix)");
                }
            }
            if self.state.range_states.iter().any(|(identity, state)| {
                self.same_allocation(identity.object, place.object())
                    && state.initialized
                    && state.ownership != Ownership::Consumed
                    && !state.zero_width && !self.is_zero(identity.length)
                    // Live ranges forbid overwrite; moved-out ranges do not.
                    // Require proved disjointness. Unknown is not permission.
                    && !(if state.zero_start {
                        self.less_equal(identity.length, *index)
                    } else {
                        self.strict_less(*index, identity.start)
                            || self.range_within(identity.start, identity.length, *index)
                    })
            }) {
                return self.unsupported("Store(overwrite initialized prefix)");
            }
        }
        let mut stored = self.live_object(value)?.clone();
        let source_is_static = self.state.static_data_objects.contains(&value);
        if !self.is_shared_immutable_storage(value, &stored) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object: value,
            });
        }
        // `static_data_objects` is SSA-local. Once stored, retain its managed
        // immutable provenance in the slot state so a later load cannot turn a
        // static child into unproven classless storage.
        stored.shared_immutable |= source_is_static;
        match transfer {
            StoreTransfer::MoveUnique => {
                if stored.ownership != Ownership::Unique {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
                self.state
                    .objects
                    .get_mut(&value)
                    .expect("live source exists")
                    .ownership = Ownership::Consumed;
            }
            StoreTransfer::CloneShared => {
                if stored.ownership != Ownership::Shared
                    || !Self::is_managed_storage(&stored)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
                stored.ownership = Ownership::Shared;
                if matches!(place.projection(), Projection::Element { .. }) {
                    self.invalidate_affine_list_root(place.object());
                }
            }
            StoreTransfer::CopyScalar => return self.unsupported("Store(reference copy)"),
        }
        let absent_aliases = self.state.nullable_absent_slots.iter().copied().filter(|(container, slot)| {
            *slot == key.1 && self.same_allocation(key.0, *container)
        }).collect::<Vec<_>>();
        for absent in absent_aliases {
            self.state.nullable_absent_slots.remove(&absent);
        }
        if self.nullable_fixed_field(place)?.is_some() {
            self.clear_nullable_unique_slot_by_identity(key.0, key.1);
            self.clear_nullable_present_slot_by_identity(key.0, key.1);
            if transfer == StoreTransfer::MoveUnique {
                self.state.nullable_unique_slots.insert(key);
            }
            self.state.nullable_present_slots.insert(key);
        }
        self.state.stored_objects.insert(key, stored);
        self.consolidate_fixed_fields(place.object())?;
        Ok(())
    }

    pub(super) fn init_nullable_owned(&mut self, place: &Place) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Write {
            return self.unsupported("InitNullableOwned(access)");
        }
        self.check_place(place)?;
        let root = self.live_object(place.object())?;
        if root.ownership != Ownership::Unique
            || root.allocation_class != Some(AllocationClass::ReferenceCounted)
            || !self.is_sole_live_token(place.object(), root.identity)
        {
            return Err(InitialCheckError::InvalidOwnership { function: self.function.id, object: place.object() });
        }
        let Some(_) = self.nullable_fixed_field(place)? else {
            return self.unsupported("InitNullableOwned(non-nullable slot)");
        };
        let key = self.reference_slot(place)?;
        if self.live_object(place.object())?.initialized_owned_slots
            || self.stored_slot_by_identity(key.0, key.1).is_some()
        {
            return self.unsupported("InitNullableOwned(overwrite live edge)");
        }
        if self.nullable_slot_absent(key.0, key.1) {
            return self.unsupported("InitNullableOwned(duplicate)");
        }
        self.state.nullable_absent_slots.insert(key);
        self.clear_nullable_present_slot_by_identity(key.0, key.1);
        self.state.nullable_unique_slots.insert(key);
        self.consolidate_fixed_fields(place.object())?;
        Ok(())
    }

    pub(super) fn nullable_fixed_field(&self, place: &Place) -> Result<Option<crate::layout::LayoutId>, InitialCheckError> {
        let Projection::Field { index } = place.projection() else { return Ok(None) };
        let descriptor = self.descriptor(self.live_object(place.object())?.layout)?;
        Ok(descriptor.fields().get(*index as usize).and_then(|field| match field.kind() {
            crate::layout::FieldKind::Pointer { pointee, slot: PointerSlot::OwnedNullable } => Some(pointee),
            crate::layout::FieldKind::Inline(slot) => match self.layouts.get(slot).map(|slot| slot.kind()) {
                Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::OwnedNullable }) => Some(*pointee),
                _ => None,
            },
            _ => None,
        }))
    }

    pub(super) fn owned_fixed_field(&self, place: &Place) -> Result<Option<crate::layout::LayoutId>, InitialCheckError> {
        let Projection::Field { index } = place.projection() else { return Ok(None) };
        let descriptor = self.descriptor(self.live_object(place.object())?.layout)?;
        Ok(descriptor.fields().get(*index as usize).and_then(|field| match field.kind() {
            crate::layout::FieldKind::Pointer { pointee, slot: PointerSlot::Owned } => Some(pointee),
            crate::layout::FieldKind::Inline(slot) => match self.layouts.get(slot).map(|slot| slot.kind()) {
                Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned }) => Some(*pointee),
                _ => None,
            },
            _ => None,
        }))
    }

    /// The owned pointer fields of a fixed layout: `(field index, pointee)`.
    pub(super) fn fixed_pointer_fields(&self, layout: crate::layout::LayoutId) -> Result<Rc<[(u64, crate::layout::LayoutId)]>, InitialCheckError> {
        if let Some(derived) = self.pointer_fields.borrow().get(&layout) {
            return Ok(Rc::clone(derived));
        }
        let derived = self.derive_fixed_pointer_fields(layout)?;
        self.pointer_fields.borrow_mut().insert(layout, Rc::clone(&derived));
        Ok(derived)
    }

    pub(super) fn derive_fixed_pointer_fields(&self, layout: crate::layout::LayoutId) -> Result<Rc<[(u64, crate::layout::LayoutId)]>, InitialCheckError> {
        let descriptor = self.descriptor(layout)?;
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return Ok(Rc::from([]));
        }
        let mut derived = Vec::with_capacity(descriptor.fields().len());
        derived.extend(descriptor
            .fields()
            .iter()
            .enumerate()
            .filter_map(|(index, field)| match field.kind() {
                crate::layout::FieldKind::Pointer { pointee, slot: PointerSlot::Owned | PointerSlot::OwnedNullable } => Some((index as u64, pointee)),
                // A record child interned as an owned pointer value slot is
                // an inline field whose layout is that slot.
                crate::layout::FieldKind::Inline(slot) => match self.layouts.get(slot).map(|slot| slot.kind()) {
                    Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned | PointerSlot::OwnedNullable }) => Some((index as u64, *pointee)),
                    _ => None,
                },
                _ => None,
            }));
        Ok(Rc::from(derived))
    }

    /// Whether every value-bearing field of a fixed record or tuple has been
    /// initialized. This is deliberately exhaustive over field storage forms:
    /// an unrecognized inline layout is not evidence of initialization.
    pub(super) fn fixed_value_fields_initialized(
        &self,
        object: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<bool, InitialCheckError> {
        let descriptor = self.descriptor(layout)?;
        if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return Ok(false);
        }
        // Consolidation asks about every field after each incremental store.
        // Resolve the current allocation aliases once instead of rescanning all
        // fact tables and re-resolving identities independently per field.
        // This index is deliberately ephemeral: moves, joins and reclamation
        // can change the current MemoryState between operations.
        let Some(identity) = self.state.objects.get(&object).map(|state| state.identity) else {
            return Ok(false);
        };
        let aliases = self.state.objects.iter().filter_map(|(candidate, state)| {
            (state.identity == identity).then_some(*candidate)
        }).collect::<Vec<_>>();
        let scalar_stored = |slot| aliases.iter().any(|alias| {
            self.state.initialized_scalar_slots.contains(&(*alias, slot))
        });
        let object_stored = |slot| aliases.iter().any(|alias| {
            self.state.stored_objects.contains_key(&(*alias, slot))
        });
        let nullable_absent = |slot| aliases.iter().any(|alias| {
            self.state.nullable_absent_slots.contains(&(*alias, slot))
        });
        for (index, field) in descriptor.fields().iter().enumerate() {
            let slot = SlotIdentity::Constant(index as u64);
            let initialized = match field.kind() {
                FieldKind::Scalar(_) => scalar_stored(slot),
                FieldKind::Pointer {
                    slot: PointerSlot::Owned,
                    ..
                } => object_stored(slot),
                FieldKind::Pointer {
                    slot: PointerSlot::OwnedNullable,
                    ..
                } => {
                    object_stored(slot) || nullable_absent(slot)
                }
                FieldKind::Pointer { .. } => false,
                FieldKind::Inline(field_layout) => match self.descriptor(field_layout)?.kind() {
                    LayoutKind::Scalar(_) => scalar_stored(slot),
                    LayoutKind::RuntimeValueSlot { value }
                        if matches!(self.descriptor(*value)?.kind(), LayoutKind::Scalar(_)) =>
                    {
                        scalar_stored(slot)
                    }
                    LayoutKind::RuntimePointerValueSlot {
                        semantics: PointerSlot::Owned,
                        ..
                    } => object_stored(slot),
                    LayoutKind::RuntimePointerValueSlot {
                        semantics: PointerSlot::OwnedNullable,
                        ..
                    } => {
                        object_stored(slot) || nullable_absent(slot)
                    }
                    _ => false,
                },
            };
            if !initialized {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Unit fields have no stored bytes but still require an explicit store.
    pub(super) fn unit_fields_initialized(&self, object: ValueId, layout: crate::layout::LayoutId) -> Result<bool, InitialCheckError> {
        Ok(self.descriptor(layout)?.fields().iter().enumerate().all(|(index, field)| {
            field.kind() != FieldKind::Scalar(ScalarKind::Unit)
                || self.checked_scalar_slot_stored(object, SlotIdentity::Constant(index as u64))
        }))
    }

    /// Summarize a complete fixed aggregate so independently constructed
    /// children can join. Nullable child uniqueness has its own conditional
    /// fact and never depends on retaining an exact child identity here.
    pub(super) fn consolidate_fixed_fields(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let layout = self.live_object(object)?.layout;
        let pointer_fields = self.fixed_pointer_fields(layout)?;
        if !matches!(self.descriptor(layout)?.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
            return Ok(());
        }
        if !self.fixed_value_fields_initialized(object, layout)? {
            return Ok(());
        }
        for (index, _) in pointer_fields.iter() {
            self.state.stored_objects.remove(&(object, SlotIdentity::Constant(*index)));
        }
        self.state.objects.get_mut(&object).expect("live object exists").initialized_owned_slots = true;
        Ok(())
    }

    pub(super) fn reference_slot(
        &self,
        place: &Place,
    ) -> Result<(ValueId, SlotIdentity), InitialCheckError> {
        // A record field is a constant slot; a list element is its index.
        let index = match place.projection() {
            Projection::Element { index } => index,
            Projection::Field { index } => return Ok((place.object(), SlotIdentity::Constant(u64::from(*index)))),
            _ => return self.unsupported("reference slot(non-element)"),
        };
        let slot = self
            .state
            .constants
            .get(index)
            .copied()
            .map(SlotIdentity::Constant)
            .unwrap_or(SlotIdentity::Dynamic(*index));
        Ok((place.object(), slot))
    }

    pub(super) fn check_place(&mut self, place: &Place) -> Result<ValueType, InitialCheckError> {
        if matches!(place.projection(), Projection::Element { .. }) {
            self.promote_header_extent(place.object())?;
        }
        let object = self.live_object(place.object())?;
        let descriptor = self.descriptor(object.layout)?;
        let derived = match place.projection() {
            Projection::Header(field) => {
                match descriptor.header() {
                    HeaderLayout::PackedList {
                        length_offset,
                        capacity_offset,
                        data_offset,
                        ..
                    } => {
                        let offset = match field {
                            HeaderField::Length => length_offset,
                            HeaderField::Capacity => capacity_offset,
                        };
                        if offset >= data_offset {
                            return self.invalid_projection(place.object());
                        }
                    }
                    HeaderLayout::None
                        if *field == HeaderField::Length
                            && matches!(descriptor.kind(), LayoutKind::RuntimeBuffer { element }
                                if self.descriptor(*element).is_ok_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)))
                            && descriptor.size() == (LayoutSize::Dynamic { base: 4, stride: 1 }) => {}
                    _ => return self.invalid_projection(place.object()),
                }
                ValueType::Size
            }
            Projection::Element { index } => {
                let Extent::Counted { count, capacity, .. } = &object.extent else {
                    return self.invalid_projection(place.object());
                };
                let count = if place.access() == AccessMode::Write {
                    capacity
                } else {
                    self.state
                        .header_values
                        .get(&(place.object(), HeaderField::Length))
                        .unwrap_or(count)
                };
                self.require_type(*index, ValueType::Size)?;
                if !self.less_than_for_owner(place.object(), *index, *count) {
                    return Err(InitialCheckError::MissingBoundsGuard {
                        function: self.function.id,
                        index: *index,
                        count: *count,
                    });
                }
                let element = descriptor
                    .kind()
                    .packed_element_layout()
                    .ok_or(InitialCheckError::InvalidProjection {
                        function: self.function.id,
                        object: place.object(),
                    })?;
                // A raw byte has no value type of its own: a read of one is a
                // zero-extended `Size` (the byte loops behind string equality
                // and character counting), and a write stores a `Size` the
                // path proves fits a byte (`store` demands that proof).
                let raw_byte = self.descriptor(element).is_ok_and(|element| {
                    element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)
                });
                if matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot {
                    semantics: PointerSlot::OwnedNullable, ..
                }) {
                    return self.unsupported("Place(nullable collection element)");
                }
                if raw_byte {
                    ValueType::Size
                } else {
                    self.value_type_for_layout(element)?
                }
            }
            Projection::Field { index } => {
                if let LayoutKind::Scalar(kind) = descriptor.kind() {
                    if *kind != ScalarKind::Int
                        || *index != 0
                        || place.access() == AccessMode::Write && object.ownership != Ownership::Unique
                    {
                        return self.invalid_projection(place.object());
                    }
                    self.value_type_for_scalar(*kind)?
                } else {
                    let field = descriptor.fields().get(*index as usize).ok_or(
                        InitialCheckError::InvalidProjection {
                            function: self.function.id,
                            object: place.object(),
                        },
                    )?;
                // The callable lane carries no value type of its own: the
                // closure registry names the signature slot 0 must hold, and
                // an unregistered layout has no readable lane at all.
                    match field.kind() {
                        FieldKind::Scalar(ScalarKind::Callable) => {
                            let derived = self.closure_slot_type(place.object(), object.layout)?;
                            if place.value_type() != derived {
                                return Err(InitialCheckError::InvalidClosureSlot {
                                    function: self.function.id,
                                    object: place.object(),
                                });
                            }
                            derived
                        }
                        kind => self.value_type_for_field(kind)?,
                    }
                }
            }
            Projection::Slice { .. } => return self.unsupported("Place(Slice)"),
        };
        if place.value_type() != derived {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: derived,
                actual: place.value_type(),
            });
        }
        Ok(derived)
    }
}
