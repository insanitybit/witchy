use std::collections::{BTreeMap, BTreeSet};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutInterner, LayoutKind, LayoutSize, PointerSlot,
    RcHeader, ScalarKind,
};

use super::{InitialCheckError, value_type};
use crate::canonical_safewir::{
    AccessMode, AllocationClass, ArithmeticOperator, BasicBlock, BorrowMode, CallArgument,
    CallableContract, ComparisonOperator, Extent, Function, FunctionSignature, GuardKind,
    HeaderField, LoadTransfer, Operation, OverflowBehavior, Ownership, ParameterContract,
    ParameterOwnership, Place, Projection, ResultContract, ReturnedExtent, StoreTransfer, ValueId,
    ValueType,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectState {
    /// The allocation or entry parameter from which this reference originates.
    /// SSA edge arguments rename values, never object identity.
    identity: ValueId,
    layout: crate::layout::LayoutId,
    extent: Extent,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
    /// A `Shared` ABI boundary may carry either an RC object or immutable
    /// static data. This is deliberately distinct from an absent/unknown
    /// allocation class, which carries no authority.
    shared_immutable: bool,
    /// A managed ABI entry parameter denotes a complete logical value even
    /// though its dynamic extent is read at runtime rather than represented by
    /// local slot facts.
    initialized_owned_slots: bool,
}

#[derive(Debug, Clone, Copy)]
struct CheckedRange {
    object: ValueId,
    start: ValueId,
    length: ValueId,
    layout: crate::layout::LayoutId,
    element: crate::layout::LayoutId,
    identity: ValueId,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RangeIdentity {
    object: ValueId,
    start: ValueId,
    length: ValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeState {
    element: crate::layout::LayoutId,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
    initialized: bool,
    zero_width: bool,
    zero_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Comparison {
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
}

/// All derived memory facts at one program point.  This is deliberately a
/// checker-owned snapshot: transforms cannot attach a claimed range, extent,
/// or ownership fact to an edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryState {
    objects: BTreeMap<ValueId, ObjectState>,
    /// Physical identities that a Shared phi may select at runtime. The phi
    /// value is the SSA identity used by subsequent operations; this side
    /// relation prevents the checker from pretending distinct allocations
    /// became one allocation merely because control flow joined.
    shared_phi_origins: BTreeMap<ValueId, BTreeSet<ValueId>>,
    stored_objects: BTreeMap<(ValueId, SlotIdentity), ObjectState>,
    range_states: BTreeMap<RangeIdentity, RangeState>,
    constants: BTreeMap<ValueId, u64>,
    exact_additions: BTreeMap<ValueId, (ValueId, ValueId)>,
    comparisons: BTreeMap<ValueId, Comparison>,
    guards: BTreeSet<(ComparisonOperator, ValueId, ValueId)>,
    header_values: BTreeMap<(ValueId, HeaderField), ValueId>,
    static_data_objects: BTreeSet<ValueId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SlotIdentity {
    Constant(u64),
    Dynamic(ValueId),
}

pub(super) struct MemoryChecker<'a> {
    function: &'a Function,
    layouts: &'a LayoutInterner,
    state: MemoryState,
}

impl<'a> MemoryChecker<'a> {
    pub(super) fn new(
        function: &'a Function,
        layouts: &'a LayoutInterner,
        signature: &FunctionSignature,
    ) -> Result<Self, InitialCheckError> {
        let mut checker = Self {
            function,
            layouts,
            state: MemoryState {
                objects: BTreeMap::new(),
                shared_phi_origins: BTreeMap::new(),
                stored_objects: BTreeMap::new(),
                range_states: BTreeMap::new(),
                constants: BTreeMap::new(),
                exact_additions: BTreeMap::new(),
                comparisons: BTreeMap::new(),
                guards: BTreeSet::new(),
                header_values: BTreeMap::new(),
                static_data_objects: BTreeSet::new(),
            },
        };
        checker.initialize_parameters(signature)?;
        Ok(checker)
    }

    pub(super) fn snapshot(&self) -> MemoryState {
        self.state.clone()
    }

    pub(super) fn restore(&mut self, state: MemoryState) {
        self.state = state;
    }

    pub(super) fn constant_condition(&self, condition: ValueId) -> Option<bool> {
        let comparison = self.state.comparisons.get(&condition)?;
        let left = *self.state.constants.get(&comparison.left)?;
        let right = *self.state.constants.get(&comparison.right)?;
        let (signed_left, signed_right) = match value_type(self.function, comparison.left).ok()? {
            ValueType::Size => (
                i64::from(u32::try_from(left).ok()? as i32),
                i64::from(u32::try_from(right).ok()? as i32),
            ),
            ValueType::Int => (left as i64, right as i64),
            _ => return None,
        };
        Some(match comparison.operator {
            ComparisonOperator::Equal => left == right,
            ComparisonOperator::NotEqual => left != right,
            ComparisonOperator::LessSigned => signed_left < signed_right,
            ComparisonOperator::LessUnsigned => left < right,
            ComparisonOperator::LessEqualSigned => signed_left <= signed_right,
            ComparisonOperator::LessEqualUnsigned => left <= right,
            ComparisonOperator::GreaterSigned => signed_left > signed_right,
            ComparisonOperator::GreaterUnsigned => left > right,
            ComparisonOperator::GreaterEqualSigned => signed_left >= signed_right,
            ComparisonOperator::GreaterEqualUnsigned => left >= right,
        })
    }

    /// Rename facts through an explicit SSA edge.  A block parameter is a new
    /// definition, so any count, capacity, header value, guard, or object state
    /// carried by an argument must be expressed in terms of that definition.
    pub(super) fn transfer_edge(
        &self,
        target: &BasicBlock,
        arguments: &[ValueId],
    ) -> Result<MemoryState, InitialCheckError> {
        let mut renames = BTreeMap::new();
        let mut unique_phi_rebases = BTreeMap::new();
        let mut shared_phi_rebases = BTreeMap::new();
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if parameter.ownership == Some(Ownership::Unique) {
                if !matches!(parameter.value_type, ValueType::Ref(_)) {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                }
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                if state.ownership != Ownership::Unique
                    || !self.is_sole_live_token(*argument, state.identity)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                unique_phi_rebases.insert(*argument, parameter.value);
            }
            if parameter.ownership == Some(Ownership::Shared) {
                let ValueType::Ref(parameter_layout) = parameter.value_type else {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: parameter.value,
                    });
                };
                let state = self.state.objects.get(argument).ok_or(
                    InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    },
                )?;
                if state.ownership != Ownership::Shared || state.layout != parameter_layout {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: *argument,
                    });
                }
                shared_phi_rebases.insert(*argument, parameter.value);
            }
            if self.state.objects.contains_key(argument)
                && renames.insert(*argument, parameter.value).is_some_and(|previous| previous != parameter.value)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *argument,
                });
            }
            renames.insert(*argument, parameter.value);
        }

        let mut objects = BTreeMap::new();
        for (value, state) in &self.state.objects {
            let original_value = *value;
            let mut state = state.clone();
            state.extent = remap_extent(&renames, &state.extent);
            let value = remap_value(&renames, original_value);
            if let Some(identity) = unique_phi_rebases.get(&original_value) {
                state.identity = *identity;
            }
            if let Some(identity) = shared_phi_rebases.get(&original_value) {
                state.identity = *identity;
                match &state.extent {
                    Extent::Counted { count, capacity, .. }
                        if target.parameters.iter().any(|parameter| {
                            parameter.value == *count && parameter.value_type == ValueType::Size
                        }) && target.parameters.iter().any(|parameter| {
                            parameter.value == *capacity
                                && parameter.value_type == ValueType::Size
                        }) => {}
                    Extent::FixedBytes(_) => {}
                    _ => {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: value,
                        });
                    }
                }
            }
            if let Some(previous) = objects.insert(value, state.clone()) {
                if previous != state {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: value,
                    });
                }
            }
        }
        for parameter in &target.parameters {
            if parameter.value_type.is_memory_bearing() && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: parameter.value,
                });
            }
        }

        let mut shared_phi_origins = self
            .state
            .shared_phi_origins
            .iter()
            .map(|(value, origins)| (remap_value(&renames, *value), origins.clone()))
            .collect::<BTreeMap<_, _>>();
        for (argument, parameter) in shared_phi_rebases {
            let origins = self
                .state
                .shared_phi_origins
                .get(&argument)
                .cloned()
                .unwrap_or_else(|| BTreeSet::from([self.state.objects[&argument].identity]));
            shared_phi_origins
                .entry(parameter)
                .or_default()
                .extend(origins);
        }

        Ok(MemoryState {
            objects,
            shared_phi_origins,
            stored_objects: self
                .state
                .stored_objects
                .iter()
                .map(|((container, index), state)| {
                    (
                        (
                            remap_value(&renames, *container),
                            match index {
                                SlotIdentity::Constant(value) => SlotIdentity::Constant(*value),
                                SlotIdentity::Dynamic(value) => {
                                    SlotIdentity::Dynamic(remap_value(&renames, *value))
                                }
                            },
                        ),
                        state.clone(),
                    )
                })
                .collect(),
            range_states: self
                .state
                .range_states
                .iter()
                .map(|(identity, state)| {
                    (
                        RangeIdentity {
                            object: remap_value(&renames, identity.object),
                            start: remap_value(&renames, identity.start),
                            length: remap_value(&renames, identity.length),
                        },
                        *state,
                    )
                })
                .collect(),
            constants: self
                .state
                .constants
                .iter()
                .map(|(value, constant)| (remap_value(&renames, *value), *constant))
                .collect(),
            exact_additions: self
                .state
                .exact_additions
                .iter()
                .map(|(result, (left, right))| {
                    (
                        remap_value(&renames, *result),
                        (remap_value(&renames, *left), remap_value(&renames, *right)),
                    )
                })
                .collect(),
            comparisons: self
                .state
                .comparisons
                .iter()
                .map(|(value, comparison)| {
                    (
                        remap_value(&renames, *value),
                        Comparison {
                            operator: comparison.operator,
                            left: remap_value(&renames, comparison.left),
                            right: remap_value(&renames, comparison.right),
                        },
                    )
                })
                .collect(),
            guards: self
                .state
                .guards
                .iter()
                .map(|(operator, left, right)| {
                    (*operator, remap_value(&renames, *left), remap_value(&renames, *right))
                })
                .collect(),
            header_values: self
                .state
                .header_values
                .iter()
                .map(|((object, field), value)| {
                    (
                        (remap_value(&renames, *object), *field),
                        remap_value(&renames, *value),
                    )
                })
                .collect(),
            static_data_objects: self
                .state
                .static_data_objects
                .iter()
                .map(|object| remap_value(&renames, *object))
                .collect(),
        })
    }

    /// A Unique phi may change object identity only when its edge argument is
    /// the sole remaining live representation of that identity.  Container
    /// slots count: rebasing a value while the same object remains stored
    /// elsewhere would leave a physical alias under the old identity.
    fn is_sole_live_token(&self, value: ValueId, identity: ValueId) -> bool {
        self.state.objects.iter().all(|(candidate, state)| {
            state.identity != identity || state.ownership == Ownership::Consumed || *candidate == value
        }) && self.state.stored_objects.values().all(|state| {
            state.identity != identity || state.ownership == Ownership::Consumed
        })
    }

    /// Meet facts from predecessor edges.  Scalar facts are usable only when
    /// every predecessor establishes the same fact.  An object state is never
    /// widened: different allocation identity, ownership, layout, or extent is
    /// an explicit unsafe join rather than an inferred alias relation.
    pub(super) fn meet(
        &self,
        block: &BasicBlock,
        existing: &MemoryState,
        incoming: &MemoryState,
    ) -> Result<MemoryState, InitialCheckError> {
        let mut objects = BTreeMap::new();
        // Analysis facts may be intersected, but owned references are release
        // obligations. An incoming edge must not erase a live affine token.
        for (state, other) in [(existing, incoming), (incoming, existing)] {
            for (value, object) in &state.objects {
                if object.ownership != Ownership::Consumed
                    && (object.allocation_class == Some(AllocationClass::ReferenceCounted)
                        || object.shared_immutable)
                    && !other.objects.contains_key(value)
                {
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: *value,
                    });
                }
            }
            for (slot, object) in &state.stored_objects {
                if other.stored_objects.get(slot) != Some(object) {
                    return Err(InitialCheckError::IncompatibleMemoryJoin {
                        function: self.function.id,
                        block: block.id,
                        object: slot.0,
                    });
                }
            }
        }
        for (value, current) in &existing.objects {
            let Some(next) = incoming.objects.get(value) else {
                continue;
            };
            if current != next {
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: *value,
                });
            }
            objects.insert(*value, current.clone());
        }
        for parameter in &block.parameters {
            if parameter.value_type.is_memory_bearing() && !objects.contains_key(&parameter.value) {
                return Err(InitialCheckError::IncompatibleMemoryJoin {
                    function: self.function.id,
                    block: block.id,
                    object: parameter.value,
                });
            }
        }

        Ok(MemoryState {
            objects,
            shared_phi_origins: meet_shared_phi_origins(existing, incoming),
            stored_objects: intersect_map(&existing.stored_objects, &incoming.stored_objects),
            range_states: meet_range_states(existing, incoming),
            constants: intersect_map(&existing.constants, &incoming.constants),
            exact_additions: intersect_map(&existing.exact_additions, &incoming.exact_additions),
            comparisons: intersect_map(&existing.comparisons, &incoming.comparisons),
            guards: existing.guards.intersection(&incoming.guards).copied().collect(),
            header_values: intersect_map(&existing.header_values, &incoming.header_values),
            static_data_objects: existing
                .static_data_objects
                .intersection(&incoming.static_data_objects)
                .copied()
                .collect(),
        })
    }

    fn initialize_parameters(
        &mut self,
        signature: &FunctionSignature,
    ) -> Result<(), InitialCheckError> {
        let entry = self
            .function
            .blocks
            .iter()
            .find(|block| block.id == self.function.entry)
            .ok_or(InitialCheckError::MissingBlock {
                function: self.function.id,
                block: self.function.entry,
            })?;
        for (index, parameter) in entry.parameters.iter().enumerate() {
            let ValueType::Ref(layout) = parameter.value_type else {
                continue;
            };
            let contract = signature
                .contract
                .parameters
                .get(index)
                .ok_or(InitialCheckError::UnsupportedMemorySignature {
                    signature: signature.id,
                })?;
            let ownership = match contract.ownership {
                ParameterOwnership::Unique => Ownership::Unique,
                ParameterOwnership::Shared => Ownership::Shared,
                ParameterOwnership::CallBorrow { mode: BorrowMode::Read } => Ownership::Shared,
                ParameterOwnership::CallBorrow { mode: BorrowMode::Write } => Ownership::Unique,
                ParameterOwnership::Value => {
                    continue;
                }
            };
            let extent = self.parameter_extent(layout)?;
            let (allocation_class, shared_immutable) = match contract.ownership {
                ParameterOwnership::Unique => (Some(AllocationClass::ReferenceCounted), false),
                ParameterOwnership::Shared => (None, true),
                ParameterOwnership::CallBorrow { .. } => (None, false),
                ParameterOwnership::Value => unreachable!(),
            };
            let initialized_owned_slots = matches!(
                contract.ownership,
                ParameterOwnership::Unique | ParameterOwnership::Shared
            );
            self.state.objects.insert(
                parameter.value,
                ObjectState {
                    identity: parameter.value,
                    layout,
                    extent,
                    ownership,
                    allocation_class,
                    shared_immutable,
                    initialized_owned_slots,
                },
            );
        }
        Ok(())
    }

    fn parameter_extent(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<Extent, InitialCheckError> {
        match self.descriptor(layout)?.size() {
            LayoutSize::Fixed(bytes) => Ok(Extent::FixedBytes(u64::from(bytes))),
            LayoutSize::Dynamic { base, .. } => {
                Ok(Extent::HeaderPrefix { minimum_bytes: base })
            }
        }
    }

    pub(super) fn check_call(
        &mut self,
        arguments: &[CallArgument],
        parameters: &[ParameterContract],
        destinations: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        for (argument, parameter) in arguments.iter().zip(parameters) {
            let value = match argument {
                CallArgument::Value(value)
                | CallArgument::Move(value)
                | CallArgument::Share(value) => *value,
                CallArgument::Borrow(borrow) => borrow.source(),
            };
            let actual = value_type(self.function, value)?;
            if !actual.is_linear_memory_ref() {
                if !matches!(argument, CallArgument::Value(_)) {
                    return self.unsupported("Call(non-memory ownership)");
                }
                continue;
            }
            match argument {
                CallArgument::Value(_) => return self.unsupported("Call(Value memory)"),
                CallArgument::Share(_) => return self.unsupported("Call(Share)"),
                CallArgument::Move(value) => {
                    self.require_owned_slots_initialized(*value, "Call(uninitialized owned slot)")?;
                    self.consume_call_move(*value, parameter.ownership)?;
                }
                CallArgument::Borrow(borrow) => {
                    if !matches!(
                        parameter.ownership,
                        ParameterOwnership::CallBorrow { mode } if mode == borrow.mode()
                    ) {
                        return Err(InitialCheckError::UnsupportedBorrowArgument {
                            function: self.function.id,
                        });
                    }
                    self.check_call_borrow(*borrow)?;
                }
            }
        }
        for (destination, result) in destinations.iter().zip(results) {
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::CheckedValue { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Fresh { layout, class, .. } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Unique,
                            allocation_class: Some(*class),
                            shared_immutable: false,
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::Shared { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: true,
                            // A checked Shared result is a complete logical
                            // value at the ABI boundary. This is distinct from
                            // locally allocated or Fresh storage, which needs
                            // slot/range evidence before it can escape.
                            initialized_owned_slots: true,
                        },
                    );
                }
                ResultContract::StaticData { layout } => {
                    self.require_ref_type(*destination, *layout)?;
                    let extent = self.parameter_extent(*layout)?;
                    self.state.objects.insert(
                        *destination,
                        ObjectState {
                            identity: *destination,
                            layout: *layout,
                            extent,
                            ownership: Ownership::Shared,
                            allocation_class: None,
                            shared_immutable: false,
                            initialized_owned_slots: false,
                        },
                    );
                    self.state.static_data_objects.insert(*destination);
                }
                _ => return self.unsupported("Call(memory result)"),
            }
        }
        Ok(())
    }

    pub(super) fn check_exit(&self, returned: &[ValueId]) -> Result<(), InitialCheckError> {
        for (object, state) in &self.state.objects {
            if state.ownership != Ownership::Consumed
                && (state.allocation_class == Some(AllocationClass::ReferenceCounted)
                    || state.shared_immutable)
                && !returned.contains(object)
            {
                return Err(InitialCheckError::UnconsumedReference {
                    function: self.function.id,
                    object: *object,
                });
            }
        }
        Ok(())
    }

    pub(super) fn check_return(
        &self,
        values: &[ValueId],
        results: &[ResultContract],
    ) -> Result<(), InitialCheckError> {
        let mut transferred = BTreeSet::new();
        for (value, result) in values.iter().zip(results) {
            // Each result lane transfers one token. Repeating an SSA reference
            // does not execute CloneRef, even when both lanes claim Shared.
            if matches!(value_type(self.function, *value)?, ValueType::Ref(_))
                && !matches!(result, ResultContract::StaticData { .. })
                && !transferred.insert(*value)
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: *value,
                });
            }
            match result {
                ResultContract::Value(value_type) if !value_type.is_linear_memory_ref() => {}
                ResultContract::CheckedValue { layout } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::Fresh { layout, class, extent } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Unique
                        || state.allocation_class != Some(*class)
                        || !returned_extent_matches(&state.extent, extent)
                        || !self.is_sole_live_token(*value, state.identity)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::StaticData { layout } => {
                    let state = self.live_object(*value)?;
                    if state.layout != *layout
                        || state.ownership != Ownership::Shared
                        || !self.state.static_data_objects.contains(value)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                ResultContract::Shared { layout } => {
                    self.require_owned_slots_initialized(*value, "Return(uninitialized owned slot)")?;
                    let state = self.live_object(*value)?;
                    // Shared owns either an RC token or immutable static data.
                    // The union never grants mutation or reclamation authority.
                    if state.layout != *layout
                        || !matches!(state.ownership, Ownership::Unique | Ownership::Shared)
                    {
                        return Err(InitialCheckError::InvalidOwnership {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                    if !self.is_shared_immutable_storage(*value, state) {
                        return Err(InitialCheckError::InvalidAllocationClass {
                            function: self.function.id,
                            object: *value,
                        });
                    }
                }
                _ => return self.unsupported("Return(memory result contract)"),
            }
        }
        Ok(())
    }

    pub(super) fn check_host_buffer(
        &mut self,
        place: &Place,
        extent: &crate::canonical_safewir::HostBufferExtent,
        import: crate::canonical_safewir::ImportId,
        parameter: usize,
    ) -> Result<(), InitialCheckError> {
        let Projection::Slice { start, length } = place.projection() else {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        };
        self.promote_header_extent(place.object())?;
        let object = self.live_object(place.object())?.clone();
        self.require_ref_type(place.object(), object.layout)?;
        if place.value_type() != ValueType::Ref(object.layout) {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: ValueType::Ref(object.layout),
                actual: place.value_type(),
            });
        }
        if place.access() == AccessMode::Write {
            if object.ownership != Ownership::Unique
                || self.state.static_data_objects.contains(&place.object())
            {
                return Err(InitialCheckError::InvalidOwnership {
                    function: self.function.id,
                    object: place.object(),
                });
            }
        }
        self.require_type(*start, ValueType::Size)?;
        self.require_type(*length, ValueType::Size)?;
        let Extent::Counted { count, stride_bytes, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        if stride_bytes != 1 || !self.is_raw_byte_buffer(object.layout)? {
            return Err(InitialCheckError::UnsupportedHostBuffer {
                function: self.function.id,
                import,
                parameter,
            });
        }
        let logical_count = self
            .state
            .header_values
            .get(&(place.object(), HeaderField::Length))
            .copied()
            .unwrap_or(count);
        let starts_at_zero = self.state.constants.get(start) == Some(&0);
        let whole_logical_body = starts_at_zero && *length == logical_count;
        let constant_range = self
            .state
            .constants
            .get(start)
            .zip(self.state.constants.get(length))
            .zip(self.state.constants.get(&logical_count))
            .is_some_and(|((start, length), count)| {
                start.checked_add(*length).is_some_and(|end| end <= *count)
            });
        if !whole_logical_body && !constant_range {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        let constant_bytes = self
            .state
            .constants
            .get(length)
            .and_then(|length| length.checked_mul(u64::from(stride_bytes)));
        match extent {
            crate::canonical_safewir::HostBufferExtent::Slice => {}
            crate::canonical_safewir::HostBufferExtent::Exact(bytes)
                if constant_bytes == Some(*bytes) => {}
            crate::canonical_safewir::HostBufferExtent::AtLeast(bytes)
                if constant_bytes.is_some_and(|actual| actual >= *bytes) => {}
            crate::canonical_safewir::HostBufferExtent::Exact(_)
            | crate::canonical_safewir::HostBufferExtent::AtLeast(_)
            | crate::canonical_safewir::HostBufferExtent::Unknown => {
                return Err(InitialCheckError::HostBufferExtentMismatch {
                    function: self.function.id,
                    import,
                    parameter,
                });
            }
        }
        Ok(())
    }

    fn copy_bytes(
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
        Ok(())
    }

    fn move_range(
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
            let moved = self.require_initialized_range(source, destination.element)?;
            self.require_destination_range_empty(destination)?;
            self.state.stored_objects.retain(|(object, _), _| *object != source.object);
            self.state.range_states.retain(|identity, _| {
                identity.object != source.object && identity.object != destination.object
            });
            self.state.range_states.insert(
                RangeIdentity { object: destination.object, start: destination.start, length: destination.length },
                RangeState { element: destination.element, ownership: Ownership::Unique, allocation_class: moved.allocation_class, initialized: true, zero_width: self.is_zero(count), zero_start: self.is_zero(destination.start) },
            );
            self.state.range_states.insert(
                RangeIdentity { object: source.object, start: source.start, length: source.length },
                RangeState { element: source.element, ownership: Ownership::Consumed, allocation_class: moved.allocation_class, initialized: false, zero_width: self.is_zero(count), zero_start: self.is_zero(source.start) },
            );
        } else {
            self.require_scalar_range_element(destination.element, "MoveRange")?;
        }
        if destination.ownership != Ownership::Unique
            || source.ownership != Ownership::Unique
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

    fn require_source_range_complete(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
        if self.state.stored_objects.iter().any(|((object, slot), _)| {
            *object == range.object && !self.slot_in_range(*slot, range)
        }) || self.state.range_states.iter().any(|(identity, state)| {
            identity.object == range.object
                && state.initialized
                && !self.is_zero(identity.length)
                && !(self.same_scalar(identity.start, range.start)
                    && self.same_scalar(identity.length, range.length))
        }) {
            return self.unsupported("MoveRange(source-partial)");
        }
        Ok(())
    }

    fn slot_in_range(&self, slot: SlotIdentity, range: CheckedRange) -> bool {
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

    fn require_initialized_range(
        &self,
        range: CheckedRange,
        element: crate::layout::LayoutId,
    ) -> Result<RangeState, InitialCheckError> {
        if let Some(state) = self.state.range_states.iter().find_map(|(identity, state)| {
            (identity.object == range.object
                && self.same_scalar(identity.start, range.start)
                && self.same_scalar(identity.length, range.length)
                && state.element == element
                && state.ownership == Ownership::Unique
                && state.initialized
                && (!state.zero_width || self.is_zero(range.length)))
                .then_some(*state)
        }) {
            return Ok(state);
        }
        let Some(start) = self.state.constants.get(&range.start).copied() else {
            return self.unsupported("MoveRange(uninitialized-source)");
        };
        let Some(length) = self.state.constants.get(&range.length).copied() else {
            return self.unsupported("MoveRange(uninitialized-source)");
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let end = start.checked_add(length).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation: "MoveRange(uninitialized-source)",
        })?;
        let mut allocation_class = None;
        for index in start..end {
            let Some(stored) = self.state.stored_objects.get(&(range.object, SlotIdentity::Constant(index))) else {
                return self.unsupported("MoveRange(uninitialized-source)");
            };
            if stored.ownership != Ownership::Unique || stored.layout != pointee {
                return self.unsupported("MoveRange(uninitialized-source)");
            }
            if allocation_class.is_some() && allocation_class != stored.allocation_class {
                return self.unsupported("MoveRange(incompatible element storage)");
            }
            allocation_class = stored.allocation_class;
        }
        Ok(RangeState { element, ownership: Ownership::Unique, allocation_class, initialized: true, zero_width: self.is_zero(range.length), zero_start: self.is_zero(range.start) })
    }

    fn require_owned_slots_initialized(
        &self,
        object: ValueId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
        if self.state.static_data_objects.contains(&object) {
            return Ok(());
        }
        let state = self.live_object(object)?;
        let allocation_identity = state.identity;
        if state.initialized_owned_slots {
            return Ok(());
        }
        let descriptor = self.descriptor(state.layout)?;
        let Some(element) = descriptor.kind().packed_element_layout() else {
            return Ok(());
        };
        if !self.value_type_for_layout(element)?.is_memory_bearing() {
            return Ok(());
        }
        let Extent::Counted { count, .. } = state.extent else {
            return self.unsupported(operation);
        };
        if self.is_zero(count) {
            return Ok(());
        }
        if self.state.range_states.iter().any(|(identity, range)| {
            identity.object == allocation_identity
                && self.same_scalar(identity.length, count)
                && range.element == element
                && range.initialized
                && range.zero_start
        }) {
            return Ok(());
        }
        let Some(count) = self.state.constants.get(&count).copied() else {
            return self.unsupported(operation);
        };
        let pointee = self.owned_pointer_slot_pointee(element)?;
        let mut initialized_slots = BTreeSet::new();
        for ((stored_object, slot), stored) in &self.state.stored_objects {
            if *stored_object != allocation_identity {
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

    fn require_destination_range_empty(&self, range: CheckedRange) -> Result<(), InitialCheckError> {
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

    fn clone_range(
        &mut self,
        destination: &Place,
        source: &Place,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
        let destination = self.check_typed_range(destination, AccessMode::Write, "CloneRange")?;
        let source = self.check_typed_range(source, AccessMode::Read, "CloneRange")?;
        self.check_range_pair(destination, source, count, "CloneRange")?;

        // CloneRange would need to retain every shared pointer element and
        // roll back already-cloned elements on failure. Neither per-element
        // ownership nor rollback is representable in the v1 operation, so
        // pointer-bearing ranges fail closed until the typed transfer kernel
        // exists. Scalar cloning is an ordinary value copy.
        self.require_scalar_range_element(destination.element, "CloneRange")?;
        if destination.ownership != Ownership::Unique
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
        }
        Ok(())
    }

    fn check_typed_range(
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
        if place.value_type() != ValueType::Ref(state.layout) {
            return Err(InitialCheckError::TypeMismatch {
                function: self.function.id,
                value: place.object(),
                expected: ValueType::Ref(state.layout),
                actual: place.value_type(),
            });
        }
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
        let Some(element) = descriptor.kind().packed_element_layout() else {
            return self.unsupported("range(non-packed-layout)");
        };
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
        let bound = if required_access == AccessMode::Write { capacity } else { logical_count };
        if !self.range_within(*start, *length, bound) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
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

    fn check_range_pair(
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

    fn require_scalar_range_element(
        &self,
        element: crate::layout::LayoutId,
        operation: &'static str,
    ) -> Result<(), InitialCheckError> {
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

    fn range_within(&self, start: ValueId, length: ValueId, count: ValueId) -> bool {
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
        from_body_start || guarded_exact_end || constant_range
    }

    fn check_byte_slice(
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
        let Extent::Counted { count, stride_bytes: 1, .. } = object.extent else {
            return Err(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object: place.object(),
            });
        };
        let logical_count = self
            .state
            .header_values
            .get(&(place.object(), HeaderField::Length))
            .copied()
            .unwrap_or(count);
        if !self.range_within(*start, *length, logical_count) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        Ok(*length)
    }

    fn is_raw_byte_buffer(
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

    fn check_call_borrow(
        &self,
        borrow: crate::canonical_safewir::CallBorrow,
    ) -> Result<(), InitialCheckError> {
        let state = self.live_object(borrow.source())?;
        let permitted = match borrow.mode() {
            BorrowMode::Read => matches!(state.ownership, Ownership::Unique | Ownership::Shared),
            BorrowMode::Write => state.ownership == Ownership::Unique,
        };
        if permitted {
            Ok(())
        } else {
            Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: borrow.source(),
            })
        }
    }

    fn consume_call_move(
        &mut self,
        source: ValueId,
        parameter: ParameterOwnership,
    ) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?;
        let permitted = match parameter {
            ParameterOwnership::Unique => state.ownership == Ownership::Unique,
            ParameterOwnership::Shared => {
                matches!(state.ownership, Ownership::Unique | Ownership::Shared)
            }
            _ => false,
        };
        if !permitted {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        if parameter == ParameterOwnership::Shared && self.is_shared_immutable_storage(source, state) {
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
        if parameter == ParameterOwnership::Unique
            && !self.is_sole_live_token(source, state.identity)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    /// Returns true when this operation belongs to the memory judgment.
    pub(super) fn check(&mut self, operation: &Operation) -> Result<bool, InitialCheckError> {
        match operation {
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
                    self.state.guards
                        .insert((comparison.operator, comparison.left, comparison.right));
                }
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
                Ok(true)
            }
            Operation::Store { place, value, transfer } => {
                self.store(place, *value, *transfer)?;
                Ok(true)
            }
            Operation::UpdateCount { object, count } => {
                self.update_count(*object, *count)?;
                Ok(true)
            }
            Operation::MoveValue { destination, source }
                if value_type(self.function, *source)?.is_memory_bearing() =>
            {
                self.move_value(*destination, *source)?;
                Ok(true)
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
                self.reclaim(*object)?;
                Ok(true)
            }
            Operation::CopyBytes { destination, source, length } => {
                self.copy_bytes(destination, source, *length)?;
                Ok(true)
            }
            Operation::MoveRange { destination, source, count } => {
                self.move_range(destination, source, *count)?;
                Ok(true)
            }
            Operation::CloneRange { destination, source, count } => {
                self.clone_range(destination, source, *count)?;
                Ok(true)
            }
            Operation::Retain { .. } => Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: operation_name(operation),
            }),
            _ => Ok(false),
        }
    }

    fn allocate(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
        class: AllocationClass,
    ) -> Result<(), InitialCheckError> {
        if class == AllocationClass::Scratch {
            return Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "Alloc(Scratch)",
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
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Unique,
                allocation_class: Some(class),
                shared_immutable: false,
                initialized_owned_slots: false,
            },
        );
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

    fn static_data(
        &mut self,
        destination: ValueId,
        layout: crate::layout::LayoutId,
        extent: &Extent,
    ) -> Result<(), InitialCheckError> {
        self.require_ref_type(destination, layout)?;
        let descriptor = self.descriptor(layout)?;
        self.check_extent(destination, extent, descriptor, true)?;
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Shared,
                allocation_class: None,
                shared_immutable: false,
                initialized_owned_slots: false,
            },
        );
        self.state.static_data_objects.insert(destination);
        Ok(())
    }

    fn check_extent(
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

    fn load(
        &mut self,
        destination: ValueId,
        place: &Place,
        transfer: LoadTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Read {
            return self.unsupported("Load(access)");
        }
        let expected = self.check_place(place)?;
        if !expected.is_memory_bearing() {
            if transfer != LoadTransfer::CopyScalar {
                return self.unsupported("Load(scalar transfer)");
            }
            self.require_type(destination, expected)?;
            if let Projection::Header(field) = place.projection() {
                self.state
                    .header_values
                    .insert((place.object(), *field), destination);
            }
            return Ok(());
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Load(non-linear reference)");
        };
        self.require_ref_type(destination, layout)?;
        let key = self.reference_slot(place)?;
        let mut stored = if let Some(stored) = self.state.stored_objects.get(&key).cloned() {
            stored
        } else {
            if transfer != LoadTransfer::CloneShared {
                return self.unsupported("Load(dynamic unique move)");
            }
            let index = match place.projection() {
                Projection::Element { index } => *index,
                _ => return self.unsupported("Load(uninitialized reference)"),
            };
            let range = self.state.range_states.iter().find_map(|(identity, state)| {
                (identity.object == place.object()
                    && state.initialized
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
            let stored = ObjectState {
                identity: destination,
                layout,
                extent,
                ownership: Ownership::Shared,
                allocation_class: range.1.allocation_class,
                shared_immutable: false,
                initialized_owned_slots: false,
            };
            self.state.objects.insert(destination, stored);
            return Ok(());
        };
        match transfer {
            LoadTransfer::CloneShared => {
                if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                    || !Self::is_managed_storage(&stored)
                {
                    return Err(InitialCheckError::InvalidOwnership {
                        function: self.function.id,
                        object: place.object(),
                    });
                }
                stored.ownership = Ownership::Shared;
                self.state.stored_objects.insert(key, stored.clone());
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

    fn store(
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
        if self.live_object(place.object())?.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        let expected = self.check_place(place)?;
        if !expected.is_memory_bearing() {
            if transfer != StoreTransfer::CopyScalar {
                return self.unsupported("Store(scalar transfer)");
            }
            return self.require_type(value, expected);
        }
        let ValueType::Ref(layout) = expected else {
            return self.unsupported("Store(non-linear reference)");
        };
        self.require_ref_type(value, layout)?;
        let key = self.reference_slot(place)?;
        if self.state.stored_objects.contains_key(&key) {
            return self.unsupported("Store(overwrite reference)");
        }
        if let Projection::Element { index } = place.projection() {
            if self.state.range_states.iter().any(|(identity, state)| {
                identity.object == place.object()
                    && state.initialized
                    && state.zero_start
                    && self.strict_less(*index, identity.length)
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
            }
            StoreTransfer::CopyScalar => return self.unsupported("Store(reference copy)"),
        }
        self.state.stored_objects.insert(key, stored);
        Ok(())
    }

    fn reference_slot(
        &self,
        place: &Place,
    ) -> Result<(ValueId, SlotIdentity), InitialCheckError> {
        let Projection::Element { index } = place.projection() else {
            return self.unsupported("reference slot(non-element)");
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

    fn check_place(&mut self, place: &Place) -> Result<ValueType, InitialCheckError> {
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
                if !self.less_than(*index, *count) {
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
                self.value_type_for_layout(element)?
            }
            Projection::Field { index } => {
                let field = descriptor.fields().get(*index as usize).ok_or(
                    InitialCheckError::InvalidProjection {
                        function: self.function.id,
                        object: place.object(),
                    },
                )?;
                self.value_type_for_field(field.kind())?
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

    fn promote_header_extent(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?.clone();
        let Extent::HeaderPrefix { minimum_bytes } = state.extent else {
            return Ok(());
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
        let count = self
            .state.header_values
            .get(&(object, HeaderField::Length))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        let is_raw_runtime_buffer = matches!(self.descriptor(state.layout)?.kind(), LayoutKind::RuntimeBuffer { element }
            if self.descriptor(*element).is_ok_and(|element| element.kind() == &LayoutKind::Scalar(ScalarKind::RawByte)));
        if is_raw_runtime_buffer {
            if !self.allocation_fits(count, base, stride) {
                return Err(InitialCheckError::MissingAllocationSizeGuard {
                    function: self.function.id,
                    capacity: count,
                    maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
                });
            }
            self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
                header_bytes: base,
                count,
                capacity: count,
                stride_bytes: stride,
            };
            return Ok(());
        }
        let capacity = self
            .state.header_values
            .get(&(object, HeaderField::Capacity))
            .copied()
            .ok_or(InitialCheckError::InvalidExtent {
                function: self.function.id,
                object,
            })?;
        if !self.less_equal(count, capacity) {
            return Err(InitialCheckError::MissingCapacityGuard {
                function: self.function.id,
                count,
                capacity,
            });
        }
        if !self.allocation_fits(capacity, base, stride) {
            return Err(InitialCheckError::MissingAllocationSizeGuard {
                function: self.function.id,
                capacity,
                maximum: (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride),
            });
        }
        self.consolidate_pointer_prefix(object, count)?;
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes: base,
            count,
            capacity,
            stride_bytes: stride,
        };
        Ok(())
    }

    fn update_count(&mut self, object: ValueId, count: ValueId) -> Result<(), InitialCheckError> {
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
        let Extent::Counted { header_bytes, capacity, stride_bytes, .. } = state.extent else {
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
        self.consolidate_pointer_prefix(object, count)?;
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        };
        self.state.header_values.insert((object, HeaderField::Length), count);
        Ok(())
    }

    fn consolidate_pointer_prefix(
        &mut self,
        object: ValueId,
        count: ValueId,
    ) -> Result<(), InitialCheckError> {
        let layout = self.live_object(object)?.layout;
        let Some(element) = self.descriptor(layout)?.kind().packed_element_layout() else {
            return Ok(());
        };
        // Only the catalog's owned pointer slot is a v1 aggregate whose
        // initialized prefix carries object ownership. Raw bytes and boolean
        // value slots are pointer-free storage, not SafeWIR value types, and
        // therefore have no prefix ownership facts to consolidate.
        if !matches!(self.descriptor(element)?.kind(), LayoutKind::RuntimePointerValueSlot { .. }) {
            return Ok(());
        }
        let pointee = self.owned_pointer_slot_pointee(element)?;
        if self.is_zero(count) {
            if self.state.stored_objects.keys().any(|(stored_object, _)| *stored_object == object)
                || self.state.range_states.iter().any(|(identity, state)| {
                    identity.object == object && state.initialized && !self.is_zero(identity.length)
                })
            {
                return self.unsupported("UpdateCount(pointer shrink with live elements)");
            }
            self.state.range_states.retain(|identity, _| identity.object != object);
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
        let existing = self.state.range_states.iter().find_map(|(identity, state)| {
            (identity.object == object && state.initialized && state.zero_start)
                .then_some((*identity, *state))
        }).ok_or(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation: "UpdateCount(pointer prefix missing)",
        })?;
        if self.same_scalar(existing.0.length, count) {
            if existing.0.length != count {
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
        });
        if one_step {
            let key = self.slot_key(object, existing.0.length);
            let stored = self.state.stored_objects.remove(&key).ok_or(
                InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "UpdateCount(pointer slot uninitialized)",
                },
            )?;
            if stored.layout != pointee {
                return self.unsupported("UpdateCount(pointer slot layout)");
            }
            if stored.ownership != Ownership::Unique {
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
                    ownership: Ownership::Unique,
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
            if stored.ownership != Ownership::Unique {
                return self.unsupported("UpdateCount(pointer slot ownership)");
            }
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
            RangeState { element, ownership: Ownership::Unique, allocation_class, initialized: true, zero_width: self.is_zero(count), zero_start: existing.1.zero_start },
        );
        Ok(())
    }

    fn slot_key(&self, object: ValueId, index: ValueId) -> (ValueId, SlotIdentity) {
        (
            object,
            self.state.constants.get(&index).copied()
                .map(SlotIdentity::Constant)
                .unwrap_or(SlotIdentity::Dynamic(index)),
        )
    }

    fn move_value(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
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
        self.state.objects.insert(destination, state);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn is_shared_immutable_storage(&self, object: ValueId, state: &ObjectState) -> bool {
        Self::is_managed_storage(state) || self.state.static_data_objects.contains(&object)
    }

    fn is_managed_storage(state: &ObjectState) -> bool {
        match state.allocation_class {
            Some(AllocationClass::ReferenceCounted) => true,
            Some(AllocationClass::Bump | AllocationClass::Scratch) => false,
            None => state.shared_immutable,
        }
    }

    fn clone_ref(
        &mut self,
        destination: ValueId,
        source: ValueId,
    ) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        let ValueType::Ref(layout) = source_type else {
            return self.unsupported("CloneRef(scalar)");
        };
        self.require_ref_type(destination, layout)?;
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
        if source_is_static {
            self.state.static_data_objects.insert(destination);
        }
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Shared;
        Ok(())
    }

    fn release(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
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
        self.require_owned_slots_initialized(object, "Release(uninitialized owned slot)")?;
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn drop_ref(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(object)?;
        if !self.is_shared_immutable_storage(object, state) {
            return Err(InitialCheckError::InvalidAllocationClass {
                function: self.function.id,
                object,
            });
        }
        self.require_owned_slots_initialized(object, "DropRef(uninitialized owned slot)")?;
        let descriptor = self.layouts.get(state.layout).ok_or(
            InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            },
        )?;
        if !drop_ref_layout_supported(self.layouts, descriptor.layout_id(), &mut BTreeSet::new()) {
            return self.unsupported("DropRef(pointer-bearing recursive destructor)");
        }
        self.state.objects
            .get_mut(&object)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn reclaim(&mut self, object: ValueId) -> Result<(), InitialCheckError> {
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
        if self.state.range_states.iter().any(|(range, state)| {
            range.object == object && state.initialized && !self.is_zero(range.length)
        }) {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object,
            });
        }
        self.state.objects.get_mut(&object).expect("live object exists").ownership = Ownership::Consumed;
        Ok(())
    }

    fn live_object(&self, object: ValueId) -> Result<&ObjectState, InitialCheckError> {
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

    fn less_than(&self, left: ValueId, right: ValueId) -> bool {
        self.state.guards
            .iter()
            .any(|(operator, guarded_left, guarded_right)| {
                *operator == ComparisonOperator::LessUnsigned
                    && self.same_scalar(left, *guarded_left)
                    && self.same_scalar(right, *guarded_right)
            })
    }

    fn strict_less(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left < right)
            || self.less_than(left, right)
    }

    fn same_scalar(&self, left: ValueId, right: ValueId) -> bool {
        left == right
            || self.state.constants.get(&left).zip(self.state.constants.get(&right))
                .is_some_and(|(left, right)| left == right)
            || self.state.header_values.iter().any(|((object, field), loaded)| {
                let Some(state) = self.state.objects.get(object) else {
                    return false;
                };
                let Extent::Counted { count, capacity, .. } = state.extent else {
                    return false;
                };
                let authoritative = match field {
                    HeaderField::Length => count,
                    HeaderField::Capacity => capacity,
                };
                (*loaded == left && authoritative == right)
                    || (*loaded == right && authoritative == left)
            })
    }

    fn is_zero(&self, value: ValueId) -> bool {
        self.state.constants.get(&value) == Some(&0)
    }

    fn less_equal(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left <= right)
            || self.is_zero(left)
            || self
                .state.guards
                .iter()
                .any(|(operator, guarded_left, guarded_right)| {
                    *operator == ComparisonOperator::LessEqualUnsigned
                        && self.same_scalar(left, *guarded_left)
                        && self.same_scalar(right, *guarded_right)
                })
            || self.same_scalar(left, right)
    }

    /// A wrapping addition is exact only when the current path proves that its
    /// mathematical sum fits the `Size` domain.  The operation remains a wrap
    /// in the IR; this merely permits consumers of `exact_additions` to use the
    /// relation when the wrap is unreachable.
    fn proven_nonwrapping_size_add(&self, left: ValueId, right: ValueId) -> bool {
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

    fn allocation_fits(&self, capacity: ValueId, base: u32, stride: u32) -> bool {
        if stride == 0 {
            return false;
        }
        let maximum = (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride);
        self.state.constants
            .get(&capacity)
            .is_some_and(|capacity| *capacity <= maximum)
            || self.state.guards.iter().any(|(operator, left, right)| {
                *operator == ComparisonOperator::LessEqualUnsigned
                    && *left == capacity
                    && self.state.constants.get(right).copied() == Some(maximum)
            })
    }

    fn constant_relation(
        &self,
        left: ValueId,
        right: ValueId,
        predicate: impl FnOnce(u64, u64) -> bool,
    ) -> bool {
        self.state.constants
            .get(&left)
            .copied()
            .zip(self.state.constants.get(&right).copied())
            .is_some_and(|(left, right)| predicate(left, right))
    }

    fn descriptor(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<&LayoutDescriptor, InitialCheckError> {
        self.layouts
            .get(layout)
            .ok_or(InitialCheckError::UnknownLayout(layout))
    }

    fn value_type_for_layout(
        &self,
        layout: crate::layout::LayoutId,
    ) -> Result<ValueType, InitialCheckError> {
        match self.descriptor(layout)?.kind() {
            LayoutKind::Scalar(kind) => self.value_type_for_scalar(*kind),
            LayoutKind::RuntimePointerValueSlot {
                pointee,
                semantics: PointerSlot::Owned,
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
    fn owned_pointer_slot_pointee(
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

    fn value_type_for_field(&self, field: FieldKind) -> Result<ValueType, InitialCheckError> {
        match field {
            FieldKind::Scalar(kind) => self.value_type_for_scalar(kind),
            FieldKind::Inline(layout) => self.value_type_for_layout(layout),
            FieldKind::Pointer { pointee, .. } => Ok(ValueType::Ref(pointee)),
        }
    }

    fn value_type_for_scalar(&self, kind: ScalarKind) -> Result<ValueType, InitialCheckError> {
        match kind {
            ScalarKind::Bool => Ok(ValueType::Bool),
            ScalarKind::Int | ScalarKind::Duration => Ok(ValueType::Int),
            ScalarKind::Float => Ok(ValueType::Float),
            ScalarKind::U32 | ScalarKind::Tag8 | ScalarKind::Tag16 | ScalarKind::Tag32 => {
                Ok(ValueType::Size)
            }
            ScalarKind::V128 => Ok(ValueType::V128),
            ScalarKind::RawByte | ScalarKind::BoolValueSlot => self.unsupported("layout scalar"),
        }
    }

    fn require_ref_type(
        &self,
        value: ValueId,
        layout: crate::layout::LayoutId,
    ) -> Result<(), InitialCheckError> {
        self.require_type(value, ValueType::Ref(layout))
    }

    fn require_type(&self, value: ValueId, expected: ValueType) -> Result<(), InitialCheckError> {
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

    fn invalid_projection<T>(&self, object: ValueId) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::InvalidProjection {
            function: self.function.id,
            object,
        })
    }

    fn unsupported<T>(&self, operation: &'static str) -> Result<T, InitialCheckError> {
        Err(InitialCheckError::UnsupportedMemoryOperation {
            function: self.function.id,
            operation,
        })
    }
}

fn drop_ref_layout_supported(
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    visiting: &mut BTreeSet<crate::layout::LayoutId>,
) -> bool {
    if !visiting.insert(layout) {
        return false;
    }
    let supported = layouts.get(layout).is_some_and(|descriptor| match descriptor.kind() {
        LayoutKind::Scalar(_) => true,
        LayoutKind::PackedList { element, .. } | LayoutKind::RuntimeBuffer { element } => {
            layouts.get(*element).is_some_and(|element| match element.kind() {
                LayoutKind::Scalar(_) => true,
                LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned } => {
                    drop_ref_layout_supported(layouts, *pointee, visiting)
                }
                _ => false,
            })
        }
        _ => false,
    });
    visiting.remove(&layout);
    supported
}

fn remap_value(renames: &BTreeMap<ValueId, ValueId>, value: ValueId) -> ValueId {
    renames.get(&value).copied().unwrap_or(value)
}

fn remap_extent(renames: &BTreeMap<ValueId, ValueId>, extent: &Extent) -> Extent {
    match extent {
        Extent::FixedBytes(bytes) => Extent::FixedBytes(*bytes),
        Extent::HeaderPrefix { minimum_bytes } => Extent::HeaderPrefix {
            minimum_bytes: *minimum_bytes,
        },
        Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        } => Extent::Counted {
            header_bytes: *header_bytes,
            count: remap_value(renames, *count),
            capacity: remap_value(renames, *capacity),
            stride_bytes: *stride_bytes,
        },
    }
}

fn intersect_map<K: Ord + Copy, V: Clone + PartialEq>(
    left: &BTreeMap<K, V>,
    right: &BTreeMap<K, V>,
) -> BTreeMap<K, V> {
    left
        .iter()
        .filter_map(|(key, value)| (right.get(key) == Some(value)).then(|| (*key, value.clone())))
        .collect()
}

fn meet_shared_phi_origins(
    left: &MemoryState,
    right: &MemoryState,
) -> BTreeMap<ValueId, BTreeSet<ValueId>> {
    left.shared_phi_origins
        .iter()
        .filter_map(|(value, left_origins)| {
            let right_origins = right.shared_phi_origins.get(value)?;
            let mut origins = left_origins.clone();
            origins.extend(right_origins);
            Some((*value, origins))
        })
        .collect()
}

/// Meet initialized aggregate prefixes across CFG edges. An empty prefix has
/// no elements and therefore places no constraint on their allocation class;
/// every other ownership and initialization fact must agree exactly.
fn meet_range_states(
    existing: &MemoryState,
    incoming: &MemoryState,
) -> BTreeMap<RangeIdentity, RangeState> {
    existing
        .range_states
        .iter()
        .filter_map(|(identity, current)| {
            let mut candidates = incoming.range_states.iter().filter(|(candidate, next)| {
                candidate.object == identity.object
                    && next.element == current.element
                    && (current.zero_width
                        || next.zero_width
                        || (same_scalar_across(
                            existing,
                            identity.start,
                            incoming,
                            candidate.start,
                        ) && same_scalar_across(
                            existing,
                            identity.length,
                            incoming,
                            candidate.length,
                        )))
            });
            let (next_identity, next) = candidates.next()?;
            if candidates.next().is_some() {
                return None;
            }
            if current.zero_width && !next.zero_width {
                return next.initialized.then_some((*next_identity, *next));
            }
            if next.zero_width && !current.zero_width {
                return current.initialized.then_some((*identity, *current));
            }
            if current.zero_width && next.zero_width {
                return Some((
                    *identity,
                    RangeState {
                        element: current.element,
                        ownership: Ownership::Unique,
                        allocation_class: None,
                        initialized: true,
                        zero_width: true,
                        zero_start: current.zero_start && next.zero_start,
                    },
                ));
            }
            if current == next {
                return Some((*identity, *current));
            }
            if current.element != next.element
                || current.initialized != next.initialized
                || !current.initialized
                || current.zero_start != next.zero_start
            {
                return None;
            }
            let ownership = match (current.ownership, next.ownership) {
                (Ownership::Unique, Ownership::Unique) => Ownership::Unique,
                (Ownership::Unique | Ownership::Shared, Ownership::Shared)
                | (Ownership::Shared, Ownership::Unique) => Ownership::Shared,
                _ => return None,
            };
            let allocation_class =
                (current.allocation_class == next.allocation_class)
                    .then_some(current.allocation_class)
                    .flatten();
            Some((
                *identity,
                RangeState { ownership, allocation_class, ..*current },
            ))
        })
        .collect()
}

fn same_scalar_across(
    left: &MemoryState,
    left_value: ValueId,
    right: &MemoryState,
    right_value: ValueId,
) -> bool {
    left_value == right_value
        || left
            .constants
            .get(&left_value)
            .zip(right.constants.get(&right_value))
            .is_some_and(|(left, right)| left == right)
}

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::CopyBytes { .. } => "CopyBytes",
        Operation::MoveRange { .. } => "MoveRange",
        Operation::CloneRange { .. } => "CloneRange",
        Operation::CloneRef { .. } => "CloneRef",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        Operation::DropRef { .. } => "DropRef",
        _ => "memory operation",
    }
}

fn returned_extent_matches(actual: &Extent, expected: &ReturnedExtent) -> bool {
    match (actual, expected) {
        (Extent::FixedBytes(actual), ReturnedExtent::FixedBytes(expected)) => actual == expected,
        (
            Extent::Counted { header_bytes: actual_header, stride_bytes: actual_stride, .. },
            ReturnedExtent::Counted { header_bytes: expected_header, stride_bytes: expected_stride },
        ) => actual_header == expected_header && actual_stride == expected_stride,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const COUNT: ValueId = ValueId::new(0);
    const CAPACITY: ValueId = ValueId::new(1);
    const START: ValueId = ValueId::new(2);
    const LENGTH: ValueId = ValueId::new(3);
    const LONG_LENGTH: ValueId = ValueId::new(4);
    const SOURCE: ValueId = ValueId::new(5);
    const DESTINATION: ValueId = ValueId::new(6);
    const END: ValueId = ValueId::new(7);
    const CHILD: ValueId = ValueId::new(8);
    const LOADED: ValueId = ValueId::new(9);
    const CONDITION: ValueId = ValueId::new(10);
    const ALIAS_START: ValueId = ValueId::new(11);
    const ALIAS_LENGTH: ValueId = ValueId::new(12);
    const ZERO: ValueId = ValueId::new(13);

    fn with_checker(
        pointer_element: bool,
        test: impl FnOnce(&mut MemoryChecker<'_>, crate::layout::LayoutId),
    ) {
        let mut layouts = LayoutInterner::new();
        let element = layouts
            .intern_scalar(ScalarKind::U32)
            .expect("scalar layout");
        let list = if pointer_element {
            let pointee = layouts
                .intern_scalar(ScalarKind::Int)
                .expect("pointee layout");
            let pointer = layouts
                .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
                .expect("pointer slot layout");
            layouts
                .intern_packed_list(pointer, RcHeader::Required)
                .expect("pointer list layout")
        } else {
            layouts
                .intern_runtime_buffer(element)
                .expect("scalar buffer layout")
        };
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [
                (COUNT, ValueType::Size),
                (CAPACITY, ValueType::Size),
                (START, ValueType::Size),
                (LENGTH, ValueType::Size),
                (LONG_LENGTH, ValueType::Size),
                (END, ValueType::Size),
                (SOURCE, ValueType::Ref(list)),
                (DESTINATION, ValueType::Ref(list)),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let mut checker = MemoryChecker::new(&function, &layouts, &signature)
            .expect("checker construction");
        for (destination, value) in [(COUNT, 1), (CAPACITY, 1), (START, 0), (LENGTH, 1), (LONG_LENGTH, 2)] {
            checker
                .check(&Operation::ConstSize { destination, value })
                .expect("constant is accepted");
        }
        let LayoutSize::Dynamic { base, stride } = layouts.get(list).expect("list descriptor").size() else {
            panic!("range test requires a dynamic layout");
        };
        for object in [SOURCE, DESTINATION] {
            checker
                .check(&Operation::Alloc {
                    destination: object,
                    layout: list,
                    extent: Extent::Counted {
                        header_bytes: base,
                        count: COUNT,
                        capacity: CAPACITY,
                        stride_bytes: stride,
                    },
                    class: AllocationClass::ReferenceCounted,
                })
                .expect("allocation is accepted");
        }
        test(&mut checker, list);
    }

    fn range_place(object: ValueId, list: crate::layout::LayoutId, length: ValueId, access: AccessMode) -> Place {
        Place::projected(
            object,
            Projection::Slice { start: START, length },
            ValueType::Ref(list),
            access,
        )
    }

    fn unique_phi_target(value: ValueId, layout: crate::layout::LayoutId) -> BasicBlock {
        BasicBlock {
            id: crate::canonical_safewir::BlockId::new(1),
            parameters: vec![crate::canonical_safewir::BlockParameter {
                value,
                value_type: ValueType::Ref(layout),
                ownership: Some(Ownership::Unique),
            }],
            operations: Vec::new(),
            terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
        }
    }

    #[test]
    fn unique_phi_rebases_distinct_fresh_identities_before_join() {
        with_checker(false, |checker, list| {
            let phi = ValueId::new(99);
            let target = unique_phi_target(phi, list);
            // Each incoming path allocates only its own forwarded object.
            let initial = checker.snapshot();
            checker.state.objects.remove(&DESTINATION);
            let left = checker.transfer_edge(&target, &[SOURCE]).expect("left unique phi");
            checker.restore(initial);
            checker.state.objects.remove(&SOURCE);
            let right = checker.transfer_edge(&target, &[DESTINATION]).expect("right unique phi");

            let joined = checker.meet(&target, &left, &right).expect("fresh identities join through phi");
            assert_eq!(joined.objects[&phi].identity, phi);
            assert_eq!(joined.objects[&phi].ownership, Ownership::Unique);
        });
    }

    #[test]
    fn unique_phi_rejects_stored_alias() {
        with_checker(false, |checker, list| {
            let phi = ValueId::new(99);
            let target = unique_phi_target(phi, list);
            let alias = checker.state.objects[&SOURCE].clone();
            checker
                .state
                .stored_objects
                .insert((DESTINATION, SlotIdentity::Constant(0)), alias);

            assert!(matches!(
                checker.transfer_edge(&target, &[SOURCE]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn move_value_rejects_shared_source() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.state.objects.get_mut(&SOURCE).expect("source").ownership = Ownership::Shared;

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn clone_ref_tracks_two_affine_shared_tokens() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.clone_ref(DESTINATION, SOURCE).expect("clone reference");

            let source = &checker.state.objects[&SOURCE];
            let destination = &checker.state.objects[&DESTINATION];
            assert_eq!(source.identity, destination.identity);
            assert_eq!(source.ownership, Ownership::Shared);
            assert_eq!(destination.ownership, Ownership::Shared);

            checker.drop_ref(SOURCE).expect("drop source token");
            checker.drop_ref(DESTINATION).expect("drop destination token");
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
            assert_eq!(checker.state.objects[&DESTINATION].ownership, Ownership::Consumed);
        });
    }

    #[test]
    fn clone_ref_rejects_storage_without_refcount_authority() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            checker.state.objects.get_mut(&SOURCE).expect("source").allocation_class = None;

            assert!(matches!(
                checker.clone_ref(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn shared_static_storage_clones_and_drops_without_mutation_authority() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = false;
            checker.state.static_data_objects.insert(SOURCE);

            checker.clone_ref(DESTINATION, SOURCE).expect("static clone is a no-op token split");
            assert!(checker.state.static_data_objects.contains(&DESTINATION));
            assert!(matches!(
                checker.reclaim(SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            assert!(matches!(
                checker.update_count(SOURCE, LENGTH),
                Err(InitialCheckError::WritableStaticData { object: SOURCE, .. })
            ));
            checker.drop_ref(SOURCE).expect("static drop is a no-op token consume");
            checker.drop_ref(DESTINATION).expect("static clone drop is a no-op token consume");
        });
    }

    #[test]
    fn shared_boundary_union_allows_only_immutable_token_operations() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;
            source.shared_immutable = true;

            checker.clone_ref(DESTINATION, SOURCE).expect("RC-or-static union clones safely");
            assert!(matches!(
                checker.reclaim(SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.drop_ref(SOURCE).expect("union drop dispatches safely at runtime");
            checker.drop_ref(DESTINATION).expect("union clone drop dispatches safely at runtime");
        });
    }

    #[test]
    fn release_rejects_shared_value_without_refcount_authority() {
        with_checker(false, |checker, _list| {
            let source = checker.state.objects.get_mut(&SOURCE).expect("source");
            source.ownership = Ownership::Shared;
            source.allocation_class = None;

            assert!(matches!(
                checker.release(SOURCE),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn move_value_rejects_live_object_alias() {
        with_checker(false, |checker, _list| {
            let alias = checker.state.objects[&SOURCE].clone();
            checker.state.objects.insert(DESTINATION, alias);

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn move_value_rejects_stored_alias() {
        with_checker(false, |checker, _list| {
            checker.state.objects.remove(&DESTINATION);
            let alias = checker.state.objects[&SOURCE].clone();
            checker
                .state
                .stored_objects
                .insert((DESTINATION, SlotIdentity::Constant(0)), alias);

            assert!(matches!(
                checker.move_value(DESTINATION, SOURCE),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn return_contract_accepts_only_sole_unique_fresh_object() {
        with_checker(false, |checker, list| {
            let LayoutSize::Dynamic { base, stride } = checker.descriptor(list).expect("list layout").size() else { panic!("dynamic list") };
            let contract = ResultContract::Fresh { layout: list, class: AllocationClass::ReferenceCounted, extent: ReturnedExtent::Counted { header_bytes: base, stride_bytes: stride } };
            assert!(checker.check_return(&[SOURCE], std::slice::from_ref(&contract)).is_ok());

            let alias = checker.state.objects[&SOURCE].clone();
            checker.state.objects.insert(DESTINATION, alias);
            assert!(matches!(
                checker.check_return(&[SOURCE], &[contract]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn return_contract_rejects_fresh_allocation_class_mismatch() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check_return(
                    &[SOURCE],
                    &[ResultContract::Fresh {
                        layout: list,
                        class: AllocationClass::Bump,
                        extent: ReturnedExtent::Counted { header_bytes: 4, stride_bytes: 4 },
                    }],
                ),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn affine_boundary_rejects_duplicate_owned_return_lanes() {
        with_checker(false, |checker, list| {
            checker.state.objects.remove(&DESTINATION);
            let LayoutSize::Dynamic { base, stride } = checker.descriptor(list).unwrap().size() else {
                panic!("dynamic list");
            };
            let fresh = ResultContract::Fresh {
                layout: list,
                class: AllocationClass::ReferenceCounted,
                extent: ReturnedExtent::Counted { header_bytes: base, stride_bytes: stride },
            };
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[fresh.clone(), fresh]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.clone_ref(DESTINATION, SOURCE).unwrap();
            let shared = ResultContract::Shared { layout: list };
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[shared.clone(), shared.clone()]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
            checker.check_return(&[SOURCE, DESTINATION], &[shared.clone(), shared]).unwrap();
        });
    }

    #[test]
    fn affine_boundary_rejects_invented_call_storage_class() {
        with_checker(false, |checker, _list| {
            for class in [None, Some(AllocationClass::Bump), Some(AllocationClass::Scratch)] {
                checker.state.objects.get_mut(&SOURCE).unwrap().allocation_class = class;
                for ownership in [ParameterOwnership::Unique, ParameterOwnership::Shared] {
                    assert!(matches!(
                        checker.consume_call_move(SOURCE, ownership),
                        Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
                    ));
                }
            }
            checker.state.objects.get_mut(&SOURCE).unwrap().allocation_class =
                Some(AllocationClass::ReferenceCounted);
            checker.consume_call_move(SOURCE, ParameterOwnership::Shared).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Consumed);
        });
    }

    #[test]
    fn affine_boundary_rejects_invented_shared_result_class() {
        with_checker(false, |checker, list| {
            let source = checker.state.objects.get_mut(&SOURCE).unwrap();
            source.ownership = Ownership::Shared;
            source.allocation_class = Some(AllocationClass::Bump);
            assert!(matches!(
                checker.check_return(&[SOURCE], &[ResultContract::Shared { layout: list }]),
                Err(InitialCheckError::InvalidAllocationClass { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn owned_result_contract_accepts_unique_rc_without_inventing_a_clone() {
        with_checker(false, |checker, list| {
            checker.drop_ref(DESTINATION).unwrap();
            checker.check_return(&[SOURCE], &[ResultContract::Shared { layout: list }]).unwrap();
            checker.check_exit(&[SOURCE]).unwrap();
            assert_eq!(checker.state.objects[&SOURCE].ownership, Ownership::Unique);
            assert!(matches!(
                checker.check_return(&[SOURCE, SOURCE], &[
                    ResultContract::Shared { layout: list },
                    ResultContract::Shared { layout: list },
                ]),
                Err(InitialCheckError::InvalidOwnership { object: SOURCE, .. })
            ));
        });
    }

    #[test]
    fn affine_boundary_join_preserves_release_obligations_on_both_edges() {
        with_checker(false, |checker, _list| {
            let block = &checker.function.blocks[0];
            let left = checker.snapshot();
            let mut right = left.clone();
            right.objects.remove(&SOURCE);
            for (a, b) in [(&left, &right), (&right, &left)] {
                assert!(matches!(
                    checker.meet(block, a, b),
                    Err(InitialCheckError::IncompatibleMemoryJoin { object: SOURCE, .. })
                ));
            }
            checker.drop_ref(SOURCE).unwrap();
            let consumed = checker.snapshot();
            checker.meet(block, &consumed, &right).unwrap();
        });
    }

    #[test]
    fn affine_boundary_join_does_not_forget_owned_heap_slots() {
        with_checker(false, |checker, _list| {
            let block = &checker.function.blocks[0];
            let left = checker.snapshot();
            let mut right = left.clone();
            right.stored_objects.insert(
                (DESTINATION, SlotIdentity::Constant(0)),
                right.objects[&SOURCE].clone(),
            );
            for (a, b) in [(&left, &right), (&right, &left)] {
                assert!(matches!(
                    checker.meet(block, a, b),
                    Err(InitialCheckError::IncompatibleMemoryJoin { object: DESTINATION, .. })
                ));
            }
        });
    }

    #[test]
    fn affine_boundary_tail_call_requires_no_untransferred_tokens() {
        with_checker(false, |checker, _list| {
            use crate::canonical_safewir::{TailSiteId, Terminator};
            let function = checker.function;
            let signature = FunctionSignature {
                id: function.signature,
                parameters: Vec::new(),
                results: Vec::new(),
                contract: CallableContract {
                    parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new(),
                },
            };
            let terminator = Terminator::TailCall {
                site: TailSiteId::from_digest([0; 32]),
                callee: function.id,
                arguments: Vec::new(),
            };
            let signatures = BTreeMap::from([(signature.id, &signature)]);
            let functions = BTreeMap::from([(function.id, function)]);
            assert!(matches!(
                super::super::check_terminator(
                    function, &terminator, &signature, &signatures, &functions, &BTreeMap::new(), checker,
                ),
                Err(InitialCheckError::UnconsumedReference { .. })
            ));
            checker.drop_ref(SOURCE).unwrap();
            checker.drop_ref(DESTINATION).unwrap();
            super::super::check_terminator(
                function, &terminator, &signature, &signatures, &functions, &BTreeMap::new(), checker,
            ).unwrap();
        });
    }



    #[test]
    fn guarded_wrapping_size_add_records_an_exact_relation() {
        with_checker(false, |checker, _list| {
            checker.state.constants.remove(&START);
            checker.state.guards.insert((
                ComparisonOperator::LessUnsigned,
                START,
                CAPACITY,
            ));
            checker
                .check(&Operation::Arithmetic {
                    destination: END,
                    operator: ArithmeticOperator::Add,
                    left: START,
                    right: LENGTH,
                    overflow: OverflowBehavior::Wrap,
                })
                .expect("guarded wrapping addition is checked");
            assert_eq!(checker.state.exact_additions.get(&END), Some(&(START, LENGTH)));
        });
    }

    #[test]
    fn unchecked_wrapping_size_add_does_not_record_an_exact_relation() {
        with_checker(false, |checker, _list| {
            checker.state.constants.remove(&START);
            checker
                .check(&Operation::Arithmetic {
                    destination: END,
                    operator: ArithmeticOperator::Add,
                    left: START,
                    right: LENGTH,
                    overflow: OverflowBehavior::Wrap,
                })
                .expect("unchecked wrapping addition remains representable");
            assert!(!checker.state.exact_additions.contains_key(&END));
        });
    }

    fn with_pointer_slot_checker(
        test: impl FnOnce(
            &mut MemoryChecker<'_>,
            crate::layout::LayoutId,
            crate::layout::LayoutId,
        ),
    ) {
        let mut layouts = LayoutInterner::new();
        let pointee = layouts.intern_scalar(ScalarKind::Int).expect("pointee layout");
        let pointer = layouts
            .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
            .expect("owned pointer slot");
        let list = layouts
            .intern_packed_list(pointer, RcHeader::Required)
            .expect("pointer list layout");
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [
                (COUNT, ValueType::Size),
                (CAPACITY, ValueType::Size),
                (START, ValueType::Size),
                (LENGTH, ValueType::Size),
                (CHILD, ValueType::Ref(pointee)),
                (LOADED, ValueType::Ref(pointee)),
                (SOURCE, ValueType::Ref(list)),
                (DESTINATION, ValueType::Ref(list)),
                (ALIAS_START, ValueType::Size),
                (ALIAS_LENGTH, ValueType::Size),
                (ZERO, ValueType::Size),
                (CONDITION, ValueType::Bool),
            ]
            .into_iter()
            .collect(),
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let mut checker = MemoryChecker::new(&function, &layouts, &signature)
            .expect("checker construction");
        for (destination, value) in [
            (COUNT, 1),
            (CAPACITY, 1),
            (START, 0),
            (LENGTH, 1),
            (ALIAS_START, 0),
            (ALIAS_LENGTH, 1),
            (ZERO, 0),
        ] {
            checker
                .check(&Operation::ConstSize { destination, value })
                .expect("constant is accepted");
        }
        checker
            .check(&Operation::Compare {
                destination: CONDITION,
                operator: ComparisonOperator::LessUnsigned,
                left: START,
                right: COUNT,
            })
            .expect("comparison is accepted");
        checker
            .check(&Operation::Guard {
                condition: CONDITION,
                kind: GuardKind::Bounds,
            })
            .expect("bounds guard is accepted");
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessUnsigned, START, CAPACITY));
        let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else {
            panic!("pointer list must be dynamic");
        };
        checker
            .check(&Operation::Alloc {
                destination: SOURCE,
                layout: list,
                extent: Extent::Counted {
                    header_bytes: base,
                    count: COUNT,
                    capacity: CAPACITY,
                    stride_bytes: stride,
                },
                class: AllocationClass::ReferenceCounted,
            })
            .expect("list allocation is accepted");
        let LayoutSize::Fixed(bytes) = layouts.get(pointee).unwrap().size() else {
            panic!("pointee must be fixed");
        };
        checker
            .check(&Operation::Alloc {
                destination: CHILD,
                layout: pointee,
                extent: Extent::FixedBytes(u64::from(bytes)),
                class: AllocationClass::ReferenceCounted,
            })
            .expect("child allocation is accepted");
        checker
            .check(&Operation::Alloc {
                destination: DESTINATION,
                layout: list,
                extent: Extent::Counted {
                    header_bytes: base,
                    count: COUNT,
                    capacity: CAPACITY,
                    stride_bytes: stride,
                },
                class: AllocationClass::ReferenceCounted,
            })
            .expect("destination allocation is accepted");
        test(&mut checker, list, pointee);
    }

    #[test]
    fn scalar_move_range_is_bounds_checked_and_admitted() {
        with_checker(false, |checker, list| {
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Ok(true)
            );
        });
    }

    #[test]
    fn scalar_clone_range_is_ownership_safe_and_admitted() {
        with_checker(false, |checker, list| {
            assert_eq!(
                checker.check(&Operation::CloneRange {
                    destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Ok(true)
            );
        });
    }

    #[test]
    fn range_transfer_rejects_a_slice_past_logical_length() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check(&Operation::CloneRange {
                    destination: range_place(DESTINATION, list, LONG_LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LONG_LENGTH, AccessMode::Read),
                    count: LONG_LENGTH,
                }),
                Err(InitialCheckError::MissingBoundsGuard { .. })
            ));
        });
    }

    #[test]
    fn range_transfer_rejects_same_storage_without_overlap_policy() {
        with_checker(false, |checker, list| {
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: range_place(SOURCE, list, LENGTH, AccessMode::Write),
                    source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                    count: LENGTH,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(overlap)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_bearing_ranges_fail_closed_without_element_transfer_state() {
        with_checker(true, |checker, list| {
            let result = checker.check(&Operation::CloneRange {
                destination: range_place(DESTINATION, list, LENGTH, AccessMode::Write),
                source: range_place(SOURCE, list, LENGTH, AccessMode::Read),
                count: LENGTH,
            });
            assert!(matches!(
                result,
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "CloneRange(pointer-bearing element)",
                    ..
                })
            ));
        });
    }

    fn pointer_range_place(object: ValueId, list: crate::layout::LayoutId, count: ValueId, access: AccessMode) -> Place {
        Place::projected(
            object,
            Projection::Slice { start: START, length: count },
            ValueType::Ref(list),
            access,
        )
    }

    #[test]
    fn dynamic_pointer_move_range_transfers_initialized_ownership_once() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            checker.state.constants.remove(&COUNT);
            checker.state.guards.insert((ComparisonOperator::LessEqualUnsigned, COUNT, CAPACITY));
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Ok(true)
            );
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: DESTINATION, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true }
            );
            assert_eq!(
                checker.state.range_states[&RangeIdentity { object: SOURCE, start: START, length: COUNT }],
                RangeState { element: pointer_element, ownership: Ownership::Consumed, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: false, zero_width: false, zero_start: true }
            );
        });
    }

    #[test]
    fn pointer_move_range_rejects_uninitialized_source() {
        with_pointer_slot_checker(|checker, list, _pointee| {
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_source_reuse_after_consumption() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            let operation = Operation::MoveRange {
                destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                count: COUNT,
            };
            assert_eq!(checker.check(&operation), Ok(true));
            assert!(matches!(
                checker.check(&operation),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(uninitialized-source)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_overlapping_storage_before_transfer() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(SOURCE, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(overlap)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_initialized_destination() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: COUNT },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            assert!(matches!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, list, COUNT, AccessMode::Write),
                    source: pointer_range_place(SOURCE, list, COUNT, AccessMode::Read),
                    count: COUNT,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "MoveRange(destination-initialized)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_move_range_rejects_reload_through_old_equal_constant_identity() {
        with_pointer_slot_checker(|checker, list, pointee| {
            let pointer_element = checker.descriptor(list).unwrap().kind().packed_element_layout().unwrap();
            checker.state.range_states.insert(
                RangeIdentity { object: SOURCE, start: START, length: LENGTH },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: Some(AllocationClass::ReferenceCounted), initialized: true, zero_width: false, zero_start: true },
            );
            checker.state.range_states.insert(
                RangeIdentity { object: DESTINATION, start: START, length: ZERO },
                RangeState { element: pointer_element, ownership: Ownership::Unique, allocation_class: None, initialized: true, zero_width: true, zero_start: true },
            );
            assert_eq!(
                checker.check(&Operation::MoveRange {
                    destination: pointer_range_place(DESTINATION, list, ALIAS_LENGTH, AccessMode::Write),
                    source: Place::projected(
                        SOURCE,
                        Projection::Slice { start: ALIAS_START, length: ALIAS_LENGTH },
                        ValueType::Ref(list),
                        AccessMode::Read,
                    ),
                    count: ALIAS_LENGTH,
                }),
                Ok(true)
            );
            assert!(matches!(
                checker.check(&Operation::Load {
                    destination: LOADED,
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        ValueType::Ref(pointee),
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CloneShared,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Load(uninitialized reference)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn pointer_slot_move_and_clone_preserve_structural_ownership() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            let place = Place::projected(
                SOURCE,
                Projection::Element { index: START },
                ValueType::Ref(pointee),
                AccessMode::Write,
            );
            assert_eq!(
                checker.check(&Operation::Store {
                    place,
                    value: CHILD,
                    transfer: StoreTransfer::MoveUnique,
                }),
                Ok(true)
            );
            assert_eq!(checker.state.objects[&CHILD].ownership, Ownership::Consumed);

            assert_eq!(
                checker.check(&Operation::Load {
                    destination: LOADED,
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        ValueType::Ref(pointee),
                        AccessMode::Read,
                    ),
                    transfer: LoadTransfer::CloneShared,
                }),
                Ok(true)
            );
            assert_eq!(checker.state.objects[&LOADED].ownership, Ownership::Shared);
            assert_eq!(
                checker.state.stored_objects[&(SOURCE, SlotIdentity::Constant(0))].ownership,
                Ownership::Shared
            );
        });
    }

    #[test]
    fn pointer_slot_rejects_scalar_copy_transfer() {
        with_pointer_slot_checker(|checker, _list, pointee| {
            assert!(matches!(
                checker.check(&Operation::Store {
                    place: Place::projected(
                        SOURCE,
                        Projection::Element { index: START },
                        ValueType::Ref(pointee),
                        AccessMode::Write,
                    ),
                    value: CHILD,
                    transfer: StoreTransfer::CopyScalar,
                }),
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    operation: "Store(reference copy)",
                    ..
                })
            ));
        });
    }

    #[test]
    fn byte_copy_requires_a_dominating_dynamic_end_guard() {
        let mut layouts = LayoutInterner::new();
        let byte = layouts
            .intern_scalar(ScalarKind::RawByte)
            .expect("raw-byte layout");
        let buffer = layouts.intern_runtime_buffer(byte).expect("byte buffer layout");
        let value_types = [
            (COUNT, ValueType::Size),
            (CAPACITY, ValueType::Size),
            (START, ValueType::Size),
            (LENGTH, ValueType::Size),
            (END, ValueType::Size),
            (SOURCE, ValueType::Ref(buffer)),
            (DESTINATION, ValueType::Ref(buffer)),
        ]
        .into_iter()
        .collect();
        let function = Function {
            id: crate::canonical_safewir::FunctionId::new(0),
            signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types,
            blocks: vec![BasicBlock {
                id: crate::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: Vec::new(),
                terminator: crate::canonical_safewir::Terminator::Return { values: Vec::new() },
            }],
        };
        let signature = FunctionSignature {
            id: function.signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract {
                parameters: Vec::new(),
                results: Vec::new(),
                parameter_effects: BTreeMap::new(),
            },
        };
        let mut checker = MemoryChecker::new(&function, &layouts, &signature)
            .expect("checker construction");
        for object in [SOURCE, DESTINATION] {
            checker.state.objects.insert(
                object,
                ObjectState {
                    identity: object,
                    layout: buffer,
                    extent: Extent::Counted {
                        header_bytes: 4,
                        count: COUNT,
                        capacity: CAPACITY,
                        stride_bytes: 1,
                    },
                    ownership: Ownership::Unique,
                    allocation_class: Some(AllocationClass::ReferenceCounted),
                    shared_immutable: false,
                    initialized_owned_slots: false,
                },
            );
        }
        checker.state.exact_additions.insert(END, (START, LENGTH));
        let operation = Operation::CopyBytes {
            destination: Place::projected(
                DESTINATION,
                Projection::Slice { start: START, length: LENGTH },
                ValueType::Ref(buffer),
                AccessMode::Write,
            ),
            source: Place::projected(
                SOURCE,
                Projection::Slice { start: START, length: LENGTH },
                ValueType::Ref(buffer),
                AccessMode::Read,
            ),
            length: LENGTH,
        };

        assert!(matches!(
            checker.check(&operation),
            Err(InitialCheckError::MissingBoundsGuard { index: START, count: COUNT, .. })
        ));
        checker
            .state
            .guards
            .insert((ComparisonOperator::LessEqualUnsigned, END, COUNT));
        assert_eq!(checker.check(&operation), Ok(true));
    }
}
