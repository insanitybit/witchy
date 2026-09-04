use std::collections::{BTreeMap, BTreeSet};

use crate::layout::{
    FieldKind, HeaderLayout, LayoutDescriptor, LayoutInterner, LayoutKind, LayoutSize, ScalarKind,
};

use super::{InitialCheckError, value_type};
use crate::canonical_safewir::{
    AccessMode, AllocationClass, ComparisonOperator, Extent, Function, HeaderField, LoadTransfer,
    Operation, Ownership, Place, Projection, StoreTransfer, ValueId, ValueType,
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
}

impl<'a> MemoryChecker<'a> {
    pub(super) fn new(
        function: &'a Function,
        layouts: &'a LayoutInterner,
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
        Ok(Self {
            function,
            layouts,
            objects: BTreeMap::new(),
            constants: BTreeMap::new(),
            comparisons: BTreeMap::new(),
            guards: BTreeSet::new(),
        })
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
            Operation::Guard { condition, .. } => {
                if let Some(comparison) = self.comparisons.get(condition) {
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
            Operation::Reclaim { object } => {
                self.reclaim(*object)?;
                Ok(true)
            }
            Operation::CopyBytes { .. }
            | Operation::MoveRange { .. }
            | Operation::CloneRange { .. }
            | Operation::Share { .. }
            | Operation::Retain { .. }
            | Operation::Release { .. } => Err(InitialCheckError::UnsupportedMemoryOperation {
                function: self.function.id,
                operation: operation_name(operation),
            }),
            Operation::MoveValue { source, .. }
                if value_type(self.function, *source)?.is_memory_bearing() =>
            {
                Err(InitialCheckError::UnsupportedMemoryOperation {
                    function: self.function.id,
                    operation: "MoveValue",
                })
            }
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
        &self,
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
        self.require_type(destination, expected)
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
