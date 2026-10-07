use super::*;

pub(super) fn wasm_value_type(
    function: FunctionId,
    value: ValueId,
    value_type: crate::canonical_safewir::ValueType,
) -> Result<ValType, EncodeError> {
    match value_type {
        crate::canonical_safewir::ValueType::Int => Ok(ValType::I64),
        crate::canonical_safewir::ValueType::Float => Ok(ValType::F64),
        crate::canonical_safewir::ValueType::Bool | crate::canonical_safewir::ValueType::Unit => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Size => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Ref(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::Gc(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::Capability(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::NullableCapability(_) => Ok(ValType::EXTERNREF),
        crate::canonical_safewir::ValueType::V128 => Ok(ValType::V128),
        // SafeWIR never exposes this representation. The callable is an
        // authenticated table-index value only after private WasmIR lowering.
        crate::canonical_safewir::ValueType::Callable(_) => Ok(ValType::I32),
        crate::canonical_safewir::ValueType::InlineAggregate(_) => {
            Err(EncodeError::UnsupportedValueType { function, value, value_type })
        }
    }
}

#[derive(Clone)]
pub(super) struct LocalLayout<'a> {
    pub(super) memory_helpers: Option<std::sync::Arc<outlined_memory::Helpers>>,
    pub(super) owner_scratch: Option<[u32; 4]>,
    // Immutable admitted recipes are module-wide, not per-function state.
    // Copy only references: cleanup recipes contain the complete owner family
    // graph and deep-cloning them here multiplies that graph by function count.
    pub(super) owner_bridges: Vec<&'a AdmittedOwnerBridge>,
    pub(super) indices: BTreeMap<ValueId, u32>,
    pub(super) extra_types: Vec<ValType>,
    pub(super) scratch: Vec<u32>,
    // This function's owner-kernel context, parked between environment
    // operations (see `owner_operation_kernel::OwnerScratch`).
    pub(super) owner_context: owner_operation_kernel::ParkedContext,
}

pub(super) fn local_layout<'a>(
    function: &Function,
    signature: &FunctionSignature,
    layouts: &LayoutInterner,
) -> Result<LocalLayout<'a>, EncodeError> {
    let entry = function
        .blocks
        .iter()
        .find(|block| block.id == function.entry)
        .ok_or(EncodeError::MissingBlock { function: function.id, block: function.entry })?;
    if entry.parameters.len() != signature.parameters.len() {
        return Err(EncodeError::ParameterLayoutMismatch { function: function.id });
    }
    let mut indices = BTreeMap::new();
    for (index, parameter) in entry.parameters.iter().enumerate() {
        if parameter.value_type != signature.parameters[index]
            || function.value_types.get(&parameter.value) != Some(&parameter.value_type)
            || indices.insert(parameter.value, index as u32).is_some()
        {
            return Err(EncodeError::ParameterLayoutMismatch { function: function.id });
        }
    }
    let mut extra_types = Vec::new();
    let temporary_slots = scalar_locals::slots(function);
    scalar_locals::verify(function, &temporary_slots).map_err(|_| EncodeError::UnsupportedLayout {
        function: function.id, operation: "scalar local slot assignment",
    })?;
    let mut temporary_indices = BTreeMap::new();
    for (value, value_type) in &function.value_types {
        if indices.contains_key(value) {
            continue;
        }
        if matches!(value_type, ValueType::InlineAggregate(_)) {
            continue;
        }
        let ty = wasm_value_type(function.id, *value, *value_type)?;
        let index = if let Some(slot) = temporary_slots.get(value) {
            *temporary_indices.entry(*slot).or_insert_with(|| {
                let index = entry.parameters.len() as u32 + extra_types.len() as u32;
                extra_types.push(ty);
                index
            })
        } else {
            let index = entry.parameters.len() as u32 + extra_types.len() as u32;
            extra_types.push(ty);
            index
        };
        indices.insert(*value, index);
    }
// Derive scratch space from the whole-function value table. The independent
// operation planner retains that table when it plans one block at a time;
// inspecting only that block's operations would shift every allocator local.
let owner_scratch = if function.value_types.values().any(|value| matches!(value, ValueType::Gc(_))) {
    let base = entry.parameters.len() as u32 + extra_types.len() as u32;
    // The first two slots are linear-reference temporaries. The third holds
    // an opaque Gc token (for example a nested rooted-slice ReadOwner result)
    // and must therefore be externref. The fourth preserves the local slice
    // coordinate across linear-environment cleanup, which consumes the
    // ordinary worklist scratch slots.
    extra_types.extend([
        ValType::I32,
        ValType::I32,
        ValType::Ref(RefType::EXTERNREF),
        ValType::I32,
    ]);
    Some([base, base + 1, base + 2, base + 3])
} else { None };
let mut scratch = vec![0; drop_worklist::SCRATCH];

    for (slot, index) in scratch.iter_mut().enumerate() {
        *index = entry.parameters.len() as u32 + extra_types.len() as u32;
        extra_types.push(ValType::I32);
        if slot == 0 {
            // Keep the first scratch slot adjacent to the value locals. The
            // explicit indices below are still independent of value IDs.
        }
    }
    if function.blocks.iter().any(|block| block.operations.iter().any(|operation|
        matches!(operation, Operation::Dictionary(
            crate::canonical_safewir::dictionary::DictionaryOperation::ProbeGroup { .. }
        ))
    )) {
        scratch.push(entry.parameters.len() as u32 + extra_types.len() as u32);
        extra_types.push(ValType::V128);
    }
    Ok(LocalLayout {
        memory_helpers: None,
        owner_scratch, owner_bridges: Vec::new(),
        indices,
        extra_types,
        scratch,
        owner_context: Default::default(),
    })
}

pub(super) fn scratch_index(layout: &LocalLayout, slot: usize) -> u32 {
    layout.scratch[slot]
}

pub(super) fn memarg(width: u32) -> MemArg {
    MemArg { offset: 0, align: width.next_power_of_two().trailing_zeros() as u32, memory_index: 0 }
}

/// `memarg` with a static byte offset in its immediate.
pub(super) fn memarg_at(width: u32, offset: u64) -> MemArg {
    MemArg { offset, ..memarg(width) }
}

/// The base of `access` on the stack and the static offset its memory
/// instruction takes as an immediate. Wasm adds that immediate to the base
/// without wrapping and traps past the end of memory, so a field or header
/// access needs no overflow check of its own: every address the checked sum
/// trapped on lies past any wasm32 memory, so the access still traps (an
/// access ending exactly at byte 2^32 of a full 4 GiB memory, which that
/// check refused, is in bounds and now succeeds).
///
/// An element access adds `index * stride` to the base in i32: the checker
/// admits an element read only below the object's count and a write only
/// below its capacity, whose whole allocation lies inside memory, so that
/// sum cannot wrap. The immediate carries the element offset.
pub(super) fn emit_access_base(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<u64, EncodeError> {
    emit_get(body, layout, function.id, access.object)?;
    if let Some(index) = access.index {
        emit_get(body, layout, function.id, index)?;
        if access.collection.stride != 1 {
            body.instruction(&wasm_encoder::Instruction::I32Const(access.collection.stride as i32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
        }
        body.instruction(&wasm_encoder::Instruction::I32Add);
    }
    Ok(u64::from(access.offset))
}

pub(super) fn emit_u32_trap_if_gt(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
    limit: u32,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function, value)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(limit)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

pub(super) fn emit_projected_address(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<(), EncodeError> {
    if let Some(index) = access.index {
        let maximum = (u32::MAX
            .checked_sub(access.offset)
            .and_then(|value| value.checked_sub(access.width))
            .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "address" })?)
            / access.collection.stride;
        // The final address check also rejects an oversized index when the
        // complete u32 object/index calculation cannot overflow u64. Keep the
        // separate index guard for unusually large strides where it can.
        if !access.indexed_address_fits_u64() {
            emit_u32_trap_if_gt(body, layout, function.id, index, maximum)?;
        }
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        emit_get(body, layout, function.id, index)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.collection.stride)));
        body.instruction(&wasm_encoder::Instruction::I64Mul);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        body.instruction(&wasm_encoder::Instruction::I64Add);
    } else {
        let maximum = u32::MAX.checked_sub(access.width).ok_or(EncodeError::UnsupportedLayout {
            function: function.id,
            operation: "address",
        })?;
    if let Some(helpers) = &layout.memory_helpers {
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(access.offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Const(maximum as i32));
        body.instruction(&wasm_encoder::Instruction::Call(helpers.address));
        return Ok(());
    }
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(maximum)));
        body.instruction(&wasm_encoder::Instruction::I64GtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get(body, layout, function.id, access.object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(access.offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        return Ok(());
    }
    emit_get(body, layout, function.id, access.object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, access.index.expect("element access has an index"))?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(access.offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX - access.width)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    Ok(())
}


pub(super) fn local_index(layout: &LocalLayout, function: FunctionId, value: ValueId) -> Result<u32, EncodeError> {
    layout.indices.get(&value).copied().ok_or(EncodeError::MissingValue { function, value })
}

pub(super) fn emit_get(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
) -> Result<(), EncodeError> {
    body.instruction(&wasm_encoder::Instruction::LocalGet(local_index(layout, function, value)?));
    Ok(())
}

pub(super) fn emit_set(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value: ValueId,
) -> Result<(), EncodeError> {
    body.instruction(&wasm_encoder::Instruction::LocalSet(local_index(layout, function, value)?));
    Ok(())
}

pub(super) fn emit_get_index(body: &mut wasm_encoder::Function, index: u32) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(index));
}

pub(super) fn emit_set_index(body: &mut wasm_encoder::Function, index: u32) {
    body.instruction(&wasm_encoder::Instruction::LocalSet(index));
}

pub(super) fn emit_arithmetic(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ArithmeticOperator,
    left: ValueId,
    right: ValueId,
) -> Result<(), EncodeError> {
    let value_type = function
        .value_types
        .get(&destination)
        .copied()
        .ok_or(EncodeError::MissingValue { function: function.id, value: destination })?;
    emit_get(body, layout, function.id, left)?;
    emit_get(body, layout, function.id, right)?;
    match value_type {
        crate::canonical_safewir::ValueType::Int => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::I64Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::I64Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::I64Mul); }
            ArithmeticOperator::DivideSigned => { body.instruction(&wasm_encoder::Instruction::I64DivS); }
            ArithmeticOperator::DivideUnsigned => { body.instruction(&wasm_encoder::Instruction::I64DivU); }
            ArithmeticOperator::RemainderSigned => { body.instruction(&wasm_encoder::Instruction::I64RemS); }
            ArithmeticOperator::RemainderUnsigned => { body.instruction(&wasm_encoder::Instruction::I64RemU); }
            ArithmeticOperator::BitAnd => { body.instruction(&wasm_encoder::Instruction::I64And); }
            ArithmeticOperator::BitOr => { body.instruction(&wasm_encoder::Instruction::I64Or); }
            ArithmeticOperator::BitXor => { body.instruction(&wasm_encoder::Instruction::I64Xor); }
            ArithmeticOperator::ShiftLeft => { body.instruction(&wasm_encoder::Instruction::I64Shl); }
            ArithmeticOperator::ShiftRightSigned => { body.instruction(&wasm_encoder::Instruction::I64ShrS); }
            ArithmeticOperator::ShiftRightUnsigned => { body.instruction(&wasm_encoder::Instruction::I64ShrU); }
        },
        crate::canonical_safewir::ValueType::Size => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::I32Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::I32Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::I32Mul); }
            ArithmeticOperator::DivideSigned => { body.instruction(&wasm_encoder::Instruction::I32DivS); }
            ArithmeticOperator::DivideUnsigned => { body.instruction(&wasm_encoder::Instruction::I32DivU); }
            ArithmeticOperator::RemainderSigned => { body.instruction(&wasm_encoder::Instruction::I32RemS); }
            ArithmeticOperator::RemainderUnsigned => { body.instruction(&wasm_encoder::Instruction::I32RemU); }
            ArithmeticOperator::BitAnd => { body.instruction(&wasm_encoder::Instruction::I32And); }
            ArithmeticOperator::BitOr => { body.instruction(&wasm_encoder::Instruction::I32Or); }
            ArithmeticOperator::BitXor => { body.instruction(&wasm_encoder::Instruction::I32Xor); }
            ArithmeticOperator::ShiftLeft => { body.instruction(&wasm_encoder::Instruction::I32Shl); }
            ArithmeticOperator::ShiftRightSigned => { body.instruction(&wasm_encoder::Instruction::I32ShrS); }
            ArithmeticOperator::ShiftRightUnsigned => { body.instruction(&wasm_encoder::Instruction::I32ShrU); }
        },
        crate::canonical_safewir::ValueType::Float => match operator {
            ArithmeticOperator::Add => { body.instruction(&wasm_encoder::Instruction::F64Add); }
            ArithmeticOperator::Subtract => { body.instruction(&wasm_encoder::Instruction::F64Sub); }
            ArithmeticOperator::Multiply => { body.instruction(&wasm_encoder::Instruction::F64Mul); }
            ArithmeticOperator::DivideSigned | ArithmeticOperator::DivideUnsigned => {
                body.instruction(&wasm_encoder::Instruction::F64Div);
            }
            _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: destination, value_type }),
        },
        _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: destination, value_type }),
    }
    emit_set(body, layout, function.id, destination)
}

pub(super) fn emit_checked_arithmetic(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ArithmeticOperator,
    left: ValueId,
    right: ValueId,
    overflow: crate::canonical_safewir::OverflowBehavior,
) -> Result<(), EncodeError> {
    match (
        function.value_types.get(&destination),
        operator,
        overflow,
    ) {
        (
            Some(ValueType::Int),
            ArithmeticOperator::Add,
            crate::canonical_safewir::OverflowBehavior::Trap,
        ) => {
            // Signed addition overflows exactly when both operands have the
            // same sign and the sum has the opposite sign:
            // `((sum ^ left) & (sum ^ right)) < 0`.
            emit_get(body, layout, function.id, left)?;
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::LocalTee(local_index(
                layout,
                function.id,
                destination,
            )?));
            emit_get(body, layout, function.id, left)?;
            body.instruction(&wasm_encoder::Instruction::I64Xor);
            emit_get(body, layout, function.id, destination)?;
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64Xor);
            body.instruction(&wasm_encoder::Instruction::I64And);
            body.instruction(&wasm_encoder::Instruction::I64Const(0));
            body.instruction(&wasm_encoder::Instruction::I64LtS);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            return Ok(());
        }
        (
            Some(ValueType::Size),
            ArithmeticOperator::Add,
            crate::canonical_safewir::OverflowBehavior::Trap,
        ) => {
            emit_get(body, layout, function.id, left)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            emit_get(body, layout, function.id, right)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
            body.instruction(&wasm_encoder::Instruction::I64GtU);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
        }
        (
            Some(ValueType::Int | ValueType::Size),
            ArithmeticOperator::DivideSigned
            | ArithmeticOperator::DivideUnsigned
            | ArithmeticOperator::RemainderSigned
            | ArithmeticOperator::RemainderUnsigned,
            crate::canonical_safewir::OverflowBehavior::Trap,
        )
        | (_, _, crate::canonical_safewir::OverflowBehavior::Wrap) => {}
        _ => {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedOverflow {
                function: function.id,
            }));
        }
    }
    emit_arithmetic(body, function, layout, destination, operator, left, right)
}

pub(super) fn emit_compare(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    operator: ComparisonOperator,
    left: ValueId,
    right: ValueId,
) -> Result<(), EncodeError> {
    let value_type = function
        .value_types
        .get(&left)
        .copied()
        .ok_or(EncodeError::MissingValue { function: function.id, value: left })?;
    let instruction = match value_type {
        crate::canonical_safewir::ValueType::Int => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::I64Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::I64Ne,
            ComparisonOperator::LessSigned => wasm_encoder::Instruction::I64LtS,
            ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::I64LtU,
            ComparisonOperator::LessEqualSigned => wasm_encoder::Instruction::I64LeS,
            ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::I64LeU,
            ComparisonOperator::GreaterSigned => wasm_encoder::Instruction::I64GtS,
            ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::I64GtU,
            ComparisonOperator::GreaterEqualSigned => wasm_encoder::Instruction::I64GeS,
            ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::I64GeU,
        },
        crate::canonical_safewir::ValueType::Size | crate::canonical_safewir::ValueType::Bool => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::I32Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::I32Ne,
            ComparisonOperator::LessSigned => wasm_encoder::Instruction::I32LtS,
            ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::I32LtU,
            ComparisonOperator::LessEqualSigned => wasm_encoder::Instruction::I32LeS,
            ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::I32LeU,
            ComparisonOperator::GreaterSigned => wasm_encoder::Instruction::I32GtS,
            ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::I32GtU,
            ComparisonOperator::GreaterEqualSigned => wasm_encoder::Instruction::I32GeS,
            ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::I32GeU,
        },
        crate::canonical_safewir::ValueType::Float => match operator {
            ComparisonOperator::Equal => wasm_encoder::Instruction::F64Eq,
            ComparisonOperator::NotEqual => wasm_encoder::Instruction::F64Ne,
            ComparisonOperator::LessSigned | ComparisonOperator::LessUnsigned => wasm_encoder::Instruction::F64Lt,
            ComparisonOperator::LessEqualSigned | ComparisonOperator::LessEqualUnsigned => wasm_encoder::Instruction::F64Le,
            ComparisonOperator::GreaterSigned | ComparisonOperator::GreaterUnsigned => wasm_encoder::Instruction::F64Gt,
            ComparisonOperator::GreaterEqualSigned | ComparisonOperator::GreaterEqualUnsigned => wasm_encoder::Instruction::F64Ge,
        },
        _ => return Err(EncodeError::UnsupportedValueType { function: function.id, value: left, value_type }),
    };
    emit_get(body, layout, function.id, left)?;
    emit_get(body, layout, function.id, right)?;
    body.instruction(&instruction);
    emit_set(body, layout, function.id, destination)
}

pub(super) fn emit_narrow_size(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: ValueId,
    value: ValueId,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I64Const(0));
    body.instruction(&wasm_encoder::Instruction::I64LtS);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtS);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, value)?;
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    emit_set(body, layout, function.id, destination)
}

pub(super) fn emit_typed_load(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
    destination: ValueId,
) -> Result<(), EncodeError> {
    let offset = emit_access_base(body, function, layout, access)?;
    let arg = memarg_at(access.width, offset);
    match access.scalar {
        ScalarKind::Unit => {
            body.instruction(&wasm_encoder::Instruction::I32Load(arg));
            body.instruction(&wasm_encoder::Instruction::Drop);
            body.instruction(&wasm_encoder::Instruction::I32Const(0))
        }
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => body.instruction(&wasm_encoder::Instruction::I32Load8U(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Load16U(arg)),
        ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => body.instruction(&wasm_encoder::Instruction::I32Load(arg)),
        ScalarKind::Int | ScalarKind::Duration => body.instruction(&wasm_encoder::Instruction::I64Load(arg)),
        ScalarKind::Float => body.instruction(&wasm_encoder::Instruction::F64Load(arg)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "scalar load" }),
    };
    emit_set(body, layout, function.id, destination)
}

pub(super) fn emit_typed_store(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
    value: ValueId,
) -> Result<(), EncodeError> {
    let offset = emit_access_base(body, function, layout, access)?;
    if access.scalar == ScalarKind::Unit {
        body.instruction(&wasm_encoder::Instruction::I32Const(0));
    } else {
        emit_get(body, layout, function.id, value)?;
    }
    let arg = memarg_at(access.width, offset);
    match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => body.instruction(&wasm_encoder::Instruction::I32Store8(arg)),
        ScalarKind::Tag16 => body.instruction(&wasm_encoder::Instruction::I32Store16(arg)),
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => body.instruction(&wasm_encoder::Instruction::I32Store(arg)),
        ScalarKind::Int | ScalarKind::Duration => body.instruction(&wasm_encoder::Instruction::I64Store(arg)),
        ScalarKind::Float => body.instruction(&wasm_encoder::Instruction::F64Store(arg)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "scalar store" }),
    };
    Ok(())
}

pub(super) fn emit_init_nullable_owned(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<(), EncodeError> {
    let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
    if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "InitNullableOwned" });
    }
    let offset = emit_access_base(body, function, layout, access)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg_at(4, offset)));
    Ok(())
}

pub(super) fn emit_clear_owned_field(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
) -> Result<(), EncodeError> {
    let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
    if access.pointer_slot != Some(PointerSlot::Owned)
        || place.access() != AccessMode::Write
        || !matches!(place.projection(), Projection::Field { .. })
    {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "TakeOwnedField" });
    }
    let offset = emit_access_base(body, function, layout, access)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg_at(4, offset)));
    Ok(())
}

pub(super) fn emit_clone_present(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    object: ValueId,
    function: &Function,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_clone_ref(body, layout, object, object, function)
}

pub(super) fn emit_alloc(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    layout_id: crate::layout::LayoutId,
    extent: &Extent,
) -> Result<(), EncodeError> {
    // The object's byte size lands in scratch 0: a counted collection
    // computes it from its capacity, a fixed layout (a record) is a constant.
    let collection = match extent {
        Extent::Counted { capacity, header_bytes, stride_bytes, .. } => {
            let collection = collection_layout(function.id, layouts, layout_id).map_err(EncodeError::Lowering)?;
            if *header_bytes != collection.data_offset || *stride_bytes != collection.stride {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" });
            }
            let maximum = (u32::MAX - collection.data_offset) / collection.stride;
            emit_u32_trap_if_gt(body, layout, function.id, *capacity, maximum)?;
            emit_get(body, layout, function.id, *capacity)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
            body.instruction(&wasm_encoder::Instruction::I64Mul);
            body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
            body.instruction(&wasm_encoder::Instruction::I64Add);
            body.instruction(&wasm_encoder::Instruction::I32WrapI64);
            emit_set_index(body, scratch_index(layout, 0));
            Some(collection)
        }
        Extent::FixedBytes(bytes) => {
            let bytes = fixed_alloc_bytes(*bytes)
                .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" })?;
            body.instruction(&wasm_encoder::Instruction::I32Const(bytes as i32));
            emit_set_index(body, scratch_index(layout, 0));
            None
        }
        Extent::HeaderPrefix { .. } => {
            return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc extent" });
        }
    };
    // Search reclaimed storage first. Scratch 2 is the current free-list
    // handle and scratch 3 is its predecessor. Reuse requires an exact
    // physical extent: shrinking the header of a larger block would lose its
    // suffix and corrupt the runtime's contiguous heap walk.
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    emit_set_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    emit_set_index(body, scratch_index(layout, 3));
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(0));
    emit_set_index(body, scratch_index(layout, 1));
    // The object's end in bytes (64-bit, so the sum cannot wrap). When it
    // lies past the current linear memory, grow by the pages needed; a
    // refused growth traps rather than writing past memory.
    let emit_end = |body: &mut wasm_encoder::Function| {
        emit_get_index(body, scratch_index(layout, 1));
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(RC_PREFIX_BYTES)));
        body.instruction(&wasm_encoder::Instruction::I64Add);
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
        body.instruction(&wasm_encoder::Instruction::I64Add);
    };
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::MemorySize(0));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_end(body);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES - 1)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(LINEAR_MEMORY_BYTES)));
    body.instruction(&wasm_encoder::Instruction::I64DivU);
    body.instruction(&wasm_encoder::Instruction::MemorySize(0));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Sub);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    body.instruction(&wasm_encoder::Instruction::MemoryGrow(0));
    body.instruction(&wasm_encoder::Instruction::I32Const(-1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(RC_PREFIX_BYTES as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::GlobalSet(0));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 3));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, 3));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(RC_PREFIX_BYTES as i32));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_set_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    emit_set_index(body, scratch_index(layout, 3));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, scratch_index(layout, 0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 2));
    emit_set(body, layout, function.id, destination)?;
    if let (Some(collection), Extent::Counted { count, capacity, .. }) = (collection, extent) {
        emit_get_index(body, scratch_index(layout, 2));
        body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_get(body, layout, function.id, *count)?;
        body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        if let Some(capacity_offset) = collection.capacity_offset {
            emit_get_index(body, scratch_index(layout, 2));
            body.instruction(&wasm_encoder::Instruction::I32Const(capacity_offset as i32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_get(body, layout, function.id, *capacity)?;
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        }
    }
    Ok(())
}

/// A fixed layout's allocation size, when it and the allocator prefix fit
/// the 32-bit address space.
pub(super) fn fixed_alloc_bytes(bytes: u64) -> Option<u32> {
    u32::try_from(bytes)
        .ok()
        .filter(|bytes| *bytes <= u32::MAX - RC_PREFIX_BYTES)
        // A reclaimed object stores its next-link in the dead payload. Keep
        // the physical cell wide enough even for zero-sized and compact
        // scalar layouts; SafeWIR still authorizes only the logical layout.
        .map(|bytes| bytes.max(4))
}

pub(super) fn emit_update_count(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    object: ValueId,
    count: ValueId,
) -> Result<(), EncodeError> {
    let collection = resolve_collection_layout(function, layouts, object).map_err(EncodeError::Lowering)?;
    if let Some(capacity_offset) = collection.capacity_offset {
        emit_get(body, layout, function.id, count)?;
        emit_get(body, layout, function.id, object)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(capacity_offset as i32));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::I32GtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(collection.length_offset as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    Ok(())
}

pub(super) fn emit_clone_ref(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    destination: ValueId,
    object: ValueId,
    function: &Function,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    if let Some(helpers) = &layout.memory_helpers {
        body.instruction(&wasm_encoder::Instruction::Call(helpers.retain));
        return emit_set(body, layout, function.id, destination);
    }
    emit_set_index(body, scratch_index(layout, 1));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(2));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, scratch_index(layout, 2));
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(-1));
    body.instruction(&wasm_encoder::Instruction::I32Eq);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_get_index(body, scratch_index(layout, 2));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, scratch_index(layout, 1));
    emit_set(body, layout, function.id, destination)?;
    Ok(())
}

pub(super) fn emit_reclaim(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    emit_set_index(body, scratch_index(layout, 1));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, 1));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
    Ok(())
}

pub(super) fn emit_reclaim_local(body: &mut wasm_encoder::Function, layout: &LocalLayout, slot: usize) {
    emit_get_index(body, scratch_index(layout, slot));
    body.instruction(&wasm_encoder::Instruction::GlobalGet(1));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_get_index(body, scratch_index(layout, slot));
    body.instruction(&wasm_encoder::Instruction::GlobalSet(1));
}

/// The owned pointer fields of a fixed layout as `(byte offset, pointee)`.
pub(super) fn fixed_pointer_fields(
    function: FunctionId,
    layouts: &LayoutInterner,
    layout: crate::layout::LayoutId,
) -> Result<Vec<(u32, crate::layout::LayoutId, bool)>, LoweringError> {
    let physical_layout = resolve_backend_layout(function, layouts, layout, "fixed pointer layout")?;
    let Some(descriptor) = layouts.get(physical_layout) else {
        return Err(LoweringError::UnsupportedLayout { function, operation: "fixed pointer layout" });
    };
    if !matches!(descriptor.kind(), LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. }) {
        return Ok(Vec::new());
    }
    descriptor
        .fields()
        .iter()
        .map(|field| match field.kind() {
            crate::layout::FieldKind::Pointer { pointee, slot } if matches!(slot, PointerSlot::Owned | PointerSlot::OwnedNullable) => {
                Ok(Some((field.offset(), pointee, slot == PointerSlot::OwnedNullable)))
            }
            crate::layout::FieldKind::Inline(slot) => {
                let physical_slot = resolve_backend_layout(function, layouts, slot, "fixed pointer field")?;
                match layouts.get(physical_slot).map(|slot| slot.kind()) {
                Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics })
                    if matches!(semantics, PointerSlot::Owned | PointerSlot::OwnedNullable) =>
                {
                    Ok(Some((field.offset(), *pointee, *semantics == PointerSlot::OwnedNullable)))
                }
                _ => Ok(None),
                }
            }
            _ => Ok(None),
        })
        .collect::<Result<Vec<_>, LoweringError>>()
        .map(|fields| fields.into_iter().flatten().collect())
}

pub(super) fn emit_release(body: &mut wasm_encoder::Function, layout: &LocalLayout, function: &Function, object: ValueId, layouts: &LayoutInterner) -> Result<(), EncodeError> {
    let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "DropRef object" });
    };
    emit_get(body, layout, function.id, object)?;
    if let Some(index) = layout.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&object_layout)) {
        body.instruction(&wasm_encoder::Instruction::Call(*index));
        return Ok(());
    }
    emit_set_index(body, scratch_index(layout, 0));
    drop_worklist::emit(body, layout, function.id, layouts, object_layout)
}

pub(super) fn emit_call_arguments(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    arguments: &[ValueId],
) -> Result<(), EncodeError> {
    for value in arguments {
        emit_get(body, layout, function, *value)?;
    }
    Ok(())
}

pub(super) fn emit_host_slice(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(), EncodeError> {
    let (place, access, with_length) = match argument {
        // A checked slice passes (ptr, len) in either direction; the place's
        // own access mode decides whether the range is bounded by the
        // object's count or by its capacity.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } => (place, place.access(), true),
        // A write buffer of a declared minimum size passes its address only.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) } => (place, AccessMode::Write, false),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer" })),
    };
    let Projection::Slice { length, .. } = place.projection() else {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function, operation: "HostCall buffer projection" }));
    };
    emit_host_scalar_slice_address(body, layout, function, value_types, layouts, place, access, *length)?;
    if with_length {
        emit_get(body, layout, function, *length)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_host_scalar_slice_address(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), EncodeError> {
    let (object, start, length, stride, data_offset) =
        host_scalar_slice_place(function, value_types, layouts, place, access, length)
            .map_err(EncodeError::Lowering)?;
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(data_offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, length)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(data_offset as i32));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(stride as i32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_raw_byte_slice_address(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    value_types: &BTreeMap<ValueId, ValueType>,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), EncodeError> {
    let (object, start, length) = raw_byte_slice_place(function, value_types, layouts, place, access, length)
        .map_err(EncodeError::Lowering)?;
    // Authenticate the complete physical range before narrowing it to the
    // two i32 ABI lanes. This is redundant with the SafeWIR checker by design:
    // the emitted host pointer must never be allowed to wrap independently.
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(4));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    emit_get(body, layout, function, length)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function, object)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get(body, layout, function, start)?;
    body.instruction(&wasm_encoder::Instruction::I32Add);
    Ok(())
}

pub(super) fn emit_copy_bytes(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    length: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    // Keep the checked addresses in locals so the small-copy and bulk paths
    // share the exact same range authentication. A fixed-width load followed
    // by one store also preserves memory.copy's overlap behavior.
    let destination_local = scratch_index(layout, 0);
    let source_local = scratch_index(layout, 1);
    let length_local = scratch_index(layout, 2);
    emit_raw_byte_slice_address(
        body,
        layout,
        function.id,
        &function.value_types,
        layouts,
        destination,
        AccessMode::Write,
        length,
    )?;
    body.instruction(&wasm_encoder::Instruction::LocalSet(destination_local));
    emit_raw_byte_slice_address(
        body,
        layout,
        function.id,
        &function.value_types,
        layouts,
        source,
        AccessMode::Read,
        length,
    )?;
    body.instruction(&wasm_encoder::Instruction::LocalSet(source_local));
    emit_get(body, layout, function.id, length)?;
    body.instruction(&wasm_encoder::Instruction::LocalSet(length_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32LeU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    for width in 1..=8 {
        body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
        body.instruction(&wasm_encoder::Instruction::I32Const(width));
        body.instruction(&wasm_encoder::Instruction::I32Eq);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        match width {
            1 => {
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                body.instruction(&wasm_encoder::Instruction::I32Load8U(memarg(1)));
                body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
            }
            2 => {
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                body.instruction(&wasm_encoder::Instruction::I32Load16U(memarg(2)));
                body.instruction(&wasm_encoder::Instruction::I32Store16(memarg(2)));
            }
            4 => {
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            }
            8 => {
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                body.instruction(&wasm_encoder::Instruction::I64Load(memarg(8)));
                body.instruction(&wasm_encoder::Instruction::I64Store(memarg(8)));
            }
            3 | 5 | 6 | 7 => {
                let chunk = if width == 3 { 2 } else { 4 };
                let offset = width - chunk;
                let first = scratch_index(layout, 3);
                let last = scratch_index(layout, 4);
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                if chunk == 2 {
                    body.instruction(&wasm_encoder::Instruction::I32Load16U(memarg(2)));
                } else {
                    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                }
                body.instruction(&wasm_encoder::Instruction::LocalSet(first));
                body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                if chunk == 2 {
                    body.instruction(&wasm_encoder::Instruction::I32Load16U(memarg(2)));
                } else {
                    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                }
                body.instruction(&wasm_encoder::Instruction::LocalSet(last));
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::LocalGet(first));
                if chunk == 2 {
                    body.instruction(&wasm_encoder::Instruction::I32Store16(memarg(2)));
                } else {
                    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
                }
                body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::LocalGet(last));
                if chunk == 2 {
                    body.instruction(&wasm_encoder::Instruction::I32Store16(memarg(2)));
                } else {
                    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
                }
            }
            _ => unreachable!(),
        }
        body.instruction(&wasm_encoder::Instruction::Else);
    }
    // Preserve memory.copy's zero-length endpoint check as well.
    body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    for _ in 1..=8 {
        body.instruction(&wasm_encoder::Instruction::End);
    }
    body.instruction(&wasm_encoder::Instruction::Else);
    // Inline medium copies only when the authenticated ranges are disjoint.
    // The bulk-memory fallback retains memmove behavior for overlap and for
    // sizes where a counted scalar loop would be counterproductive.
    let cursor = scratch_index(layout, 5);
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::I32Const(9));
    body.instruction(&wasm_encoder::Instruction::I32GeU);
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::I32Const(4096));
    body.instruction(&wasm_encoder::Instruction::I32LeU);
    body.instruction(&wasm_encoder::Instruction::I32And);
    body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
    body.instruction(&wasm_encoder::Instruction::I32LeU);
    body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
    body.instruction(&wasm_encoder::Instruction::I32LeU);
    body.instruction(&wasm_encoder::Instruction::I32Or);
    body.instruction(&wasm_encoder::Instruction::I32And);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::LocalSet(cursor));
    for width in [8, 1] {
        body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::LocalGet(cursor));
        body.instruction(&wasm_encoder::Instruction::I32Const(width));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
        body.instruction(&wasm_encoder::Instruction::I32GtU);
        body.instruction(&wasm_encoder::Instruction::BrIf(1));
        body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
        body.instruction(&wasm_encoder::Instruction::LocalGet(cursor));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
        body.instruction(&wasm_encoder::Instruction::LocalGet(cursor));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        if width == 8 {
            body.instruction(&wasm_encoder::Instruction::I64Load(memarg(8)));
            body.instruction(&wasm_encoder::Instruction::I64Store(memarg(8)));
        } else {
            body.instruction(&wasm_encoder::Instruction::I32Load8U(memarg(1)));
            body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
        }
        body.instruction(&wasm_encoder::Instruction::LocalGet(cursor));
        body.instruction(&wasm_encoder::Instruction::I32Const(width));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::LocalSet(cursor));
        body.instruction(&wasm_encoder::Instruction::Br(0));
        body.instruction(&wasm_encoder::Instruction::End);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    body.instruction(&wasm_encoder::Instruction::Else);
    body.instruction(&wasm_encoder::Instruction::LocalGet(destination_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(source_local));
    body.instruction(&wasm_encoder::Instruction::LocalGet(length_local));
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

pub(super) fn emit_move_range_base(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    object: ValueId,
    start: ValueId,
    collection: CollectionLayout,
    scratch: usize,
) -> Result<(), EncodeError> {
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, layout, function.id, object)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    emit_get(body, layout, function.id, start)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(collection.data_offset)));
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, scratch)));
    Ok(())
}

pub(super) fn emit_move_range_element_address(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    base_scratch: usize,
    index_scratch: usize,
    stride: u32,
) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, base_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, index_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, base_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, index_scratch)));
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Add);
    body.instruction(&wasm_encoder::Instruction::I32WrapI64);
}

pub(super) fn emit_move_range(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    count: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object()).map_err(EncodeError::Lowering)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object()).map_err(EncodeError::Lowering)?;
    if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || destination_layout != source_layout
    {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "MoveRange" }));
    }
    if destination_layout.pointer_layout.is_none() {
        // Scalar movement is logical ownership transfer. The checked source
        // becomes unreadable, so the backend may leave its physical bytes in
        // place and use the same overlap-safe copy sequence as CloneRange.
        return emit_clone_range(body, function, layout, destination, source, count, layouts);
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "MoveRange" })),
    };
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 3)));
    emit_move_range_base(body, function, layout, source.object(), source_start, source_layout, 1)?;
    emit_move_range_base(body, function, layout, destination.object(), destination_start, destination_layout, 2)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 3)));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Br(2));
    body.instruction(&wasm_encoder::Instruction::End);
    emit_move_range_element_address(body, layout, 2, 0, destination_layout.stride);
    emit_move_range_element_address(body, layout, 1, 0, source_layout.stride);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    emit_move_range_element_address(body, layout, 1, 0, source_layout.stride);
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::LocalSet(scratch_index(layout, 0)));
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

pub(super) fn emit_clone_range(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destination: &Place,
    source: &Place,
    count: ValueId,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object()).map_err(EncodeError::Lowering)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object()).map_err(EncodeError::Lowering)?;
    if destination_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || source_layout.pointer_slot == Some(PointerSlot::OwnedNullable)
        || destination_layout != source_layout
    {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" }));
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "CloneRange" })),
    };
    emit_move_range_base(body, function, layout, source.object(), source_start, source_layout, 1)?;
    emit_move_range_base(body, function, layout, destination.object(), destination_start, destination_layout, 2)?;
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(destination_layout.stride)));
    body.instruction(&wasm_encoder::Instruction::I64Mul);
    body.instruction(&wasm_encoder::Instruction::I64Const(i64::from(u32::MAX)));
    body.instruction(&wasm_encoder::Instruction::I64GtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    if destination_layout.pointer_layout.is_some() {
        // Retain every source element before publishing the copied prefix.
        // A zero or saturated child count may trap; until the whole loop
        // succeeds, the destination remains untouched.
        if layout.scratch.len() < 5 {
            return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "CloneRange scratch depth" });
        }
        body.instruction(&wasm_encoder::Instruction::I32Const(0));
        emit_set_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        emit_get_index(body, scratch_index(layout, 0));
        emit_get(body, layout, function.id, count)?;
        body.instruction(&wasm_encoder::Instruction::I32LtU);
        body.instruction(&wasm_encoder::Instruction::I32Eqz);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Br(2));
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 1));
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I32Const(destination_layout.stride as i32));
        body.instruction(&wasm_encoder::Instruction::I32Mul);
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        emit_set_index(body, scratch_index(layout, 3));
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::GlobalGet(2));
        body.instruction(&wasm_encoder::Instruction::I32LtU);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Else);
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::I32Const(8));
        body.instruction(&wasm_encoder::Instruction::I32Sub);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        emit_set_index(body, scratch_index(layout, 4));
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Eqz);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Const(-1));
        body.instruction(&wasm_encoder::Instruction::I32Eq);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 3));
        body.instruction(&wasm_encoder::Instruction::I32Const(8));
        body.instruction(&wasm_encoder::Instruction::I32Sub);
        emit_get_index(body, scratch_index(layout, 4));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_set_index(body, scratch_index(layout, 0));
        body.instruction(&wasm_encoder::Instruction::Br(0));
        body.instruction(&wasm_encoder::Instruction::End);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 2)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(scratch_index(layout, 1)));
    emit_get(body, layout, function.id, count)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(destination_layout.stride as i32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    Ok(())
}

pub(super) fn emit_host_call(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[HostCallArgument],
    contracts: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    layouts: &LayoutInterner,
) -> Result<(), EncodeError> {
    let contract = contracts.get(&import).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
        function: function.id,
        operation: "HostCall contract",
    }))?;
    for (role, argument) in contract.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(_), HostCallArgument::Value(value))
            | (HostParameterRole::Capability(_), HostCallArgument::Capability(value)) => {
                emit_get(body, layout, function.id, *value)?;
            }
            (HostParameterRole::Buffer { .. }, HostCallArgument::Buffer { .. }) => {
                emit_host_slice(body, layout, function.id, &function.value_types, layouts, argument)?;
            }
            _ => return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "HostCall argument" })),
        }
    }
    let index = import_indices.get(&import).copied().ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
        function: function.id,
        operation: "HostCall import",
    }))?;
    body.instruction(&wasm_encoder::Instruction::Call(index));
    for destination in destinations.iter().rev() {
        emit_set(body, layout, function.id, *destination)?;
    }
    Ok(())
}

pub(super) fn emit_external_call(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[ValueId],
    import_indices: &BTreeMap<ImportId, u32>,
) -> Result<(), EncodeError> {
    for argument in arguments {
        if function.value_types.get(argument) != Some(&ValueType::Unit) {
            emit_get(body, layout, function.id, *argument)?;
        }
    }
    let index = import_indices.get(&import).copied().ok_or(
        EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id,
            operation: "ExternalCall import",
        }),
    )?;
    body.instruction(&wasm_encoder::Instruction::Call(index));
    // Only non-unit results arrived on the Wasm operand stack.  Materialize
    // source unit destinations locally after draining them, preserving the
    // source-level value map without exporting a physical unit lane.
    for destination in destinations.iter().rev() {
        if function.value_types.get(destination) != Some(&ValueType::Unit) {
            emit_set(body, layout, function.id, *destination)?;
        }
    }
    for destination in destinations {
        if function.value_types.get(destination) == Some(&ValueType::Unit) {
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_set(body, layout, function.id, *destination)?;
        }
    }
    Ok(())
}

pub(super) fn emit_results(
    body: &mut wasm_encoder::Function,
    layout: &LocalLayout,
    function: FunctionId,
    destinations: &[ValueId],
) -> Result<(), EncodeError> {
    for value in destinations.iter().rev() {
        emit_set(body, layout, function, *value)?;
    }
    Ok(())
}

pub(super) fn emit_instruction(
    body: &mut wasm_encoder::Function,
    function: &Function,
    layout: &LocalLayout,
    instruction: &ScalarInstruction,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    import_indices: &BTreeMap<ImportId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    static_data: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    layouts: &LayoutInterner,
    owner_cleanup_function: Option<u32>,
) -> Result<(), EncodeError> {
    match instruction {
        ScalarInstruction::ProofNoop => {}
        ScalarInstruction::ClosureOwner(operation) => { owner_operation_kernel::emit(body, function, layout, layouts, operation, import_indices, owner_cleanup_function)?; }
        ScalarInstruction::Dictionary(operation) => { dictionary_kernel::emit(body, function, layout, layouts, operation)?; }
        ScalarInstruction::UnitConst { destination } => { body.instruction(&wasm_encoder::Instruction::I32Const(0)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::IntConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I64Const(*value)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::FloatConst { destination, bits } => { body.instruction(&wasm_encoder::Instruction::F64Const(Ieee64::new(*bits))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::BoolConst { destination, value } => { body.instruction(&wasm_encoder::Instruction::I32Const(i32::from(*value))); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::SizeConst { destination, value } => {
            let value = u32::try_from(*value).map_err(|_| EncodeError::UnsupportedValueType { function: function.id, value: *destination, value_type: crate::canonical_safewir::ValueType::Size })?;
            body.instruction(&wasm_encoder::Instruction::I32Const(value as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::CapabilityNone { destination } => { body.instruction(&wasm_encoder::Instruction::RefNull(wasm_encoder::HeapType::EXTERN)); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilitySome { destination, source } => { emit_get(body, layout, function.id, *source)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilityIsPresent { destination, source } => { emit_get(body, layout, function.id, *source)?; body.instruction(&wasm_encoder::Instruction::RefIsNull); body.instruction(&wasm_encoder::Instruction::I32Eqz); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::CapabilityUnwrap { destination, source } => { emit_get(body, layout, function.id, *source)?; body.instruction(&wasm_encoder::Instruction::RefAsNonNull); emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::Arithmetic { destination, operator, left, right, overflow } => {
            emit_checked_arithmetic(
                body,
                function,
                layout,
                *destination,
                *operator,
                *left,
                *right,
                *overflow,
            )?;
        }
        ScalarInstruction::Compare { destination, operator, left, right } => { emit_compare(body, function, layout, *destination, *operator, *left, *right)?; }
        ScalarInstruction::NarrowSize { destination, value } => emit_narrow_size(body, function, layout, *destination, *value)?,
        ScalarInstruction::WidenSize { destination, value } => {
            emit_get(body, layout, function.id, *value)?;
            body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::FloatUnary { destination, operator, value } => {
            emit_get(body, layout, function.id, *value)?;
            body.instruction(&match operator {
                crate::canonical_safewir::FloatUnaryOperator::Sqrt => wasm_encoder::Instruction::F64Sqrt,
                crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => wasm_encoder::Instruction::I64TruncSatF64S,
                crate::canonical_safewir::FloatUnaryOperator::FromInt => wasm_encoder::Instruction::F64ConvertI64S,
            });
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::Guard { condition, .. } => { emit_get(body, layout, function.id, *condition)?; body.instruction(&wasm_encoder::Instruction::I32Eqz); body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty)); body.instruction(&wasm_encoder::Instruction::Unreachable); body.instruction(&wasm_encoder::Instruction::End); }
        ScalarInstruction::MoveValue { destination, source } => { emit_get(body, layout, function.id, *source)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::MakeInlineAggregate { .. } => {}
        ScalarInstruction::ExtractInlineField { destination, field, .. } => { emit_get(body, layout, function.id, *field)?; emit_set(body, layout, function.id, *destination)?; }
        ScalarInstruction::FunctionValue { destination, table_index, .. } => {
            body.instruction(&wasm_encoder::Instruction::I32Const(*table_index as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::Alloc { destination, layout: layout_id, extent, class } => {
            if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Alloc class" });
            }
            emit_alloc(body, function, layout, layouts, *destination, *layout_id, extent)?;
        }
        ScalarInstruction::StaticData { destination, data } => {
            let offset = static_data.get(data).copied().ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id,
                operation: "StaticData",
            }))?;
            body.instruction(&wasm_encoder::Instruction::I32Const(offset as i32));
            emit_set(body, layout, function.id, *destination)?;
        }
        ScalarInstruction::InitNullableOwned { place } => {
            emit_init_nullable_owned(body, function, layout, layouts, place)?;
        }
        ScalarInstruction::Load { destination, place, transfer } => {
            if is_raw_allocation_capacity_load(function.id, &function.value_types, layouts, place) {
                emit_get(body, layout, function.id, place.object())?;
                body.instruction(&wasm_encoder::Instruction::I32Const(4));
                body.instruction(&wasm_encoder::Instruction::I32Sub);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                body.instruction(&wasm_encoder::Instruction::I32Const(4));
                body.instruction(&wasm_encoder::Instruction::I32Sub);
                emit_set(body, layout, function.id, *destination)?;
                return Ok(());
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, layout, access, *destination)?;
            match transfer {
                LoadTransfer::BorrowAffine => {
                    emit_get(body, layout, function.id, *destination)?;
                    body.instruction(&wasm_encoder::Instruction::I32Eqz);
                    body.instruction(&wasm_encoder::Instruction::If(wasm_encoder::BlockType::Empty));
                    body.instruction(&wasm_encoder::Instruction::Unreachable);
                    body.instruction(&wasm_encoder::Instruction::End);
                },
                LoadTransfer::CloneShared => emit_clone_ref(body, layout, *destination, *destination, function)?,
                LoadTransfer::ClonePresent => emit_clone_present(body, layout, *destination, function)?,
                LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField => {
                    emit_get(body, layout, function.id, *destination)?;
                    body.instruction(&wasm_encoder::Instruction::I32Eqz);
                    body.instruction(&wasm_encoder::Instruction::If(wasm_encoder::BlockType::Empty));
                    body.instruction(&wasm_encoder::Instruction::Unreachable);
                    body.instruction(&wasm_encoder::Instruction::End);
                    let writable = crate::canonical_safewir::Place::projected(
                        place.object(), place.projection().clone(), place.value_type(),
                        crate::canonical_safewir::AccessMode::Write,
                    );
                    if *transfer == LoadTransfer::TakeOwnedField {
                        emit_clear_owned_field(body, function, layout, layouts, &writable)?;
                    } else {
                        emit_init_nullable_owned(body, function, layout, layouts, &writable)?;
                    }
                }
                LoadTransfer::TakeLast => {
                    let Projection::Element { index } = place.projection() else { return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "TakeLast" }); };
                    emit_update_count(body, function, layout, layouts, place.object(), *index)?;
                }
                LoadTransfer::MoveUnique | LoadTransfer::CopyScalar => {}
            }
        }
        ScalarInstruction::Store { place, value, transfer } => {
            if !matches!(*transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique | StoreTransfer::CloneShared) {
                return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Store transfer" });
            }
            let access = resolve_memory_place(function, layouts, place).map_err(EncodeError::Lowering)?;
            if *transfer == StoreTransfer::CloneShared {
                // Retain before publishing the second edge. Assigning the
                // source local to itself preserves its existing owned token.
                emit_clone_ref(body, layout, *value, *value, function)?;
            }
            emit_typed_store(body, function, layout, access, *value)?;
        }
        ScalarInstruction::CopyBytes { destination, source, length } => {
            emit_copy_bytes(body, function, layout, destination, source, *length, layouts)?;
        }
        ScalarInstruction::MoveRange { destination, source, count } => {
            emit_move_range(body, function, layout, destination, source, *count, layouts)?;
        }
        ScalarInstruction::CloneRange { destination, source, count } => {
            emit_clone_range(body, function, layout, destination, source, *count, layouts)?;
        }
        ScalarInstruction::UpdateCount { object, count } => emit_update_count(body, function, layout, layouts, *object, *count)?,
        ScalarInstruction::CloneRef { destination, source } => emit_clone_ref(body, layout, *destination, *source, function)?,
        ScalarInstruction::Release { object } => emit_release(body, layout, function, *object, layouts)?,
        ScalarInstruction::Reclaim { object } => emit_reclaim(body, layout, function, *object)?,
        ScalarInstruction::Call { destinations, callee, arguments } => {
            emit_call_arguments(body, layout, function.id, arguments)?;
            let index = function_indices.get(callee).copied().ok_or(EncodeError::MissingFunction(*callee))?;
            body.instruction(&wasm_encoder::Instruction::Call(index));
            emit_results(body, layout, function.id, destinations)?;
        }
        ScalarInstruction::ExternalCall { destinations, import, arguments } => {
            emit_external_call(body, function, layout, destinations, *import, arguments, import_indices)?;
        }
        ScalarInstruction::HostCall { destinations, import, arguments } => emit_host_call(body, function, layout, destinations, *import, arguments, imports, import_indices, layouts)?,
        ScalarInstruction::VarCall { destinations, callee, signature, arguments } => {
            emit_call_arguments(body, layout, function.id, arguments)?;
            emit_get(body, layout, function.id, *callee)?;
            let type_index = signature_indices.get(signature).copied().ok_or(EncodeError::UnsupportedSignature { signature: *signature })?;
            body.instruction(&wasm_encoder::Instruction::CallIndirect { type_index, table_index: 0 });
            emit_results(body, layout, function.id, destinations)?;
        }
    }
    Ok(())
}
