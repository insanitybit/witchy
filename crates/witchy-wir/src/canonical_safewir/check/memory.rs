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
    ParameterOwnership, Place, Projection, ResultContract, StoreTransfer, ValueId, ValueType,
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
}

#[derive(Debug, Clone, Copy)]
struct CheckedRange {
    object: ValueId,
    length: ValueId,
    layout: crate::layout::LayoutId,
    element: crate::layout::LayoutId,
    identity: ValueId,
    ownership: Ownership,
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
    stored_objects: BTreeMap<(ValueId, SlotIdentity), ObjectState>,
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
                stored_objects: BTreeMap::new(),
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

    /// Rename facts through an explicit SSA edge.  A block parameter is a new
    /// definition, so any count, capacity, header value, guard, or object state
    /// carried by an argument must be expressed in terms of that definition.
    pub(super) fn transfer_edge(
        &self,
        target: &BasicBlock,
        arguments: &[ValueId],
    ) -> Result<MemoryState, InitialCheckError> {
        let mut renames = BTreeMap::new();
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
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
            let mut state = state.clone();
            state.extent = remap_extent(&renames, &state.extent);
            let value = remap_value(&renames, *value);
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

        Ok(MemoryState {
            objects,
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
            stored_objects: intersect_map(&existing.stored_objects, &incoming.stored_objects),
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
            self.state.objects.insert(
                parameter.value,
                ObjectState {
                    identity: parameter.value,
                    layout,
                    extent,
                    ownership,
                    allocation_class: None,
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
            if !actual.is_memory_bearing() {
                if !matches!(argument, CallArgument::Value(_)) {
                    return self.unsupported("Call(non-memory ownership)");
                }
                continue;
            }
            match argument {
                CallArgument::Value(_) => return self.unsupported("Call(Value memory)"),
                CallArgument::Share(_) => return self.unsupported("Call(Share)"),
                CallArgument::Move(value) => {
                    if !matches!(parameter.ownership, ParameterOwnership::Unique) {
                        return Err(InitialCheckError::UnsupportedBorrowArgument {
                            function: self.function.id,
                        });
                    }
                    self.consume_call_move(*value)?;
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
        if results.iter().any(|result| {
            !matches!(result, ResultContract::Value(value) if !value.is_memory_bearing())
        }) || destinations.iter().any(|destination| {
            value_type(self.function, *destination)
                .map(|value_type| value_type.is_memory_bearing())
                .unwrap_or(true)
        }) {
            return self.unsupported("Call(memory result)");
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

        // A range move consumes the initialized elements. The v1 IR has no
        // per-element initialization map or clear operation, so admitting a
        // pointer-bearing element would leave physical owners duplicated or
        // uncleared. Scalar elements have no ownership state to transfer.
        self.require_scalar_range_element(destination.element, "MoveRange")?;
        if destination.ownership != Ownership::Unique
            || source.ownership != Ownership::Unique
            || self.state.static_data_objects.contains(&destination.object)
        {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: destination.object,
            });
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
        if !self.range_within(*start, *length, logical_count) {
            return Err(InitialCheckError::MissingBoundsGuard {
                function: self.function.id,
                index: *start,
                count: logical_count,
            });
        }
        let _ = operation;
        Ok(CheckedRange {
            object: place.object(),
            length: *length,
            layout: state.layout,
            element,
            identity: state.identity,
            ownership: state.ownership,
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

    fn consume_call_move(&mut self, source: ValueId) -> Result<(), InitialCheckError> {
        let state = self.live_object(source)?;
        if state.ownership != Ownership::Unique {
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
            Operation::Share { destination, source } => {
                self.share(*destination, *source)?;
                Ok(true)
            }
            Operation::Release { object } => {
                self.release(*object)?;
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
        self.state.objects.insert(
            destination,
            ObjectState {
                identity: destination,
                layout,
                extent: extent.clone(),
                ownership: Ownership::Unique,
                allocation_class: Some(class),
            },
        );
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
        let mut stored = self
            .state
            .stored_objects
            .get(&key)
            .cloned()
            .ok_or(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: "Load(uninitialized reference)",
            })?;
        match transfer {
            LoadTransfer::CloneShared => {
                if !matches!(stored.ownership, Ownership::Unique | Ownership::Shared)
                    || stored.allocation_class != Some(AllocationClass::ReferenceCounted)
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
        let mut stored = self.live_object(value)?.clone();
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
                    || stored.allocation_class != Some(AllocationClass::ReferenceCounted)
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
                let Extent::Counted { count, .. } = &object.extent else {
                    return self.invalid_projection(place.object());
                };
                let count = self
                    .state
                    .header_values
                    .get(&(place.object(), HeaderField::Length))
                    .unwrap_or(count);
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
        self.state.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        };
        self.state.header_values.insert((object, HeaderField::Length), count);
        Ok(())
    }

    fn move_value(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        self.require_type(destination, source_type)?;
        let state = self.live_object(source)?.clone();
        self.state.objects.insert(destination, state);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    fn share(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        let ValueType::Ref(layout) = source_type else {
            return self.unsupported("Share(scalar)");
        };
        self.require_ref_type(destination, layout)?;
        let mut state = self.live_object(source)?.clone();
        if state.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: source,
            });
        }
        state.ownership = Ownership::Shared;
        self.state.objects.insert(destination, state);
        self.state.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
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
            .contains(&(ComparisonOperator::LessUnsigned, left, right))
    }

    fn less_equal(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left <= right)
            || self
                .state.guards
                .contains(&(ComparisonOperator::LessEqualUnsigned, left, right))
            || left == right
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

fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::CopyBytes { .. } => "CopyBytes",
        Operation::MoveRange { .. } => "MoveRange",
        Operation::CloneRange { .. } => "CloneRange",
        Operation::Share { .. } => "Share",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        _ => "memory operation",
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
                (CHILD, ValueType::Ref(pointee)),
                (LOADED, ValueType::Ref(pointee)),
                (SOURCE, ValueType::Ref(list)),
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
        for (destination, value) in [(COUNT, 1), (CAPACITY, 1), (START, 0)] {
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
