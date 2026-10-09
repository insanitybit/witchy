use super::*;

pub(super) fn scalar_arguments(
    _function: FunctionId,
    arguments: &[CallArgument],
) -> Result<Vec<ValueId>, LoweringError> {
    arguments
        .iter()
        .map(|argument| match argument {
            CallArgument::Value(value) | CallArgument::Move(value) => Ok(*value),
        })
        .collect()
}

pub(super) fn host_scalar_wasm_type(
    function: FunctionId,
    value: ValueId,
    value_type: ValueType,
) -> Result<ValType, LoweringError> {
    match value_type {
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Size | ValueType::V128 =>
            wasm_value_type(function, value, value_type).map_err(|_| LoweringError::UnsupportedOperation { function, operation: "HostCall value" }),
        _ => Err(LoweringError::UnsupportedOperation { function, operation: "HostCall value" }),
    }
}

pub(super) fn runtime_byte_slice(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(ValueId, ValueId, ValueId), LoweringError> {
    let HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } = argument else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer" });
    };
    let Projection::Slice { start, length } = place.projection() else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
    };
    host_scalar_slice_place(function, value_types, layouts, place, AccessMode::Read, *length)?;
    Ok((place.object(), *start, *length))
}

/// Physical host slices are restricted to copied raw bytes or `Int` lanes.
/// The checked projection length remains in elements; the encoder multiplies
/// by this authenticated layout stride before forming a host memory range.
pub(super) fn host_scalar_slice_place(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    operation_length: ValueId,
) -> Result<(ValueId, ValueId, ValueId, u32, u32), LoweringError> {
    if place.access() != access {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall slice access" });
    }
    let Projection::Slice { start, length } = place.projection() else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall slice projection" });
    };
    if *length != operation_length || value_types.get(start) != Some(&ValueType::Size)
        || value_types.get(length) != Some(&ValueType::Size) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall slice bounds" });
    }
    let ValueType::Ref(layout) = value_types.get(&place.object()).copied().ok_or(
        LoweringError::UnsupportedOperation { function, operation: "HostCall slice object" })? else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall slice object" });
    };
    if place.value_type() != ValueType::Ref(layout) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall slice type" });
    }
    let collection = collection_layout(function, layouts, layout)?;
    if !matches!((collection.scalar, collection.stride),
        (ScalarKind::RawByte, 1) | (ScalarKind::Int, 8)) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall scalar slice layout" });
    }
    Ok((place.object(), *start, *length, collection.stride, collection.data_offset))
}

pub(super) fn raw_byte_slice_place(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    operation_length: ValueId,
) -> Result<(ValueId, ValueId, ValueId), LoweringError> {
    if place.access() != access {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes access" });
    }
    let Projection::Slice { start, length } = place.projection() else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes projection" });
    };
    if *length != operation_length {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes length" });
    }
    if value_types.get(start) != Some(&ValueType::Size) || value_types.get(length) != Some(&ValueType::Size) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes slice bounds" });
    }
    let ValueType::Ref(layout) = value_types.get(&place.object()).copied().ok_or(
        LoweringError::UnsupportedOperation { function, operation: "CopyBytes object" },
    )? else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes object" });
    };
    if place.value_type() != ValueType::Ref(layout) {
        return Err(LoweringError::UnsupportedOperation { function, operation: "CopyBytes type" });
    }
    let physical_layout = resolve_backend_layout(function, layouts, layout, "CopyBytes layout")?;
    let descriptor = layouts.get(physical_layout).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" })?;
    let LayoutKind::RuntimeBuffer { element } = descriptor.kind() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "CopyBytes layout" });
    };
    let physical_element = resolve_backend_layout(function, layouts, *element, "CopyBytes element")?;
    let element = layouts.get(physical_element).ok_or(LoweringError::UnsupportedLayout { function, operation: "CopyBytes element" })?;
    if element.kind() != &LayoutKind::Scalar(ScalarKind::RawByte) {
        return Err(LoweringError::UnsupportedLayout { function, operation: "CopyBytes element" });
    }
    Ok((place.object(), *start, *length))
}

pub(super) fn validate_host_call_lowering(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    import: &ImportContract,
    arguments: &[HostCallArgument],
    destinations: &[ValueId],
) -> Result<(), LoweringError> {
    if !matches!(import.abi_identity, ImportContractIdentity::AuthenticatedAbi(_))
        || !matches!(import.mutation, HostMutation::ReadOnly | HostMutation::WriteBuffers)
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall contract" });
    }
    if import.parameter_roles.len() != arguments.len() || import.result_roles.len() != destinations.len() {
        return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall arity" });
    }
    for (role, argument) in import.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(expected), HostCallArgument::Value(value)) => {
                if value_types.get(value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall value type" });
                }
                host_scalar_wasm_type(function, *value, *expected)?;
            }
            (HostParameterRole::Capability(expected), HostCallArgument::Capability(value)) => {
                if value_types.get(value) != Some(&ValueType::Capability(*expected)) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall capability type" });
                }
            }
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Read, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let _ = runtime_byte_slice(function, value_types, layouts, argument)?;
            }
            // A checked write slice: the same (ptr, len) pair as a read
            // slice, with the length the buffer's capacity. The checker
            // bounds the range by that capacity, and the guest publishes the
            // written count itself under its own guard.
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::Slice },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let Projection::Slice { length, .. } = place.projection() else {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
                };
                let _ = host_scalar_slice_place(function, value_types, layouts, place, AccessMode::Write, *length)?;
            }
            // A write buffer of a declared minimum size: the host receives
            // the checked slice's address only; the checker bounded the
            // slice by the object's capacity.
            (
                HostParameterRole::Buffer { value_type: ValueType::Ref(layout), access: AccessMode::Write, extent: HostBufferExtent::AtLeast(_) },
                HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) },
            ) => {
                if place.value_type() != ValueType::Ref(*layout) {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer type" });
                }
                let Projection::Slice { length, .. } = place.projection() else {
                    return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" });
                };
                let _ = host_scalar_slice_place(function, value_types, layouts, place, AccessMode::Write, *length)?;
            }
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall argument" }),
        }
    }
    for (role, destination) in import.result_roles.iter().zip(destinations) {
        let expected = match role {
            HostResultRole::Value(value_type) => *value_type,
            HostResultRole::Capability(kind) => ValueType::Capability(*kind),
            HostResultRole::NullableCapability(kind) => ValueType::NullableCapability(*kind),
        };
        if value_types.get(destination) != Some(&expected) {
            return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall result type" });
        }
        if let HostResultRole::Value(value_type) = role {
            host_scalar_wasm_type(function, *destination, *value_type)?;
        }
    }
    Ok(())
}

pub(super) fn validate_external_call_lowering(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    import: &ImportContract,
    signature: &FunctionSignature,
    arguments: &[CallArgument],
    destinations: &[ValueId],
) -> Result<(), LoweringError> {
    let scalar = |value| matches!(value, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit);
    if !matches!(import.abi_identity, ImportContractIdentity::AuthenticatedExternal(_))
        || !import.parameter_roles.is_empty()
        || !import.result_roles.is_empty()
        || !matches!(import.mutation, HostMutation::ReadOnly)
        || signature.parameters.len() != arguments.len()
        || signature.results.len() != destinations.len()
        || signature.contract.parameters.len() != signature.parameters.len()
        || signature.contract.results.len() != signature.results.len()
        || !signature.parameters.iter().copied().all(scalar)
        || !signature.results.iter().copied().all(scalar)
        || !signature.contract.parameter_effects.is_empty()
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall contract" });
    }
    for ((argument, expected), contract) in arguments.iter().zip(&signature.parameters).zip(&signature.contract.parameters) {
        let CallArgument::Value(value) = argument else {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall move argument" });
        };
        if value_types.get(value) != Some(expected)
            || contract.value_type != *expected
            || contract.ownership != ParameterOwnership::Value
        {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall argument" });
        }
        wasm_value_type(function, *value, *expected)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall scalar argument" })?;
    }
    for ((destination, expected), contract) in destinations.iter().zip(&signature.results).zip(&signature.contract.results) {
        if value_types.get(destination) != Some(expected)
            || !matches!(contract, crate::canonical_safewir::ResultContract::Value(value) if value == expected)
        {
            return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall result" });
        }
        wasm_value_type(function, *destination, *expected)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall scalar result" })?;
    }
    Ok(())
}

/// A host buffer's address is the byte address of its first element: an
/// i32 under `Byte`, an i64 under `Scaled8`, whose heap passes 4 GiB.
pub(super) fn host_import_wasm_signature(
    function: FunctionId,
    import: &ImportContract,
    refs: ref_encoding::RefEncoding,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    let address = match refs {
        ref_encoding::RefEncoding::Byte => ValType::I32,
        ref_encoding::RefEncoding::Scaled8 => ValType::I64,
    };
    let mut parameters = Vec::new();
    for role in &import.parameter_roles {
        match role {
            HostParameterRole::Value(value_type) => parameters.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostParameterRole::Capability(_) => parameters.push(ValType::EXTERNREF),
            HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Read, extent: HostBufferExtent::Slice }
            | HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Write, extent: HostBufferExtent::Slice } => {
                parameters.extend([address, ValType::I32]);
            }
            HostParameterRole::Buffer { value_type: ValueType::Ref(_), access: AccessMode::Write, extent: HostBufferExtent::AtLeast(_) } => {
                parameters.push(address);
            }
            _ => return Err(LoweringError::UnsupportedOperation { function, operation: "HostCall import signature" }),
        }
    }
    let mut results = Vec::new();
    for role in &import.result_roles {
        match role {
            HostResultRole::Value(value_type) => results.push(host_scalar_wasm_type(function, ValueId::new(u32::MAX), *value_type)?),
            HostResultRole::Capability(_) | HostResultRole::NullableCapability(_) => results.push(ValType::EXTERNREF),
        }
    }
    Ok((parameters, results))
}

pub(super) fn external_import_wasm_signature(
    function: FunctionId,
    signature: &FunctionSignature,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    let scalar = |value| matches!(value, ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit);
    if !signature.parameters.iter().copied().all(scalar)
        || !signature.results.iter().copied().all(scalar)
    {
        return Err(LoweringError::UnsupportedOperation { function, operation: "ExternalCall import signature" });
    }
    // Unit is source-level evidence only at this ABI: it consumes and
    // produces no Wasm lane.  In particular, an exported `() -> ()` has an
    // empty result vector rather than an invented i32 result.
    let parameters = signature.parameters.iter().enumerate()
        .filter(|(_, value)| **value != ValueType::Unit)
        .map(|(index, value)| wasm_value_type(function, ValueId::new(index as u32), *value)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall import parameter" }))
        .collect::<Result<Vec<_>, _>>()?;
    let results = signature.results.iter().enumerate()
        .filter(|(_, value)| **value != ValueType::Unit)
        .map(|(index, value)| wasm_value_type(function, ValueId::new(index as u32), *value)
            .map_err(|_| LoweringError::UnsupportedOperation { function, operation: "ExternalCall import result" }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((parameters, results))
}

pub(super) fn import_wasm_signature(
    function: FunctionId,
    import: &ImportContract,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    refs: ref_encoding::RefEncoding,
) -> Result<(Vec<ValType>, Vec<ValType>), LoweringError> {
    match &import.abi_identity {
        ImportContractIdentity::AuthenticatedAbi(_) => host_import_wasm_signature(function, import, refs),
        ImportContractIdentity::AuthenticatedExternal(_) => {
            let signature = signatures.get(&import.signature)
                .ok_or(LoweringError::UnsupportedOperation { function, operation: "ExternalCall missing signature" })?;
            external_import_wasm_signature(function, signature)
        }
        ImportContractIdentity::Unknown => Err(LoweringError::UnsupportedOperation { function, operation: "unknown import identity" }),
    }
}

pub(super) fn collection_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<CollectionLayout, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "layout")?;
    let descriptor = layouts
        .get(physical_layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    if let LayoutKind::CanonicalDictionary { .. } = descriptor.kind() {
        if descriptor.size() != (LayoutSize::Dynamic { base: 36, stride: 33 }) {
            return Err(LoweringError::UnsupportedLayout { function, operation: "dictionary layout" });
        }
        return Ok(CollectionLayout {
            layout,
            length_offset: 0,
            capacity_offset: Some(4),
            data_offset: 36,
            stride: 33,
            scalar: ScalarKind::Unit,
            pointer_layout: None,
            pointer_slot: None,
        });
    }
    let (element, raw_runtime_buffer) = match descriptor.kind() {
        LayoutKind::PackedList { element, .. } => (*element, false),
        LayoutKind::RuntimeBuffer { element } => (*element, true),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let physical_element = resolve_backend_layout(function, layouts, element, "layout")?;
    let element_descriptor = layouts
        .get(physical_element)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    let (scalar, pointer_layout, pointer_slot) = match element_descriptor.kind() {
        LayoutKind::Scalar(scalar) if *scalar == ScalarKind::Unit || scalar.is_ordinary_lowering_target() => (*scalar, None, None),
        LayoutKind::Scalar(ScalarKind::RawByte) if raw_runtime_buffer => {
            (ScalarKind::RawByte, None, None)
        }
        LayoutKind::RuntimePointerValueSlot {
            pointee,
            semantics,
        } if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
            (ScalarKind::U32, Some(*pointee), Some(*semantics))
        }
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "pointer-bearing layout" }),
    };
    let LayoutSize::Dynamic { base, stride } = descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    let (length_offset, capacity_offset, data_offset) = match (
        raw_runtime_buffer,
        descriptor.header(),
    ) {
        (true, HeaderLayout::None) if base == 4 && stride == 1 => (0, None, 4),
        (
            false,
            HeaderLayout::PackedList {
                length_offset,
                capacity_offset,
                data_offset,
                rc: RcHeader::Required,
            },
        ) => (length_offset, Some(capacity_offset), data_offset),
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "layout" }),
    };
    let LayoutSize::Fixed(element_size) = element_descriptor.size() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    };
    if data_offset != base || stride != element_size {
        return Err(LoweringError::UnsupportedLayout { function, operation: "layout" });
    }
    Ok(CollectionLayout {
        layout,
        length_offset,
        capacity_offset,
        data_offset,
        stride,
        scalar,
        pointer_layout,
        pointer_slot,
    })
}

/// Resolve a nominal identity only at the physical descriptor boundary. SSA
/// `ValueType::Ref` and the pointer metadata returned by this module retain
/// the original identity so exact source/ABI correspondence remains visible.
pub(super) fn resolve_backend_layout(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
    operation: &'static str,
) -> Result<crate::layout::LayoutId, LoweringError> {
    layouts
        .resolve_nominal(layout)
        .map_err(|_| LoweringError::UnsupportedLayout { function, operation })
}

pub(super) fn resolve_collection_layout(
    function: &Function,
    layouts: &LayoutInterner,
    object: ValueId,
) -> Result<CollectionLayout, LoweringError> {
    let Some(ValueType::Ref(layout)) = function.value_types.get(&object).copied() else {
        return Err(LoweringError::UnsupportedLayout { function: function.id, operation: "object" });
    };
    collection_layout(function.id, layouts, layout)
}

pub(super) fn resolve_collection_layout_for_values(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    object: ValueId,
) -> Result<CollectionLayout, LoweringError> {
    let Some(ValueType::Ref(layout)) = value_types.get(&object).copied() else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "object" });
    };
    collection_layout(function, layouts, layout)
}

pub(super) fn is_raw_allocation_capacity_load(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
) -> bool {
    matches!(place.projection(), Projection::Header(HeaderField::Capacity))
        && place.access() == AccessMode::Read
        && place.value_type() == ValueType::Size
        && resolve_collection_layout_for_values(function, value_types, layouts, place.object())
            .is_ok_and(|collection| collection.scalar == ScalarKind::RawByte
                && collection.capacity_offset.is_none()
                && collection.data_offset == 4
                && collection.stride == 1)
}

pub(super) fn resolve_memory_place(
    function: &Function,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<MemoryAccess, LoweringError> {
    if let Projection::Field { index } = place.projection() {
        let Some(ValueType::Ref(object_layout)) = function.value_types.get(&place.object()).copied() else {
            return Err(LoweringError::UnsupportedLayout { function: function.id, operation: "object" });
        };
        return field_access(function.id, layouts, place.object(), object_layout, *index);
    }
    let collection = resolve_collection_layout(function, layouts, place.object())?;
    match place.projection() {
        Projection::Header(field) => {
            let offset = match field {
                HeaderField::Length => collection.length_offset,
                HeaderField::Capacity => collection.capacity_offset.ok_or(LoweringError::UnsupportedLayout {
                    function: function.id,
                    operation: "header",
                })?,
            };
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, pointer_slot: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
            pointer_slot: collection.pointer_slot,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function: function.id,
            operation: "projection",
        }),
    }
}

/// A scalar field of a fixed-layout object: its address is the object plus
/// the field's offset, with no index. The access carries the field's own
/// stride so the shared address emitter needs no collection.
pub(super) fn field_access(
    function: FunctionId,
    layouts: &LayoutInterner,
    object: ValueId,
    layout: crate::layout::LayoutId,
    index: u32,
) -> Result<MemoryAccess, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "layout")?;
    let descriptor = layouts
        .get(physical_layout)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "layout" })?;
    if let LayoutKind::Scalar(scalar) = descriptor.kind() {
        if index != 0 || !matches!(scalar, ScalarKind::Int) {
            return Err(LoweringError::UnsupportedLayout { function, operation: "field" });
        }
        let width = scalar.size();
        return Ok(MemoryAccess {
            object,
            collection: CollectionLayout {
                layout,
                length_offset: 0,
                capacity_offset: None,
                data_offset: 0,
                stride: width,
                scalar: *scalar,
                pointer_layout: None,
                pointer_slot: None,
            },
            offset: 0,
            width,
            scalar: *scalar,
            pointer_layout: None,
            pointer_slot: None,
            index: None,
        });
    }
    let field = descriptor
        .fields()
        .get(index as usize)
        .ok_or(LoweringError::UnsupportedLayout { function, operation: "field" })?;
    // A scalar field is loaded as its kind; a pointer field (an owned pointer
    // value slot) is a 32-bit object address whose pointee the reference
    // transfer needs.
    let (scalar, pointer_layout, pointer_slot) = match field.kind() {
        crate::layout::FieldKind::Scalar(scalar) if scalar == ScalarKind::Unit || scalar.is_ordinary_lowering_target() => (scalar, None, None),
        crate::layout::FieldKind::Pointer { pointee, slot } if matches!(slot, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
            (ScalarKind::U32, Some(pointee), Some(slot))
        }
        crate::layout::FieldKind::Inline(slot) => {
            let physical_slot = resolve_backend_layout(function, layouts, slot, "field kind")?;
            match layouts.get(physical_slot).map(|slot| slot.kind()) {
            Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics })
                if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) =>
            {
                (ScalarKind::U32, Some(*pointee), Some(*semantics))
            }
            _ => return Err(LoweringError::UnsupportedLayout { function, operation: "field kind" }),
            }
        }
        _ => return Err(LoweringError::UnsupportedLayout { function, operation: "field kind" }),
    };
    let width = scalar.size();
    Ok(MemoryAccess {
        object,
        collection: CollectionLayout {
            layout,
            length_offset: 0,
            capacity_offset: None,
            data_offset: field.offset(),
            stride: width,
            scalar,
            pointer_layout,
            pointer_slot,
        },
        offset: field.offset(),
        width,
        scalar,
        pointer_layout,
        pointer_slot,
        index: None,
    })
}

pub(super) fn resolve_memory_place_for_values(
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<MemoryAccess, LoweringError> {
    if let Projection::Field { index } = place.projection() {
        let Some(ValueType::Ref(object_layout)) = value_types.get(&place.object()).copied() else {
            return Err(LoweringError::UnsupportedLayout { function, operation: "object" });
        };
        return field_access(function, layouts, place.object(), object_layout, *index);
    }
    let collection = resolve_collection_layout_for_values(function, value_types, layouts, place.object())?;
    match place.projection() {
        Projection::Header(field) => {
            let offset = match field {
                HeaderField::Length => collection.length_offset,
                HeaderField::Capacity => collection.capacity_offset.ok_or(LoweringError::UnsupportedLayout {
                    function,
                    operation: "header",
                })?,
            };
            Ok(MemoryAccess { object: place.object(), collection, offset, width: 4, scalar: ScalarKind::U32, pointer_layout: None, pointer_slot: None, index: None })
        }
        Projection::Element { index } => Ok(MemoryAccess {
            object: place.object(),
            collection,
            offset: collection.data_offset,
            width: collection.scalar.size(),
            scalar: collection.scalar,
            pointer_layout: collection.pointer_layout,
            pointer_slot: collection.pointer_slot,
            index: Some(*index),
        }),
        Projection::Field { .. } | Projection::Slice { .. } => Err(LoweringError::UnsupportedLayout {
            function,
            operation: "projection",
        }),
    }
}

pub(super) fn is_indirect_scalar(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::Int | ValueType::Float | ValueType::Bool | ValueType::Unit | ValueType::Size | ValueType::V128
    )
}

pub(super) fn is_indirect_result(value_type: ValueType) -> bool {
    is_indirect_scalar(value_type)
        || matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_)
            | ValueType::Capability(_) | ValueType::NullableCapability(_))
}

pub(super) fn is_indirect_parameter(signature: &FunctionSignature, index: usize, value_type: ValueType) -> bool {
    if is_indirect_scalar(value_type) {
        return true;
    }
    if matches!(value_type, ValueType::Capability(_) | ValueType::NullableCapability(_)) {
        // Externref capability lanes carry their exact checked kind, not
        // linear-memory ownership or permission to apply memory effects.
        return signature.contract.parameters.get(index).is_some_and(|parameter| {
            parameter.value_type == value_type
                && parameter.ownership == ParameterOwnership::Value
        }) && signature.contract.parameter_effects.get(&(index as u32))
            .is_none_or(|effects| effects.is_empty());
    }
    matches!(value_type, ValueType::Ref(_) | ValueType::Gc(_))
        && signature.contract.parameters.get(index).is_some_and(|parameter| {
            let effects = signature.contract.parameter_effects.get(&(index as u32));
            match parameter.ownership {
                ParameterOwnership::Shared | ParameterOwnership::WriteBack => effects
                    .is_none_or(|effects| effects.iter().all(|effect| matches!(
                        effect,
                        MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release,
                    ))),
                // A moved unique token can be written by its callee. The
                // source-bound affine plan and final SafeWIR check establish
                // the exact token transfer before this representation step.
                ParameterOwnership::Unique => effects
                    .is_none_or(|effects| effects.iter().all(|effect| matches!(
                        effect,
                        MemoryEffect::Read | MemoryEffect::Retain | MemoryEffect::Release | MemoryEffect::Write,
                    ))),
                // A `var unique` function-value lane has a checked write-back
                // contract.  Its caller still has to move the exact token and
                // the final SafeWIR checks the replacement token; accepting a
                // broader targetless effect summary here would be unsound.
                ParameterOwnership::UniqueWriteBack => effects.is_some_and(|effects| {
                    effects.iter().any(|effect| *effect == MemoryEffect::Write)
                        && effects.iter().all(|effect| matches!(
                            effect,
                            MemoryEffect::Read
                                | MemoryEffect::Retain
                                | MemoryEffect::Release
                                | MemoryEffect::Write,
                        ))
                }),
                ParameterOwnership::Value => is_authenticated_indirect_shared_alias(signature, index),
            }
        })
}

/// The non-escaping shared-reference closure lane: one closure environment,
/// one moved `&'a String` token, and one result aliasing exactly that source
/// parameter. Initial SafeWIR validation and canonical lowering have already
/// tied this shape to the pointer-bound source callable identity.
pub(super) fn is_authenticated_indirect_shared_alias(signature: &FunctionSignature, index: usize) -> bool {
    let ([environment, ValueType::Ref(parameter)], [ValueType::Ref(result)]) =
        (signature.parameters.as_slice(), signature.results.as_slice())
    else { return false };
    index == 1
        && *parameter == *result
        && matches!(environment, ValueType::Ref(_))
        && matches!(signature.contract.parameters.as_slice(), [
            ParameterContract { value_type: ValueType::Ref(environment_layout), ownership: ParameterOwnership::Shared },
            ParameterContract { value_type: ValueType::Ref(layout), ownership: ParameterOwnership::Value },
        ] if *environment == ValueType::Ref(*environment_layout) && *layout == *parameter)
        && matches!(signature.contract.results.as_slice(), [
            ResultContract::AliasesParameter { parameter: 1, layout },
        ] if *layout == *parameter)
        && matches!(signature.contract.parameter_effects.get(&0), Some(effects)
            if effects.as_slice() == [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release])
        && matches!(signature.contract.parameter_effects.get(&1), Some(effects)
            if effects.as_slice() == [MemoryEffect::Read, MemoryEffect::Retain, MemoryEffect::Release])
        && signature.contract.parameter_effects.len() == 2
}

/// The one memory-bearing indirect ABI this encoder admits.  Its authority is
/// not inferred here: initial SafeWIR validation has already bound the
/// closure-layout adapter to the checked source declaration and its exact
/// access identity.  This merely preserves that one physical lane through
/// `call_indirect`; every other write-bearing reference signature remains
/// refused.
pub(super) fn is_authenticated_indirect_callable_writeback(signature: &FunctionSignature) -> bool {
    let ([environment, ValueType::Ref(parameter)], [ordinary, write_back]) =
        (signature.parameters.as_slice(), signature.results.as_slice())
    else {
        return false;
    };
    *ordinary == ValueType::Ref(*parameter)
        && *write_back == ValueType::Ref(*parameter)
        && matches!(environment, ValueType::Ref(_))
        && matches!(signature.contract.parameters.as_slice(), [
            ParameterContract {
                value_type: ValueType::Ref(environment_layout),
                ownership: ParameterOwnership::Shared,
            },
            ParameterContract {
                value_type: ValueType::Ref(layout),
                ownership: ParameterOwnership::WriteBack,
            },
        ] if *environment == ValueType::Ref(*environment_layout) && *layout == *parameter)
        && matches!(signature.contract.results.as_slice(), [
            ResultContract::Shared { layout: ordinary_layout },
            ResultContract::Shared { layout: write_back_layout },
        ] if *ordinary_layout == *parameter && *write_back_layout == *parameter)
        && matches!(signature.contract.parameter_effects.get(&0), Some(effects)
            if effects.as_slice() == [
                MemoryEffect::Read,
                MemoryEffect::Retain,
                MemoryEffect::Release,
            ])
        && matches!(signature.contract.parameter_effects.get(&1), Some(effects)
            if effects.as_slice() == [
                MemoryEffect::Read,
                MemoryEffect::Retain,
                MemoryEffect::Release,
                MemoryEffect::Write,
            ])
        && signature.contract.parameter_effects.len() == 2
}

pub(super) fn checked_table_index(
    function_indices: &BTreeMap<FunctionId, u32>,
    function: FunctionId,
) -> Result<u32, CanonicalAdmissionError> {
    let first_defined = function_indices
        .values()
        .copied()
        .min()
        .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    function_indices
        .get(&function)
        .copied()
        .and_then(|index| index.checked_sub(first_defined))
        .ok_or(CanonicalAdmissionError::UnsupportedPlan)
}

pub(super) fn lower_operation(
    function: FunctionId,
    block: crate::canonical_safewir::BlockId,
    index: usize,
    operation: &Operation,
    value_types: &BTreeMap<ValueId, ValueType>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
) -> Result<WasmInstruction, LoweringError> {
    let source = SourceItemId::Operation { function, block, index };
    let operation = match operation {
        Operation::ReauthenticateRawHeaderExtent { .. } => ScalarInstruction::ProofNoop,
        Operation::ClosureOwner(operation) => ScalarInstruction::ClosureOwner(operation.clone()),
        Operation::Dictionary(operation) => ScalarInstruction::Dictionary(operation.clone()),
        Operation::ConstUnit { destination } => ScalarInstruction::UnitConst { destination: *destination },
        Operation::ConstInt { destination, value } => ScalarInstruction::IntConst { destination: *destination, value: *value },
        Operation::ConstFloat { destination, bits } => ScalarInstruction::FloatConst { destination: *destination, bits: *bits },
        Operation::ConstBool { destination, value } => ScalarInstruction::BoolConst { destination: *destination, value: *value },
        Operation::ConstSize { destination, value } => ScalarInstruction::SizeConst { destination: *destination, value: *value },
        Operation::CapabilityNone { destination, .. } => ScalarInstruction::CapabilityNone { destination: *destination },
        Operation::CapabilitySome { destination, source } => ScalarInstruction::CapabilitySome { destination: *destination, source: *source },
        Operation::CapabilityIsPresent { destination, source } => ScalarInstruction::CapabilityIsPresent { destination: *destination, source: *source },
        Operation::CapabilityUnwrap { destination, source } => ScalarInstruction::CapabilityUnwrap { destination: *destination, source: *source },
        Operation::Arithmetic { destination, operator, left, right, overflow } => {
            ScalarInstruction::Arithmetic {
                destination: *destination,
                operator: *operator,
                left: *left,
                right: *right,
                overflow: *overflow,
            }
        }
        Operation::Compare { destination, operator, left, right } => ScalarInstruction::Compare {
            destination: *destination,
            operator: *operator,
            left: *left,
            right: *right,
        },
        Operation::NarrowSize { destination, value, exact } => ScalarInstruction::NarrowSize { destination: *destination, value: *value, exact: *exact },
        Operation::WidenSize { destination, value } => ScalarInstruction::WidenSize { destination: *destination, value: *value },
        Operation::FloatUnary { destination, operator, value } => ScalarInstruction::FloatUnary { destination: *destination, operator: *operator, value: *value },
        Operation::Guard { condition, kind } => ScalarInstruction::Guard { condition: *condition, kind: *kind },
        Operation::MoveValue { destination, source } => {
            ScalarInstruction::MoveValue { destination: *destination, source: *source }
        }
        Operation::MakeInlineAggregate { destination, layout, fields } => {
            aggregates.insert(*destination, fields.clone());
            ScalarInstruction::MakeInlineAggregate { destination: *destination, layout: *layout, fields: fields.clone() }
        }
        Operation::ExtractInlineField { destination, aggregate, index } => {
            let fields = aggregates.get(aggregate).ok_or(LoweringError::UnsupportedOperation {
                function, operation: "ExtractInlineField aggregate lifetime",
            })?;
            let field = *fields.get(*index as usize).ok_or(LoweringError::UnsupportedOperation {
                function, operation: "ExtractInlineField index",
            })?;
            ScalarInstruction::ExtractInlineField {
                destination: *destination,
                aggregate: *aggregate,
                index: *index,
                field,
            }
        }
        Operation::FunctionValue { destination, function: target } => {
            let table_index = table_indices.get(target).copied().ok_or(
                LoweringError::UnsupportedOperation {
                    function,
                    operation: "FunctionValue target",
                },
            )?;
            ScalarInstruction::FunctionValue {
                destination: *destination,
                function: *target,
                table_index,
            }
        }
        Operation::Alloc { destination, layout, extent, class } => {
            // A counted collection allocates by capacity; a fixed layout (a
            // scalar record) allocates its constant size.
            let supported = match extent {
                Extent::Counted { .. } => collection_layout(function, layouts, *layout).is_ok(),
                Extent::FixedBytes(bytes) => resolve_backend_layout(function, layouts, *layout, "Alloc layout")
                    .ok()
                    .and_then(|physical_layout| layouts.get(physical_layout))
                    .is_some_and(|descriptor| matches!(descriptor.size(), LayoutSize::Fixed(size) if u64::from(size) == *bytes)),
                Extent::HeaderPrefix { .. } => false,
            };
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted || !supported {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Alloc" });
            }
            ScalarInstruction::Alloc { destination: *destination, layout: *layout, extent: extent.clone(), class: *class }
        }
        Operation::StaticData { destination, data, .. } => {
            ScalarInstruction::StaticData { destination: *destination, data: *data }
        }
        Operation::InitNullableOwned { place } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
                return Err(LoweringError::UnsupportedLayout { function, operation: "InitNullableOwned" });
            }
            ScalarInstruction::InitNullableOwned { place: place.clone() }
        }
        Operation::Load { destination, place, transfer } => {
            if is_raw_allocation_capacity_load(function, value_types, layouts, place) {
                if *transfer != LoadTransfer::CopyScalar || value_types.get(destination) != Some(&ValueType::Size) {
                    return Err(LoweringError::UnsupportedLayout { function, operation: "raw allocation capacity" });
                }
                ScalarInstruction::Load { destination: *destination, place: place.clone(), transfer: *transfer }
            } else {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            let valid_transfer = match access.pointer_layout {
                Some(pointee) if access.pointer_slot == Some(PointerSlot::OwnedNullable) => {
                    matches!(transfer, LoadTransfer::BorrowAffine | LoadTransfer::ClonePresent | LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField)
                        && place.value_type() == ValueType::Ref(pointee)
                }
                Some(pointee) => {
                    matches!(transfer, LoadTransfer::BorrowAffine | LoadTransfer::CloneShared | LoadTransfer::MoveUnique | LoadTransfer::TakeLast | LoadTransfer::TakeOwnedField)
                        && place.value_type() == ValueType::Ref(pointee)
                }
                None => matches!(transfer, LoadTransfer::CopyScalar | LoadTransfer::TakeLast),
            };
            if !valid_transfer || place.access() != AccessMode::Read {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Load" });
            }
            ScalarInstruction::Load { destination: *destination, place: place.clone(), transfer: *transfer }
            }
        }
        Operation::Store { place, value, transfer } => {
            let access = resolve_memory_place_for_values(function, value_types, layouts, place)?;
            let valid_transfer = match access.pointer_layout {
                Some(pointee) => {
                    matches!(*transfer, StoreTransfer::MoveUnique | StoreTransfer::CloneShared | StoreTransfer::ReplaceMoveUnique | StoreTransfer::ReplaceCloneShared)
                        && (!transfer.replaces_owned() || access.pointer_slot == Some(PointerSlot::Owned))
                        && place.value_type() == ValueType::Ref(pointee)
                        && value_types.get(value) == Some(&ValueType::Ref(pointee))
                }
                None => *transfer == StoreTransfer::CopyScalar,
            };
            if !valid_transfer || place.access() != AccessMode::Write {
                return Err(LoweringError::UnsupportedLayout { function, operation: "Store" });
            }
            ScalarInstruction::Store { place: place.clone(), value: *value, transfer: *transfer }
        }
        Operation::UpdateCount { object, count } => {
            if resolve_collection_layout_for_values(function, value_types, layouts, *object).is_err() {
                return Err(LoweringError::UnsupportedLayout { function, operation: "UpdateCount" });
            }
            ScalarInstruction::UpdateCount { object: *object, count: *count }
        }
        Operation::CloneRef { destination, source } => {
            // Reachable only through lower_validated. RC class and affine
            // ownership were established by the independent final checker,
            // including at parameters, calls, and CFG joins. Do not reconstruct
            // them from the order in which this backend visits instructions.
            ScalarInstruction::CloneRef { destination: *destination, source: *source }
        }
        Operation::Release { object: _ } | Operation::DropRef { object: _ } | Operation::Reclaim { object: _ } => {
            match operation {
                Operation::Release { object } => ScalarInstruction::Release { object: *object },
                Operation::DropRef { object } => ScalarInstruction::Release { object: *object },
                Operation::Reclaim { object } => ScalarInstruction::Reclaim { object: *object },
                _ => unreachable!(),
            }
        }
        Operation::Call { destinations, callee, arguments } => ScalarInstruction::Call {
            destinations: destinations.clone(),
            callee: *callee,
            arguments: scalar_arguments(function, arguments)?,
        },
        Operation::VarCall { destinations, callee, signature, arguments } => {
            let signature_def = signatures.get(signature).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "VarCall signature",
            })?;
            let callable_writeback = is_authenticated_indirect_callable_writeback(signature_def);
            if value_types.get(callee) != Some(&ValueType::Callable(*signature))
                || signature_def.parameters.len() != arguments.len()
                || signature_def.results.len() != destinations.len()
                || signature_def.parameters.iter().copied().enumerate().any(|(index, value_type)| {
                    !is_indirect_parameter(signature_def, index, value_type)
                        && !(callable_writeback && index == 1 && value_type == signature_def.parameters[1])
                })
                || signature_def.results.iter().copied().any(|value_type| !is_indirect_result(value_type))
            {
                return Err(LoweringError::UnsupportedOperation {
                    function,
                    operation: "VarCall memory-bearing or mismatched signature",
                });
            }
            for (argument, expected) in arguments.iter().zip(&signature_def.parameters) {
                let value = match argument {
                    CallArgument::Value(value) | CallArgument::Move(value) => *value,
                };
                if value_types.get(&value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function,
                        operation: "VarCall argument type",
                    });
                }
                if matches!(expected, ValueType::Ref(_)) && !matches!(argument, CallArgument::Move(_)) {
                    return Err(LoweringError::UnsupportedCallArgument { function });
                }
            }
            for (destination, expected) in destinations.iter().zip(&signature_def.results) {
                if value_types.get(destination) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function,
                        operation: "VarCall result type",
                    });
                }
            }
            ScalarInstruction::VarCall {
                destinations: destinations.clone(),
                callee: *callee,
                signature: *signature,
                arguments: scalar_arguments(function, arguments)?,
            }
        }
        Operation::ExternalCall { destinations, import, arguments } => {
            let contract = imports.get(import).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "ExternalCall contract",
            })?;
            let signature = signatures.get(&contract.signature).ok_or(LoweringError::UnsupportedOperation {
                function,
                operation: "ExternalCall signature",
            })?;
            validate_external_call_lowering(function, value_types, contract, signature, arguments, destinations)?;
            ScalarInstruction::ExternalCall {
                destinations: destinations.clone(),
                import: *import,
                arguments: scalar_arguments(function, arguments)?,
            }
        }
        Operation::HostCall { destinations, import, arguments } => {
            let contract = imports.get(import).ok_or(LoweringError::UnsupportedOperation { function, operation: "HostCall contract" })?;
            validate_host_call_lowering(function, value_types, layouts, contract, arguments, destinations)?;
            ScalarInstruction::HostCall { destinations: destinations.clone(), import: *import, arguments: arguments.clone() }
        }
        Operation::CopyBytes { destination, source, length } => {
            raw_byte_slice_place(function, value_types, layouts, destination, AccessMode::Write, *length)?;
            raw_byte_slice_place(function, value_types, layouts, source, AccessMode::Read, *length)?;
            ScalarInstruction::CopyBytes {
                destination: destination.clone(),
                source: source.clone(),
                length: *length,
            }
        }
        Operation::MoveRange { destination, source, count } => {
            let destination_layout = resolve_collection_layout_for_values(function, value_types, layouts, destination.object())?;
            let source_layout = resolve_collection_layout_for_values(function, value_types, layouts, source.object())?;
            if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || destination_layout != source_layout
                || !matches!(destination.projection(), Projection::Slice { length, .. } if *length == *count)
                || !matches!(source.projection(), Projection::Slice { length, .. } if *length == *count)
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "MoveRange" });
            }
            ScalarInstruction::MoveRange { destination: destination.clone(), source: source.clone(), count: *count }
        }
        Operation::CloneRange { destination, source, count } => {
            let destination_layout = resolve_collection_layout_for_values(function, value_types, layouts, destination.object())?;
            let source_layout = resolve_collection_layout_for_values(function, value_types, layouts, source.object())?;
            if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
                || destination_layout != source_layout
                || !matches!(destination.projection(), Projection::Slice { length, .. } if *length == *count)
                || !matches!(source.projection(), Projection::Slice { length, .. } if *length == *count)
            {
                return Err(LoweringError::UnsupportedLayout { function, operation: "CloneRange" });
            }
            ScalarInstruction::CloneRange { destination: destination.clone(), source: source.clone(), count: *count }
        }
        Operation::Retain { .. } => {
            return Err(LoweringError::UnsupportedOperation { function, operation: operation_name(operation) });
        }
    };
    Ok(WasmInstruction { source, operation })
}

pub(super) fn operation_name(operation: &Operation) -> &'static str {
    match operation {
        Operation::ClosureOwner(_) => "ClosureOwner",
        Operation::Dictionary(_) => "Dictionary",
        Operation::ConstUnit { .. } => "ConstUnit",
        Operation::ConstInt { .. } => "ConstInt",
        Operation::ConstFloat { .. } => "ConstFloat",
        Operation::ConstBool { .. } => "ConstBool",
        Operation::ConstSize { .. } => "ConstSize",
        Operation::CapabilityNone { .. } => "CapabilityNone",
        Operation::CapabilitySome { .. } => "CapabilitySome",
        Operation::CapabilityIsPresent { .. } => "CapabilityIsPresent",
        Operation::CapabilityUnwrap { .. } => "CapabilityUnwrap",
        Operation::Arithmetic { .. } => "Arithmetic",
        Operation::Compare { .. } => "Compare",
        Operation::NarrowSize { .. } => "NarrowSize",
        Operation::WidenSize { .. } => "WidenSize",
        Operation::FloatUnary { .. } => "FloatUnary",
        Operation::Guard { .. } => "Guard",
        Operation::ReauthenticateRawHeaderExtent { .. } => "ReauthenticateRawHeaderExtent",
        Operation::Alloc { .. } => "Alloc",
        Operation::StaticData { .. } => "StaticData",
        Operation::InitNullableOwned { .. } => "InitNullableOwned",
        Operation::Load { .. } => "Load",
        Operation::Store { .. } => "Store",
        Operation::CopyBytes { .. } => "CopyBytes",
        Operation::MoveRange { .. } => "MoveRange",
        Operation::CloneRange { .. } => "CloneRange",
        Operation::UpdateCount { .. } => "UpdateCount",
        Operation::MoveValue { .. } => "MoveValue",
        Operation::MakeInlineAggregate { .. } => "MakeInlineAggregate",
        Operation::ExtractInlineField { .. } => "ExtractInlineField",
        Operation::FunctionValue { .. } => "FunctionValue",
        Operation::CloneRef { .. } => "CloneRef",
        Operation::Retain { .. } => "Retain",
        Operation::Release { .. } => "Release",
        Operation::DropRef { .. } => "DropRef",
        Operation::Reclaim { .. } => "Reclaim",
        Operation::Call { .. } => "Call",
        Operation::VarCall { .. } => "VarCall",
        Operation::ExternalCall { .. } => "ExternalCall",
        Operation::HostCall { .. } => "HostCall",
    }
}

pub(super) fn lower_terminator(
    function: &Function,
    block: crate::canonical_safewir::BlockId,
    terminator: &Terminator,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
) -> Result<WasmTerminator, LoweringError> {
    let source = SourceItemId::Terminator { function: function.id, block };
    let operation = match terminator {
        Terminator::Branch { target, arguments } => ScalarTerminator::Branch { target: *target, arguments: arguments.clone() },
        Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => ScalarTerminator::BranchIf {
            condition: *condition,
            then_target: *then_target,
            then_arguments: then_arguments.clone(),
            else_target: *else_target,
            else_arguments: else_arguments.clone(),
        },
        Terminator::Return { values } => ScalarTerminator::Return { values: values.clone() },
        Terminator::TailCall { site, callee, arguments } => ScalarTerminator::TailCall {
            site: *site,
            callee: *callee,
            arguments: scalar_arguments(function.id, arguments)?,
        },
        Terminator::VarTailCall { site, callee, signature, arguments } => {
            let signature_def = signatures.get(signature).ok_or(
                LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "VarTailCall signature",
                },
            )?;
            if function.value_types.get(callee) != Some(&ValueType::Callable(*signature))
                || signature_def.parameters.len() != arguments.len()
                || signature_def
                    .parameters
                    .iter()
                    .copied()
                    .enumerate()
                    .any(|(index, value_type)| !is_indirect_parameter(signature_def, index, value_type))
                || signature_def
                    .results
                    .iter()
                    .copied()
                    .any(|value_type| !is_indirect_result(value_type))
            {
                return Err(LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "VarTailCall memory-bearing or mismatched signature",
                });
            }
            for (argument, expected) in arguments.iter().zip(&signature_def.parameters) {
                let value = match argument {
                    CallArgument::Value(value) | CallArgument::Move(value) => *value,
                };
                if function.value_types.get(&value) != Some(expected) {
                    return Err(LoweringError::UnsupportedOperation {
                        function: function.id,
                        operation: "VarTailCall argument type",
                    });
                }
                if matches!(expected, ValueType::Ref(_)) && !matches!(argument, CallArgument::Move(_)) {
                    return Err(LoweringError::UnsupportedCallArgument { function: function.id });
                }
            }
            ScalarTerminator::VarTailCall {
                site: *site,
                callee: *callee,
                signature: *signature,
                arguments: scalar_arguments(function.id, arguments)?,
            }
        }
        Terminator::Trap => ScalarTerminator::Trap,
        Terminator::Unreachable => ScalarTerminator::Unreachable,
    };
    Ok(WasmTerminator { source, operation })
}

pub(super) fn lower_block(
    function: &Function,
    block: &BasicBlock,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
) -> Result<WasmBlock, LoweringError> {
    let mut instructions = Vec::with_capacity(block.operations.len());
    for (index, operation) in block.operations.iter().enumerate() {
        instructions.push(lower_operation(function.id, block.id, index, operation, &function.value_types, signatures, table_indices, layouts, imports, aggregates)?);
    }
    Ok(WasmBlock {
        id: block.id,
        instructions,
        terminator: lower_terminator(function, block.id, &block.terminator, signatures)?,
    })
}

pub(super) fn lower_function(
    function: &Function,
    layouts: &LayoutInterner,
    imports: &BTreeMap<ImportId, ImportContract>,
    signatures: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, FunctionSignature>,
    table_indices: &BTreeMap<FunctionId, u32>,
) -> Result<WasmFunction, LoweringError> {
    let mut aggregates = BTreeMap::new();
    Ok(WasmFunction {
        id: function.id,
        signature: function.signature,
        entry: function.entry,
        blocks: function
            .blocks
            .iter()
            .map(|block| lower_block(function, block, layouts, imports, signatures, table_indices, &mut aggregates))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub(super) fn lower_validated(module: &ValidatedSafeWir) -> Result<WasmIr, LoweringError> {
    let module = module.module();
    let (_, layouts) = crate::layout::LayoutBundle::decode_canonical(&module.layouts().canonical_bytes())
        .map_err(|_| LoweringError::InvalidLayoutBundle)?;
    let refs = ref_encoding::RefEncoding::for_heap(
        witchy_canonical_source::capabilities::HeapMode::from_launch_contract(module.launch_contract_payload()));
    let (static_data, heap_start) = static_data_plan(module.static_data_segments(), refs)?;
    let imports = module.imports().iter().map(|import| (import.id, import.clone())).collect::<BTreeMap<_, _>>();
    let signatures = module.signatures().iter().map(|signature| (signature.id, signature.clone())).collect::<BTreeMap<_, _>>();
    let has_function_table = module.functions().iter().flat_map(|function| function.blocks.iter()).any(|block| {
        matches!(block.terminator, Terminator::VarTailCall { .. })
            || block.operations.iter().any(|operation| matches!(operation,
                Operation::VarCall { .. } | Operation::FunctionValue { .. }
                | Operation::ClosureOwner(
                    crate::canonical_safewir::closure_owner_contract::OwnerOperation::Construct { .. }
                    | crate::canonical_safewir::closure_owner_contract::OwnerOperation::ReadCallable { .. }
                )
            ))
    });
    let table_indices = module
        .functions()
        .iter()
        .enumerate()
        .map(|(index, function)| {
            u32::try_from(index)
                .map(|index| (function.id, index))
                .map_err(|_| LoweringError::UnsupportedOperation {
                    function: function.id,
                    operation: "function table size",
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(WasmIr {
        owner_bridges: owner_bridges::build(module)?,
        revision: module.revision(),
        checked_source: module.checked_source(),
        launch_contract_payload: module.launch_contract_payload().to_vec(),
        entry_payload: None,
        static_data,
        imports: module.imports().iter().map(|import| WasmImport { id: import.id, signature: import.signature }).collect(),
        table: has_function_table.then(|| WasmTable {
            functions: module.functions().iter().map(|function| function.id).collect(),
        }),
        heap_start,
        refs,
        functions: module
            .functions()
            .iter()
            .map(|function| lower_function(function, &layouts, &imports, &signatures, &table_indices))
            .collect::<Result<Vec<_>, _>>()?,
    })
}
