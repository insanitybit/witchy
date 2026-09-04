use std::collections::{BTreeMap, BTreeSet};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutInterner, LayoutKind, LayoutSize, ScalarKind,
};

use super::{InitialCheckError, value_type};
use crate::canonical_safewir::{
    AccessMode, AllocationClass, BorrowMode, CallArgument, ComparisonOperator, Extent, Function,
    FunctionSignature, GuardKind, HeaderField, LoadTransfer, Operation, Ownership, ParameterContract,
    ParameterOwnership, Place, Projection, ResultContract, StoreTransfer, ValueId, ValueType,
};

#[derive(Debug, Clone)]
struct ObjectState {
    layout: crate::layout::LayoutId,
    extent: Extent,
    ownership: Ownership,
    allocation_class: Option<AllocationClass>,
}

#[derive(Debug, Clone, Copy)]
struct Comparison {
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
}

pub(super) struct MemoryChecker<'a> {
    function: &'a Function,
    layouts: &'a LayoutInterner,
    objects: BTreeMap<ValueId, ObjectState>,
    constants: BTreeMap<ValueId, u64>,
    comparisons: BTreeMap<ValueId, Comparison>,
    guards: BTreeSet<(ComparisonOperator, ValueId, ValueId)>,
    header_values: BTreeMap<(ValueId, HeaderField), ValueId>,
}

impl<'a> MemoryChecker<'a> {
    pub(super) fn new(
        function: &'a Function,
        layouts: &'a LayoutInterner,
        signature: &FunctionSignature,
    ) -> Result<Self, InitialCheckError> {
        let has_memory = function
            .value_types
            .values()
            .any(ValueType::is_memory_bearing)
            || function.blocks.iter().any(|block| {
                block.operations.iter().any(is_memory_operation)
            });
        if has_memory && function.blocks.len() != 1 {
            return Err(InitialCheckError::UnsupportedMemoryControlFlow {
                function: function.id,
            });
        }
        let mut checker = Self {
            function,
            layouts,
            objects: BTreeMap::new(),
            constants: BTreeMap::new(),
            comparisons: BTreeMap::new(),
            guards: BTreeSet::new(),
            header_values: BTreeMap::new(),
        };
        checker.initialize_parameters(signature)?;
        Ok(checker)
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
            self.objects.insert(
                parameter.value,
                ObjectState {
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
        self.objects
            .get_mut(&source)
            .expect("live object exists")
            .ownership = Ownership::Consumed;
        Ok(())
    }

    /// Returns true when this operation belongs to the memory judgment.
    pub(super) fn check(&mut self, operation: &Operation) -> Result<bool, InitialCheckError> {
        match operation {
            Operation::ConstSize { destination, value } => {
                self.constants.insert(*destination, *value);
                Ok(false)
            }
            Operation::Compare { destination, operator, left, right } => {
                self.comparisons.insert(
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
                let comparison = self.comparisons.get(condition).copied().ok_or(
                    InitialCheckError::InvalidGuard {
                        function: self.function.id,
                        condition: *condition,
                        kind: *kind,
                    },
                )?;
                if *kind == GuardKind::Bounds {
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
                    self.guards
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
            Operation::CopyBytes { .. }
            | Operation::MoveRange { .. }
            | Operation::CloneRange { .. }
            | Operation::Retain { .. }
                => Err(InitialCheckError::UnsupportedMemoryOperation {
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
        self.objects.insert(
            destination,
            ObjectState {
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
        self.objects.insert(
            destination,
            ObjectState {
                layout,
                extent: extent.clone(),
                ownership: Ownership::Shared,
                allocation_class: None,
            },
        );
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
        if place.access() != AccessMode::Read || transfer != LoadTransfer::CopyScalar {
            return self.unsupported("Load(non-scalar)");
        }
        let expected = self.check_place(place)?;
        if expected.is_memory_bearing() {
            return self.unsupported("Load(reference)");
        }
        self.require_type(destination, expected)?;
        if let Projection::Header(field) = place.projection() {
            self.header_values
                .insert((place.object(), *field), destination);
        }
        Ok(())
    }

    fn store(
        &self,
        place: &Place,
        value: ValueId,
        transfer: StoreTransfer,
    ) -> Result<(), InitialCheckError> {
        if place.access() != AccessMode::Write || transfer != StoreTransfer::CopyScalar {
            return self.unsupported("Store(non-scalar)");
        }
        if self.live_object(place.object())?.ownership != Ownership::Unique {
            return Err(InitialCheckError::InvalidOwnership {
                function: self.function.id,
                object: place.object(),
            });
        }
        let expected = self.check_place(place)?;
        if expected.is_memory_bearing() {
            return self.unsupported("Store(reference)");
        }
        self.require_type(value, expected)
    }

    fn check_place(&self, place: &Place) -> Result<ValueType, InitialCheckError> {
        let object = self.live_object(place.object())?;
        let descriptor = self.descriptor(object.layout)?;
        let derived = match place.projection() {
            Projection::Header(field) => {
                let HeaderLayout::PackedList {
                    length_offset,
                    capacity_offset,
                    data_offset,
                    ..
                } = descriptor.header()
                else {
                    return self.invalid_projection(place.object());
                };
                let offset = match field {
                    HeaderField::Length => length_offset,
                    HeaderField::Capacity => capacity_offset,
                };
                if offset >= data_offset {
                    return self.invalid_projection(place.object());
                }
                ValueType::Size
            }
            Projection::Element { index } => {
                let Extent::Counted { count, .. } = &object.extent else {
                    return self.invalid_projection(place.object());
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

    fn update_count(&mut self, object: ValueId, count: ValueId) -> Result<(), InitialCheckError> {
        self.require_type(count, ValueType::Size)?;
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
            .header_values
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
        self.objects.get_mut(&object).expect("live object exists").extent = Extent::Counted {
            header_bytes,
            count,
            capacity,
            stride_bytes,
        };
        self.header_values.insert((object, HeaderField::Length), count);
        Ok(())
    }

    fn move_value(&mut self, destination: ValueId, source: ValueId) -> Result<(), InitialCheckError> {
        let source_type = value_type(self.function, source)?;
        self.require_type(destination, source_type)?;
        let state = self.live_object(source)?.clone();
        self.objects.insert(destination, state);
        self.objects
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
        self.objects.insert(destination, state);
        self.objects
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
        self.objects
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
        self.objects.get_mut(&object).expect("live object exists").ownership = Ownership::Consumed;
        Ok(())
    }

    fn live_object(&self, object: ValueId) -> Result<&ObjectState, InitialCheckError> {
        let state = self.objects.get(&object).ok_or(InitialCheckError::InvalidOwnership {
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
        self.guards
            .contains(&(ComparisonOperator::LessUnsigned, left, right))
    }

    fn less_equal(&self, left: ValueId, right: ValueId) -> bool {
        self.constant_relation(left, right, |left, right| left <= right)
            || self
                .guards
                .contains(&(ComparisonOperator::LessEqualUnsigned, left, right))
            || left == right
    }

    fn allocation_fits(&self, capacity: ValueId, base: u32, stride: u32) -> bool {
        if stride == 0 {
            return false;
        }
        let maximum = (u64::from(u32::MAX) - u64::from(base)) / u64::from(stride);
        self.constants
            .get(&capacity)
            .is_some_and(|capacity| *capacity <= maximum)
            || self.guards.iter().any(|(operator, left, right)| {
                *operator == ComparisonOperator::LessEqualUnsigned
                    && *left == capacity
                    && self.constants.get(right).copied() == Some(maximum)
            })
    }

    fn constant_relation(
        &self,
        left: ValueId,
        right: ValueId,
        predicate: impl FnOnce(u64, u64) -> bool,
    ) -> bool {
        self.constants
            .get(&left)
            .copied()
            .zip(self.constants.get(&right).copied())
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

fn is_memory_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Alloc { .. }
            | Operation::StaticData { .. }
            | Operation::Load { .. }
            | Operation::Store { .. }
            | Operation::CopyBytes { .. }
            | Operation::MoveRange { .. }
            | Operation::CloneRange { .. }
            | Operation::UpdateCount { .. }
            | Operation::Share { .. }
            | Operation::Retain { .. }
            | Operation::Release { .. }
            | Operation::Reclaim { .. }
    )
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
