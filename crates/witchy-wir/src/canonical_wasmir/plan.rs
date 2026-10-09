use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum PlanToken {
    Drop,
    RefCastNonNull(u32),
    StructGet { struct_type_index: u32, field_index: u32 },
    ArrayLen,
    ArrayGet(u32),
    LocalGet(u32),
    LocalSet(u32),
    RefNull(String),
    RefIsNull,
    RefAsNonNull,
    LocalTee(u32),
    I32Const(i32),
    I64Const(i64),
    I64ExtendI32U,
    F64Sqrt,
    I64TruncSatF64S,
    F64ConvertI64S,
    I64Mul,
    I64Add,
    I64Sub,
    MemorySize,
    MemoryGrow,
    MemoryFill(u32),
    I32WrapI64,
    I64Eq,
    I64LtS,
    I64GtS,
    GlobalGet(u32),
    GlobalSet(u32),
    I32Add,
    I32Mul,
    I32Sub,
    I32LtU,
    I32LeU,
    I32GtU,
    I32Eqz,
    I32Eq,
    I32Load { offset: u64, align: u8, memory: u32 },
    I32Load8U { offset: u64, align: u8, memory: u32 },
    I32Load16U { offset: u64, align: u8, memory: u32 },
    I64Load { offset: u64, align: u8, memory: u32 },
    V128Load { offset: u64, align: u8, memory: u32 },
    I8x16Splat,
    I8x16Eq,
    I8x16Bitmask,
    I32Ctz,
    F64Load { offset: u64, align: u8, memory: u32 },
    I32Store { offset: u64, align: u8, memory: u32 },
    I32Store8 { offset: u64, align: u8, memory: u32 },
    I32Store16 { offset: u64, align: u8, memory: u32 },
    I64Store { offset: u64, align: u8, memory: u32 },
    F64Store { offset: u64, align: u8, memory: u32 },
    MemoryCopy { source: u32, destination: u32 },
    I64GtU,
    I64Ne,
    I64LeS,
    I64GeS,
    I64LtU,
    I64LeU,
    I64GeU,
    I32Ne,
    I32LtS,
    I32LeS,
    I32GtS,
    I32GeS,
    I32GeU,
    F64Eq,
    F64Ne,
    F64Lt,
    F64Le,
    F64Gt,
    F64Ge,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Const(u64),
    Block(String),
    Loop(String),
    If(String),
    Else,
    Br(u32),
    BrIf(u32),
    Unreachable,
    Return,
    Call(u32),
    CallIndirect { type_index: u32, table_index: u32 },
    ReturnCall(u32),
    ReturnCallIndirect { type_index: u32, table_index: u32 },
    End,
}

pub(super) fn decode_plan_token(operator: wasmparser::Operator<'_>) -> Result<PlanToken, CanonicalAdmissionError> {
    Ok(match operator {
        wasmparser::Operator::Drop => PlanToken::Drop,
        wasmparser::Operator::LocalGet { local_index } => PlanToken::LocalGet(local_index),
        wasmparser::Operator::LocalSet { local_index } => PlanToken::LocalSet(local_index),
        wasmparser::Operator::RefNull { hty } => PlanToken::RefNull(format!("{hty:?}")),
wasmparser::Operator::RefCastNonNull { hty: wasmparser::HeapType::Concrete(index) } => PlanToken::RefCastNonNull(index.as_module_index().ok_or(CanonicalAdmissionError::UnsupportedPlan)?),
wasmparser::Operator::StructGet { struct_type_index, field_index } => PlanToken::StructGet { struct_type_index, field_index },
wasmparser::Operator::ArrayLen => PlanToken::ArrayLen,
wasmparser::Operator::ArrayGet { array_type_index } => PlanToken::ArrayGet(array_type_index),
wasmparser::Operator::RefIsNull => PlanToken::RefIsNull,

        wasmparser::Operator::RefAsNonNull => PlanToken::RefAsNonNull,
        wasmparser::Operator::I32Const { value } => PlanToken::I32Const(value),
        wasmparser::Operator::I64Const { value } => PlanToken::I64Const(value),
        wasmparser::Operator::I64ExtendI32U => PlanToken::I64ExtendI32U,
        wasmparser::Operator::F64Sqrt => PlanToken::F64Sqrt,
        wasmparser::Operator::I64TruncSatF64S => PlanToken::I64TruncSatF64S,
        wasmparser::Operator::F64ConvertI64S => PlanToken::F64ConvertI64S,
        wasmparser::Operator::I64Mul => PlanToken::I64Mul,
        wasmparser::Operator::I64Add => PlanToken::I64Add,
        wasmparser::Operator::I64Sub => PlanToken::I64Sub,
        wasmparser::Operator::MemorySize { mem: 0 } => PlanToken::MemorySize,
        wasmparser::Operator::MemoryGrow { mem: 0 } => PlanToken::MemoryGrow,
        wasmparser::Operator::MemoryFill { mem } => PlanToken::MemoryFill(mem),
        wasmparser::Operator::LocalTee { local_index } => PlanToken::LocalTee(local_index),
        wasmparser::Operator::I32WrapI64 => PlanToken::I32WrapI64,
        wasmparser::Operator::I64Eq => PlanToken::I64Eq,
        wasmparser::Operator::I64LtS => PlanToken::I64LtS,
        wasmparser::Operator::I64GtS => PlanToken::I64GtS,
        wasmparser::Operator::GlobalGet { global_index } => PlanToken::GlobalGet(global_index),
        wasmparser::Operator::GlobalSet { global_index } => PlanToken::GlobalSet(global_index),
        wasmparser::Operator::I32Add => PlanToken::I32Add,
        wasmparser::Operator::I32Mul => PlanToken::I32Mul,
        wasmparser::Operator::I32Sub => PlanToken::I32Sub,
        wasmparser::Operator::I32LtU => PlanToken::I32LtU,
        wasmparser::Operator::I32LeU => PlanToken::I32LeU,
        wasmparser::Operator::I32GtU => PlanToken::I32GtU,
        wasmparser::Operator::I32Eqz => PlanToken::I32Eqz,
        wasmparser::Operator::I32Eq => PlanToken::I32Eq,
        wasmparser::Operator::I32Load { memarg } => PlanToken::I32Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Load8U { memarg } => PlanToken::I32Load8U { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Load16U { memarg } => PlanToken::I32Load16U { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I64Load { memarg } => PlanToken::I64Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::V128Load { memarg } => PlanToken::V128Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I8x16Splat => PlanToken::I8x16Splat,
        wasmparser::Operator::I8x16Eq => PlanToken::I8x16Eq,
        wasmparser::Operator::I8x16Bitmask => PlanToken::I8x16Bitmask,
        wasmparser::Operator::I32Ctz => PlanToken::I32Ctz,
        wasmparser::Operator::F64Load { memarg } => PlanToken::F64Load { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Store { memarg } => PlanToken::I32Store {
            offset: memarg.offset,
            align: memarg.align,
            memory: memarg.memory,
        },
        wasmparser::Operator::I32Store8 { memarg } => PlanToken::I32Store8 { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I32Store16 { memarg } => PlanToken::I32Store16 { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::I64Store { memarg } => PlanToken::I64Store { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::F64Store { memarg } => PlanToken::F64Store { offset: memarg.offset, align: memarg.align, memory: memarg.memory },
        wasmparser::Operator::MemoryCopy { src_mem, dst_mem } => PlanToken::MemoryCopy {
            source: src_mem,
            destination: dst_mem,
        },
        wasmparser::Operator::I64GtU => PlanToken::I64GtU,
        wasmparser::Operator::I64Ne => PlanToken::I64Ne,
        wasmparser::Operator::I64LeS => PlanToken::I64LeS,
        wasmparser::Operator::I64GeS => PlanToken::I64GeS,
        wasmparser::Operator::I64LtU => PlanToken::I64LtU,
        wasmparser::Operator::I64LeU => PlanToken::I64LeU,
        wasmparser::Operator::I64GeU => PlanToken::I64GeU,
        wasmparser::Operator::I32Ne => PlanToken::I32Ne,
        wasmparser::Operator::I32LtS => PlanToken::I32LtS,
        wasmparser::Operator::I32LeS => PlanToken::I32LeS,
        wasmparser::Operator::I32GtS => PlanToken::I32GtS,
        wasmparser::Operator::I32GeS => PlanToken::I32GeS,
        wasmparser::Operator::I32GeU => PlanToken::I32GeU,
        wasmparser::Operator::F64Eq => PlanToken::F64Eq,
        wasmparser::Operator::F64Ne => PlanToken::F64Ne,
        wasmparser::Operator::F64Lt => PlanToken::F64Lt,
        wasmparser::Operator::F64Le => PlanToken::F64Le,
        wasmparser::Operator::F64Gt => PlanToken::F64Gt,
        wasmparser::Operator::F64Ge => PlanToken::F64Ge,
        wasmparser::Operator::I64DivS => PlanToken::I64DivS,
        wasmparser::Operator::I64DivU => PlanToken::I64DivU,
        wasmparser::Operator::I64RemS => PlanToken::I64RemS,
        wasmparser::Operator::I64RemU => PlanToken::I64RemU,
        wasmparser::Operator::I64And => PlanToken::I64And,
        wasmparser::Operator::I64Or => PlanToken::I64Or,
        wasmparser::Operator::I64Xor => PlanToken::I64Xor,
        wasmparser::Operator::I64Shl => PlanToken::I64Shl,
        wasmparser::Operator::I64ShrS => PlanToken::I64ShrS,
        wasmparser::Operator::I64ShrU => PlanToken::I64ShrU,
        wasmparser::Operator::I32DivS => PlanToken::I32DivS,
        wasmparser::Operator::I32DivU => PlanToken::I32DivU,
        wasmparser::Operator::I32RemS => PlanToken::I32RemS,
        wasmparser::Operator::I32RemU => PlanToken::I32RemU,
        wasmparser::Operator::I32And => PlanToken::I32And,
        wasmparser::Operator::I32Or => PlanToken::I32Or,
        wasmparser::Operator::I32Xor => PlanToken::I32Xor,
        wasmparser::Operator::I32Shl => PlanToken::I32Shl,
        wasmparser::Operator::I32ShrS => PlanToken::I32ShrS,
        wasmparser::Operator::I32ShrU => PlanToken::I32ShrU,
        wasmparser::Operator::F64Add => PlanToken::F64Add,
        wasmparser::Operator::F64Sub => PlanToken::F64Sub,
        wasmparser::Operator::F64Mul => PlanToken::F64Mul,
        wasmparser::Operator::F64Div => PlanToken::F64Div,
        wasmparser::Operator::F64Const { value } => PlanToken::F64Const(value.bits()),
        wasmparser::Operator::Block { blockty } => PlanToken::Block(format!("{blockty:?}")),
        wasmparser::Operator::Loop { blockty } => PlanToken::Loop(format!("{blockty:?}")),
        wasmparser::Operator::If { blockty } => PlanToken::If(format!("{blockty:?}")),
        wasmparser::Operator::Else => PlanToken::Else,
        wasmparser::Operator::Br { relative_depth } => PlanToken::Br(relative_depth),
        wasmparser::Operator::BrIf { relative_depth } => PlanToken::BrIf(relative_depth),
        wasmparser::Operator::Unreachable => PlanToken::Unreachable,
        wasmparser::Operator::Return => PlanToken::Return,
        wasmparser::Operator::Call { function_index } => PlanToken::Call(function_index),
        wasmparser::Operator::CallIndirect { type_index, table_index } => PlanToken::CallIndirect { type_index, table_index },
        wasmparser::Operator::ReturnCall { function_index } => PlanToken::ReturnCall(function_index),
        wasmparser::Operator::ReturnCallIndirect { type_index, table_index } => {
            PlanToken::ReturnCallIndirect { type_index, table_index }
        }
        wasmparser::Operator::End => PlanToken::End,
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

pub(super) fn plan_local(layout: &LocalLayout, value: ValueId) -> Result<u32, CanonicalAdmissionError> {
    layout.indices.get(&value).copied().ok_or(CanonicalAdmissionError::UnsupportedPlan)
}

pub(super) fn push_get(plan: &mut Vec<PlanToken>, layout: &LocalLayout, value: ValueId) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalGet(plan_local(layout, value)?));
    Ok(())
}

pub(super) fn push_set(plan: &mut Vec<PlanToken>, layout: &LocalLayout, value: ValueId) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalSet(plan_local(layout, value)?));
    Ok(())
}

pub(super) fn plan_call_argument(argument: &CallArgument) -> Result<ValueId, CanonicalAdmissionError> {
    match argument {
        CallArgument::Value(value) | CallArgument::Move(value) => Ok(*value),
    }
}

pub(super) fn push_store(plan: &mut Vec<PlanToken>) {
    plan.push(PlanToken::I32Store { offset: 0, align: 2, memory: 0 });
}

pub(super) fn push_u32_trap_if_gt_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    value: ValueId,
    limit: u32,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, value)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(limit)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    Ok(())
}

pub(super) fn push_projected_address_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<(), CanonicalAdmissionError> {
    if layout.refs == super::ref_encoding::RefEncoding::Scaled8 {
        return push_scaled_projected_address_plan(plan, layout, access);
    }
    if let Some(index) = access.index {
        let maximum = (u32::MAX
            .checked_sub(access.offset)
            .and_then(|value| value.checked_sub(access.width))
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?
            ) / access.collection.stride;
        if !access.indexed_address_fits_u64() {
            push_u32_trap_if_gt_plan(plan, layout, index, maximum)?;
        }
        push_get(plan, layout, access.object)?;
        plan.push(PlanToken::I64ExtendI32U);
        push_get(plan, layout, index)?;
        plan.extend([
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(access.collection.stride)),
            PlanToken::I64Mul,
            PlanToken::I64Const(i64::from(access.offset)),
            PlanToken::I64Add,
            PlanToken::I64Add,
        ]);
        push_get(plan, layout, access.object)?;
        plan.push(PlanToken::I64ExtendI32U);
        push_get(plan, layout, index)?;
        plan.extend([
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(access.collection.stride)),
            PlanToken::I64Mul,
            PlanToken::I64Const(i64::from(access.offset)),
            PlanToken::I64Add,
            PlanToken::I64Add,
            PlanToken::I64Const(i64::from(u32::MAX - access.width)),
            PlanToken::I64GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::I32WrapI64,
            PlanToken::I64ExtendI32U,
        ]);
        return Ok(());
    }
    let maximum = u32::MAX.checked_sub(access.width).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    if let Some(helpers) = &layout.memory_helpers {
        push_get(plan, layout, access.object)?;
        plan.extend([PlanToken::I32Const(access.offset as i32), PlanToken::I32Const(maximum as i32), PlanToken::Call(helpers.address), PlanToken::I64ExtendI32U]);
        return Ok(());
    }
    push_get(plan, layout, access.object)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(access.offset)),
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(maximum)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, access.object)?;
    plan.extend([PlanToken::I32Const(access.offset as i32), PlanToken::I32Add, PlanToken::I64ExtendI32U]);
    let _ = function;
    Ok(())
}

/// Mirror of `emit_scaled_projected_address`.
fn push_scaled_projected_address_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, access.object)?;
    layout.refs.plan_base(plan);
    if let Some(index) = access.index {
        let maximum = (u32::MAX
            .checked_sub(access.offset)
            .and_then(|value| value.checked_sub(access.width))
            .ok_or(CanonicalAdmissionError::UnsupportedPlan)?
            ) / access.collection.stride;
        push_u32_trap_if_gt_plan(plan, layout, index, maximum)?;
        push_get(plan, layout, index)?;
        plan.extend([
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(access.collection.stride)),
            PlanToken::I64Mul,
            PlanToken::I64Add,
        ]);
    }
    plan.extend([PlanToken::I64Const(i64::from(access.offset)), PlanToken::I64Add]);
    Ok(())
}

/// The plan of `emit_access_base`: the base, plus `index * stride` in i32 for
/// an element, with the offset left for the immediate.
pub(super) fn push_access_base_plan(
    plan: &mut Vec<PlanToken>,
    _function: &Function,
    layout: &LocalLayout,
    access: MemoryAccess,
) -> Result<u64, CanonicalAdmissionError> {
    push_get(plan, layout, access.object)?;
    layout.refs.plan_base(plan);
    if let Some(index) = access.index {
        push_get(plan, layout, index)?;
        if access.collection.stride != 1 {
            plan.extend([PlanToken::I32Const(access.collection.stride as i32), PlanToken::I32Mul]);
        }
        layout.refs.plan_add(plan);
    }
    layout.refs.plan_finish(plan);
    Ok(u64::from(access.offset))
}

pub(super) fn push_memory_load_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
    destination: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let offset = push_access_base_plan(plan, function, layout, access)?;
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => PlanToken::I32Load8U { offset, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Load16U { offset, align: 1, memory: 0 },
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => PlanToken::I32Load { offset, align: 2, memory: 0 },
        ScalarKind::Int | ScalarKind::Duration => PlanToken::I64Load { offset, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Load { offset, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    if access.scalar == ScalarKind::Unit {
        plan.extend([PlanToken::Drop, PlanToken::I32Const(0)]);
    }
    push_set(plan, layout, destination)
}

pub(super) fn push_init_nullable_owned_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if access.pointer_slot != Some(PointerSlot::OwnedNullable) || place.access() != AccessMode::Write {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let offset = push_access_base_plan(plan, function, layout, access)?;
    plan.extend([PlanToken::I32Const(0), PlanToken::I32Store { offset, align: 2, memory: 0 }]);
    Ok(())
}

pub(super) fn push_clear_owned_field_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if access.pointer_slot != Some(PointerSlot::Owned)
        || place.access() != AccessMode::Write
        || !matches!(place.projection(), Projection::Field { .. })
    {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let offset = push_access_base_plan(plan, function, layout, access)?;
    plan.extend([PlanToken::I32Const(0), PlanToken::I32Store { offset, align: 2, memory: 0 }]);
    Ok(())
}

pub(super) fn push_require_present_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    value: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, value)?;
    plan.extend([
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    Ok(())
}

pub(super) fn push_memory_store_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    place: &Place,
    value: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let access = resolve_memory_place_for_values(function.id, &function.value_types, layouts, place)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let offset = push_access_base_plan(plan, function, layout, access)?;
    if access.scalar == ScalarKind::Unit {
        plan.push(PlanToken::I32Const(0));
    } else {
        push_get(plan, layout, value)?;
    }
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte => PlanToken::I32Store8 { offset, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Store16 { offset, align: 1, memory: 0 },
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable => PlanToken::I32Store { offset, align: 2, memory: 0 },
        ScalarKind::Int | ScalarKind::Duration => PlanToken::I64Store { offset, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Store { offset, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    Ok(())
}

pub(super) fn push_share_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    destination: ValueId,
    object: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, object)?;
    if let Some(helpers) = &layout.memory_helpers {
        plan.push(PlanToken::Call(helpers.retain));
        return push_set(plan, layout, destination);
    }
    plan.extend([
        PlanToken::LocalSet(layout.scratch[1]),
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::GlobalGet(2),
        PlanToken::I32LtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Else,
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
    layout.refs.plan_base(plan);
    plan.push(PlanToken::I32Const(8));
    layout.refs.plan_sub(plan);
    layout.refs.plan_finish(plan);
    plan.extend([
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalSet(layout.scratch[2]),
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(-1),
        PlanToken::I32Eq,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
    layout.refs.plan_base(plan);
    plan.push(PlanToken::I32Const(8));
    layout.refs.plan_sub(plan);
    layout.refs.plan_finish(plan);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::I32Const(1),
        PlanToken::I32Add,
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::End,
        PlanToken::LocalGet(layout.scratch[1]),
    ]);
    push_set(plan, layout, destination)
}

pub(super) fn push_update_count_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    object: ValueId,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let collection = resolve_collection_layout_for_values(function.id, &function.value_types, layouts, object)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if let Some(capacity_offset) = collection.capacity_offset {
        push_get(plan, layout, count)?;
        push_get(plan, layout, object)?;
        layout.refs.plan_base(plan);
        plan.push(PlanToken::I32Const(capacity_offset as i32));
        layout.refs.plan_add(plan);
        layout.refs.plan_finish(plan);
        plan.extend([
            PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
            PlanToken::I32GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
        ]);
    }
    push_get(plan, layout, object)?;
    layout.refs.plan_base(plan);
    plan.push(PlanToken::I32Const(collection.length_offset as i32));
    layout.refs.plan_add(plan);
    layout.refs.plan_finish(plan);
    push_get(plan, layout, count)?;
    push_store(plan);
    Ok(())
}

pub(super) fn push_host_slice_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    argument: &HostCallArgument,
) -> Result<(), CanonicalAdmissionError> {
    let (place, access, with_length) = match argument {
        // A checked slice passes (ptr, len) in either direction; the place's
        // own access mode decides whether the range is bounded by the
        // object's count or by its capacity.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::Slice } => (place, place.access(), true),
        // A write buffer of a declared minimum size passes its address only.
        HostCallArgument::Buffer { place, extent: HostBufferExtent::AtLeast(_) } => (place, AccessMode::Write, false),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    let Projection::Slice { length, .. } = place.projection() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    push_host_scalar_slice_address_plan(plan, function, layout, layouts, place, access, *length)?;
    if with_length {
        push_get(plan, layout, *length)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_host_scalar_slice_address_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let (object, start, length, stride, data_offset) = host_scalar_slice_place(
        function.id, &function.value_types, layouts, place, access, length,
    ).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if layout.refs == ref_encoding::RefEncoding::Scaled8 {
        push_get(plan, layout, object)?;
        layout.refs.plan_object(plan);
        plan.extend([PlanToken::I64Const(i64::from(data_offset)), PlanToken::I64Add]);
        push_get(plan, layout, start)?;
        plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(i64::from(stride)), PlanToken::I64Mul, PlanToken::I64Add]);
        let _ = length;
        return Ok(());
    }
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(i64::from(data_offset)), PlanToken::I64Add]);
    push_get(plan, layout, start)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(i64::from(stride)), PlanToken::I64Mul, PlanToken::I64Add]);
    push_get(plan, layout, length)?;
    plan.extend([
        PlanToken::I64ExtendI32U, PlanToken::I64Const(i64::from(stride)), PlanToken::I64Mul,
        PlanToken::I64Add, PlanToken::I64Const(i64::from(u32::MAX)), PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()), PlanToken::Unreachable, PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I32Const(data_offset as i32), PlanToken::I32Add]);
    push_get(plan, layout, start)?;
    plan.extend([PlanToken::I32Const(stride as i32), PlanToken::I32Mul, PlanToken::I32Add]);
    Ok(())
}

pub(super) fn push_host_call_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    layouts: &LayoutInterner,
    destinations: &[ValueId],
    import: ImportId,
    arguments: &[HostCallArgument],
) -> Result<(), CanonicalAdmissionError> {
    let contract = imports.get(&import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    validate_host_call_lowering(function.id, &function.value_types, layouts, contract, arguments, destinations)
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    for (role, argument) in contract.parameter_roles.iter().zip(arguments) {
        match (role, argument) {
            (HostParameterRole::Value(_), HostCallArgument::Value(value))
            | (HostParameterRole::Capability(_), HostCallArgument::Capability(value)) => push_get(plan, layout, *value)?,
            (HostParameterRole::Buffer { .. }, HostCallArgument::Buffer { .. }) => push_host_slice_plan(plan, function, layout, layouts, argument)?,
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        }
    }
    plan.push(PlanToken::Call(*import_indices.get(&import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
    for destination in destinations.iter().rev() {
        push_set(plan, layout, *destination)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_raw_byte_slice_address_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let (object, start, length) = raw_byte_slice_place(
        function.id,
        &function.value_types,
        layouts,
        place,
        access,
        length,
    )
    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I64ExtendI32U, PlanToken::I64Const(4), PlanToken::I64Add]);
    push_get(plan, layout, start)?;
    plan.push(PlanToken::I64ExtendI32U);
    plan.push(PlanToken::I64Add);
    push_get(plan, layout, length)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add]);
    push_get(plan, layout, start)?;
    plan.push(PlanToken::I32Add);
    Ok(())
}

/// Mirror of `emit_scaled_raw_byte_slice_index`.
fn push_scaled_raw_byte_slice_index_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    place: &Place,
    access: AccessMode,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let (object, start, length) = raw_byte_slice_place(
        function.id,
        &function.value_types,
        layouts,
        place,
        access,
        length,
    )
    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    push_get(plan, layout, start)?;
    plan.push(PlanToken::I64ExtendI32U);
    push_get(plan, layout, length)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX - 4)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    layout.refs.plan_base(plan);
    plan.push(PlanToken::I32Const(4));
    layout.refs.plan_add(plan);
    push_get(plan, layout, start)?;
    layout.refs.plan_add(plan);
    Ok(())
}

pub(super) fn push_copy_bytes_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    length: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    if layout.refs == super::ref_encoding::RefEncoding::Scaled8 {
        push_scaled_raw_byte_slice_index_plan(plan, function, layout, layouts, destination, AccessMode::Write, length)?;
        push_scaled_raw_byte_slice_index_plan(plan, function, layout, layouts, source, AccessMode::Read, length)?;
        push_get(plan, layout, length)?;
        plan.extend([PlanToken::I64ExtendI32U, PlanToken::MemoryCopy { source: 0, destination: 0 }]);
        return Ok(());
    }
    let destination_local = scratch_index(layout, 0);
    let source_local = scratch_index(layout, 1);
    let length_local = scratch_index(layout, 2);
    push_raw_byte_slice_address_plan(
        plan,
        function,
        layout,
        layouts,
        destination,
        AccessMode::Write,
        length,
    )?;
    plan.push(PlanToken::LocalSet(destination_local));
    push_raw_byte_slice_address_plan(
        plan,
        function,
        layout,
        layouts,
        source,
        AccessMode::Read,
        length,
    )?;
    plan.push(PlanToken::LocalSet(source_local));
    push_get(plan, layout, length)?;
    plan.push(PlanToken::LocalSet(length_local));
    plan.extend([
        PlanToken::LocalGet(length_local),
        PlanToken::I32Const(8),
        PlanToken::I32LeU,
        PlanToken::If("Empty".to_owned()),
    ]);
    for width in 1..=8 {
        plan.extend([
            PlanToken::LocalGet(length_local),
            PlanToken::I32Const(width),
            PlanToken::I32Eq,
            PlanToken::If("Empty".to_owned()),
        ]);
        let (load, store) = match width {
            1 => (
                PlanToken::I32Load8U { offset: 0, align: 0, memory: 0 },
                PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 },
            ),
            2 => (
                PlanToken::I32Load16U { offset: 0, align: 1, memory: 0 },
                PlanToken::I32Store16 { offset: 0, align: 1, memory: 0 },
            ),
            4 => (
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
            ),
            8 => (
                PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
                PlanToken::I64Store { offset: 0, align: 3, memory: 0 },
            ),
            3 | 5 | 6 | 7 => {
                let chunk = if width == 3 { 2 } else { 4 };
                let offset = width - chunk;
                let first = scratch_index(layout, 3);
                let last = scratch_index(layout, 4);
                let (load, store) = if chunk == 2 {
                    (
                        PlanToken::I32Load16U { offset: 0, align: 1, memory: 0 },
                        PlanToken::I32Store16 { offset: 0, align: 1, memory: 0 },
                    )
                } else {
                    (
                        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
                    )
                };
                plan.extend([
                    PlanToken::LocalGet(source_local), PlanToken::I64ExtendI32U, load.clone(), PlanToken::LocalSet(first),
                    PlanToken::LocalGet(source_local), PlanToken::I32Const(offset), PlanToken::I32Add,
                    PlanToken::I64ExtendI32U, load, PlanToken::LocalSet(last),
                    PlanToken::LocalGet(destination_local), PlanToken::I64ExtendI32U, PlanToken::LocalGet(first), store.clone(),
                    PlanToken::LocalGet(destination_local), PlanToken::I32Const(offset), PlanToken::I32Add,
                    PlanToken::I64ExtendI32U, PlanToken::LocalGet(last), store,
                ]);
                plan.push(PlanToken::Else);
                continue;
            }
            _ => unreachable!(),
        };
        plan.extend([
            PlanToken::LocalGet(destination_local), PlanToken::I64ExtendI32U,
            PlanToken::LocalGet(source_local), PlanToken::I64ExtendI32U,
            load, store, PlanToken::Else,
        ]);
    }
    let bulk_copy = [
        PlanToken::LocalGet(destination_local), PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(source_local), PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(length_local), PlanToken::I64ExtendI32U,
        PlanToken::MemoryCopy { source: 0, destination: 0 },
    ];
    plan.extend(bulk_copy.clone());
    plan.extend(std::iter::repeat_n(PlanToken::End, 8));
    plan.push(PlanToken::Else);
    let cursor = scratch_index(layout, 5);
    plan.extend([
        PlanToken::LocalGet(length_local), PlanToken::I32Const(9), PlanToken::I32GeU,
        PlanToken::LocalGet(length_local), PlanToken::I32Const(4096), PlanToken::I32LeU,
        PlanToken::I32And,
        PlanToken::LocalGet(destination_local), PlanToken::LocalGet(length_local), PlanToken::I32Add,
        PlanToken::LocalGet(source_local), PlanToken::I32LeU,
        PlanToken::LocalGet(source_local), PlanToken::LocalGet(length_local), PlanToken::I32Add,
        PlanToken::LocalGet(destination_local), PlanToken::I32LeU,
        PlanToken::I32Or, PlanToken::I32And,
        PlanToken::If("Empty".to_owned()),
        PlanToken::I32Const(0), PlanToken::LocalSet(cursor),
    ]);
    for width in [8, 1] {
        plan.extend([
            PlanToken::Block("Empty".to_owned()),
            PlanToken::Loop("Empty".to_owned()),
            PlanToken::LocalGet(cursor), PlanToken::I32Const(width), PlanToken::I32Add,
            PlanToken::LocalGet(length_local), PlanToken::I32GtU, PlanToken::BrIf(1),
            PlanToken::LocalGet(destination_local), PlanToken::LocalGet(cursor), PlanToken::I32Add,
            PlanToken::I64ExtendI32U,
            PlanToken::LocalGet(source_local), PlanToken::LocalGet(cursor), PlanToken::I32Add,
            PlanToken::I64ExtendI32U,
        ]);
        if width == 8 {
            plan.extend([
                PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
                PlanToken::I64Store { offset: 0, align: 3, memory: 0 },
            ]);
        } else {
            plan.extend([
                PlanToken::I32Load8U { offset: 0, align: 0, memory: 0 },
                PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 },
            ]);
        }
        plan.extend([
            PlanToken::LocalGet(cursor), PlanToken::I32Const(width), PlanToken::I32Add,
            PlanToken::LocalSet(cursor), PlanToken::Br(0), PlanToken::End, PlanToken::End,
        ]);
    }
    plan.push(PlanToken::Else);
    plan.extend(bulk_copy);
    plan.extend([PlanToken::End, PlanToken::End]);
    Ok(())
}

pub(super) fn push_move_range_base_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    object: ValueId,
    start: ValueId,
    collection: CollectionLayout,
    scratch: usize,
) -> Result<(), CanonicalAdmissionError> {
    if layout.refs == super::ref_encoding::RefEncoding::Scaled8 {
        for _ in 0..2 {
            push_get(plan, layout, start)?;
            plan.extend([
                PlanToken::I64ExtendI32U,
                PlanToken::I64Const(i64::from(collection.stride)),
                PlanToken::I64Mul,
                PlanToken::I64Const(i64::from(collection.data_offset)),
                PlanToken::I64Add,
            ]);
        }
        plan.extend([
            PlanToken::I64Const(i64::from(u32::MAX)),
            PlanToken::I64GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::I32WrapI64,
            PlanToken::LocalSet(layout.scratch[scratch]),
        ]);
        return Ok(());
    }
    push_get(plan, layout, object)?;
    plan.push(PlanToken::I64ExtendI32U);
    push_get(plan, layout, start)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(collection.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(collection.data_offset)),
        PlanToken::I64Add,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    push_get(plan, layout, object)?;
    plan.push(PlanToken::I64ExtendI32U);
    push_get(plan, layout, start)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(collection.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(collection.data_offset)),
        PlanToken::I64Add,
        PlanToken::I64Add,
        PlanToken::I32WrapI64,
        PlanToken::LocalSet(layout.scratch[scratch]),
    ]);
    Ok(())
}

/// Mirror of `emit_range_cursor`.
pub(super) fn push_range_cursor_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    object: ValueId,
    scratch: usize,
) -> Result<(), CanonicalAdmissionError> {
    if layout.refs == super::ref_encoding::RefEncoding::Scaled8 {
        push_get(plan, layout, object)?;
        layout.refs.plan_base(plan);
        plan.push(PlanToken::LocalGet(layout.scratch[scratch]));
        layout.refs.plan_add(plan);
    } else {
        plan.push(PlanToken::LocalGet(layout.scratch[scratch]));
    }
    Ok(())
}

pub(super) fn push_move_range_element_address_plan(
    plan: &mut Vec<PlanToken>,
    layout: &LocalLayout,
    object: ValueId,
    base_scratch: usize,
    index_scratch: usize,
    stride: u32,
) -> Result<(), CanonicalAdmissionError> {
    if layout.refs == super::ref_encoding::RefEncoding::Scaled8 {
        plan.extend([
            PlanToken::LocalGet(layout.scratch[base_scratch]),
            PlanToken::I64ExtendI32U,
            PlanToken::LocalGet(layout.scratch[index_scratch]),
            PlanToken::I64ExtendI32U,
            PlanToken::I64Const(i64::from(stride)),
            PlanToken::I64Mul,
            PlanToken::I64Add,
            PlanToken::I64Const(i64::from(u32::MAX)),
            PlanToken::I64GtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
        ]);
        push_range_cursor_plan(plan, layout, object, base_scratch)?;
        plan.extend([
            PlanToken::LocalGet(layout.scratch[index_scratch]),
            PlanToken::I32Const(stride as i32),
            PlanToken::I32Mul,
        ]);
        layout.refs.plan_add(plan);
        return Ok(());
    }
    plan.extend([
        PlanToken::LocalGet(layout.scratch[base_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(layout.scratch[index_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(stride)),
        PlanToken::I64Mul,
        PlanToken::I64Add,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    plan.extend([
        PlanToken::LocalGet(layout.scratch[base_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::LocalGet(layout.scratch[index_scratch]),
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(stride)),
        PlanToken::I64Mul,
        PlanToken::I64Add,
        PlanToken::I32WrapI64,
        PlanToken::I64ExtendI32U,
    ]);
    Ok(())
}

pub(super) fn push_move_range_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    if destination_layout.pointer_layout.is_none() {
        return push_clone_range_plan(
            plan,
            function,
            layout,
            layouts,
            destination,
            source,
            count,
        );
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    push_get(plan, layout, count)?;
    plan.push(PlanToken::LocalSet(layout.scratch[3]));
    push_move_range_base_plan(plan, layout, source.object(), source_start, source_layout, 1)?;
    push_move_range_base_plan(plan, layout, destination.object(), destination_start, destination_layout, 2)?;
    plan.extend([
        PlanToken::I32Const(0),
        PlanToken::LocalSet(layout.scratch[0]),
        PlanToken::Block("Empty".to_owned()),
        PlanToken::Loop("Empty".to_owned()),
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::LocalGet(layout.scratch[3]),
        PlanToken::I32LtU,
        PlanToken::I32Eqz,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Br(2),
        PlanToken::End,
    ]);
    push_move_range_element_address_plan(plan, layout, destination.object(), 2, 0, destination_layout.stride)?;
    push_move_range_element_address_plan(plan, layout, source.object(), 1, 0, source_layout.stride)?;
    plan.push(PlanToken::I32Load { offset: 0, align: 2, memory: 0 });
    plan.push(PlanToken::I32Store { offset: 0, align: 2, memory: 0 });
    push_move_range_element_address_plan(plan, layout, source.object(), 1, 0, source_layout.stride)?;
    plan.extend([
        PlanToken::I32Const(0),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[0]),
        PlanToken::I32Const(1),
        PlanToken::I32Add,
        PlanToken::LocalSet(layout.scratch[0]),
        PlanToken::Br(0),
        PlanToken::End,
        PlanToken::End,
    ]);
    Ok(())
}

pub(super) fn push_clone_range_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    destination: &Place,
    source: &Place,
    count: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let destination_layout = resolve_collection_layout(function, layouts, destination.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let source_layout = resolve_collection_layout(function, layouts, source.object())
        .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    if destination_layout != source_layout {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let (destination_start, source_start) = match (destination.projection(), source.projection()) {
        (Projection::Slice { start: destination_start, length: destination_length }, Projection::Slice { start: source_start, length: source_length })
            if *destination_length == count && *source_length == count => (*destination_start, *source_start),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    push_move_range_base_plan(plan, layout, source.object(), source_start, source_layout, 1)?;
    push_move_range_base_plan(plan, layout, destination.object(), destination_start, destination_layout, 2)?;
    push_get(plan, layout, count)?;
    plan.extend([
        PlanToken::I64ExtendI32U,
        PlanToken::I64Const(i64::from(destination_layout.stride)),
        PlanToken::I64Mul,
        PlanToken::I64Const(i64::from(u32::MAX)),
        PlanToken::I64GtU,
        PlanToken::If("Empty".to_owned()),
        PlanToken::Unreachable,
        PlanToken::End,
    ]);
    if destination_layout.pointer_layout.is_some() {
        if layout.scratch.len() < 5 {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        }
        let load = PlanToken::I32Load { offset: 0, align: 2, memory: 0 };
        plan.extend([
            PlanToken::I32Const(0),
            PlanToken::LocalSet(layout.scratch[0]),
            PlanToken::Block("Empty".to_owned()),
            PlanToken::Loop("Empty".to_owned()),
            PlanToken::LocalGet(layout.scratch[0]),
        ]);
        push_get(plan, layout, count)?;
        plan.extend([
            PlanToken::I32LtU,
            PlanToken::I32Eqz,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Br(2),
            PlanToken::End,
        ]);
        push_range_cursor_plan(plan, layout, source.object(), 1)?;
        plan.extend([
            PlanToken::LocalGet(layout.scratch[0]),
            PlanToken::I32Const(destination_layout.stride as i32),
            PlanToken::I32Mul,
        ]);
        layout.refs.plan_add(plan);
        layout.refs.plan_finish(plan);
        plan.extend([
            load.clone(),
            PlanToken::LocalSet(layout.scratch[3]),
            PlanToken::LocalGet(layout.scratch[3]),
            PlanToken::GlobalGet(2),
            PlanToken::I32LtU,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Else,
            PlanToken::LocalGet(layout.scratch[3]),
        ]);
        layout.refs.plan_base(plan);
        plan.push(PlanToken::I32Const(8));
        layout.refs.plan_sub(plan);
        layout.refs.plan_finish(plan);
        plan.extend([
            load,
            PlanToken::LocalSet(layout.scratch[4]),
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Eqz,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Const(-1),
            PlanToken::I32Eq,
            PlanToken::If("Empty".to_owned()),
            PlanToken::Unreachable,
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[3]),
        ]);
        layout.refs.plan_base(plan);
        plan.push(PlanToken::I32Const(8));
        layout.refs.plan_sub(plan);
        layout.refs.plan_finish(plan);
        plan.extend([
            PlanToken::LocalGet(layout.scratch[4]),
            PlanToken::I32Const(1),
            PlanToken::I32Add,
            PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
            PlanToken::End,
            PlanToken::LocalGet(layout.scratch[0]),
            PlanToken::I32Const(1),
            PlanToken::I32Add,
            PlanToken::LocalSet(layout.scratch[0]),
            PlanToken::Br(0),
            PlanToken::End,
            PlanToken::End,
        ]);
    }
    push_range_cursor_plan(plan, layout, destination.object(), 2)?;
    layout.refs.plan_finish(plan);
    push_range_cursor_plan(plan, layout, source.object(), 1)?;
    layout.refs.plan_finish(plan);
    push_get(plan, layout, count)?;
    plan.extend([
        PlanToken::I32Const(destination_layout.stride as i32),
        PlanToken::I32Mul,
        PlanToken::I64ExtendI32U,
        PlanToken::MemoryCopy { source: 0, destination: 0 },
    ]);
    Ok(())
}

pub(super) fn push_reclaim_plan(plan: &mut Vec<PlanToken>, layout: &LocalLayout, object: ValueId) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, layout, object)?;
    plan.push(PlanToken::LocalSet(layout.scratch[1]));
    plan.push(PlanToken::LocalGet(layout.scratch[1]));
    layout.refs.plan_object(plan);
    plan.extend([
        PlanToken::GlobalGet(1),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[1]),
        PlanToken::GlobalSet(1),
    ]);
    Ok(())
}

pub(super) fn push_reclaim_local_plan(plan: &mut Vec<PlanToken>, layout: &LocalLayout, slot: usize) {
    plan.push(PlanToken::LocalGet(layout.scratch[slot]));
    layout.refs.plan_object(plan);
    plan.extend([
        PlanToken::GlobalGet(1),
        PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        PlanToken::LocalGet(layout.scratch[slot]),
        PlanToken::GlobalSet(1),
    ]);
}

pub(super) fn push_drop_ref_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    layout: &LocalLayout,
    object: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let Some(ValueType::Ref(object_layout)) = function.value_types.get(&object).copied() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    push_get(plan, layout, object)?;
    if let Some(index) = layout.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&object_layout)) {
        plan.push(PlanToken::Call(*index));
        return Ok(());
    }
    plan.push(PlanToken::LocalSet(layout.scratch[0]));
    drop_worklist::plan(plan, function.id, layouts, layout, object_layout)
}

pub(super) fn expected_alloc_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layout: &LocalLayout,
    collection: Option<CollectionLayout>,
    destination: ValueId,
    extent: &Extent,
) -> Result<(), CanonicalAdmissionError> {
    memory_op_counters::plan_bump(plan, memory_op_counters::Counter::Allocs);
    // Mirror of `emit_alloc`: the size in scratch 0 comes from the capacity
    // of a counted collection or is a fixed layout's constant.
    let collection = match extent {
        Extent::Counted { capacity, header_bytes, stride_bytes, .. } => {
            let collection = collection.ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            if *header_bytes != collection.data_offset || *stride_bytes != collection.stride {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            }
            let maximum = (layout.refs.max_alloc_bytes() - collection.data_offset) / collection.stride;
            push_get(plan, layout, *capacity)?;
            plan.extend([
                PlanToken::I64ExtendI32U,
                PlanToken::I64Const(i64::from(maximum)),
                PlanToken::I64GtU,
                PlanToken::If("Empty".to_owned()),
                PlanToken::Unreachable,
                PlanToken::End,
            ]);
            push_get(plan, layout, *capacity)?;
            plan.extend([
                PlanToken::I64ExtendI32U,
                PlanToken::I64Const(i64::from(collection.stride)),
                PlanToken::I64Mul,
                PlanToken::I64Const(i64::from(collection.data_offset)),
                PlanToken::I64Add,
            ]);
            layout.refs.plan_round_i64(plan);
            plan.extend([PlanToken::I32WrapI64, PlanToken::LocalSet(layout.scratch[0])]);
            Some(collection)
        }
        Extent::FixedBytes(bytes) => {
            let bytes = fixed_alloc_bytes(*bytes).and_then(|bytes| layout.refs.round_bytes(bytes))
                .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            plan.extend([PlanToken::I32Const(bytes as i32), PlanToken::LocalSet(layout.scratch[0])]);
            None
        }
        Extent::HeaderPrefix { .. } => return Err(CanonicalAdmissionError::UnsupportedPlan),
    };
    let s = [0, 1, 2, 3].map(|slot| layout.scratch[slot]);
    push_alloc_core_plan(plan, layout.refs, s, layout.memory_helpers.as_ref().map(|helpers| helpers.alloc));
    plan.extend([
        PlanToken::LocalGet(layout.scratch[2]),
        PlanToken::LocalSet(plan_local(layout, destination)?),
    ]);
    if let (Some(collection), Extent::Counted { count, capacity, .. }) = (collection, extent) {
        plan.push(PlanToken::LocalGet(layout.scratch[2]));
        layout.refs.plan_base(plan);
        plan.push(PlanToken::I32Const(collection.length_offset as i32));
        layout.refs.plan_add(plan);
        layout.refs.plan_finish(plan);
        push_get(plan, layout, *count)?;
        push_store(plan);
        if let Some(capacity_offset) = collection.capacity_offset {
            plan.push(PlanToken::LocalGet(layout.scratch[2]));
            layout.refs.plan_base(plan);
            plan.push(PlanToken::I32Const(capacity_offset as i32));
            layout.refs.plan_add(plan);
            layout.refs.plan_finish(plan);
            push_get(plan, layout, *capacity)?;
            push_store(plan);
        }
    }
    let _ = function;
    Ok(())
}

/// Mirror of `emit_alloc_core`.
pub(super) fn push_alloc_core_plan(plan: &mut Vec<PlanToken>, refs: ref_encoding::RefEncoding, s: [u32; 4], slow: Option<u32>) {
    use PlanToken as P;
    let empty = || "Empty".to_owned();
    if slow.is_some() {
        plan.extend([P::Block(empty()), P::Block(empty())]);
    }
    plan.extend([
        P::GlobalGet(1), P::LocalSet(s[2]), P::I32Const(0), P::LocalSet(s[3]),
        P::Block(empty()), P::Loop(empty()),
        P::LocalGet(s[2]), P::I32Eqz, P::If(empty()),
        P::GlobalGet(0), P::LocalSet(s[1]),
    ]);
    let mut end = vec![P::LocalGet(s[1])];
    refs.plan_object(&mut end);
    end.extend([P::I64Const(i64::from(RC_PREFIX_BYTES)), P::I64Add, P::LocalGet(s[0]), P::I64ExtendI32U, P::I64Add]);
    plan.extend(end.iter().cloned());
    if slow.is_some() {
        plan.extend([P::MemorySize, P::I64Const(i64::from(LINEAR_MEMORY_BYTES)), P::I64Mul, P::I64GeU, P::BrIf(3)]);
    } else {
        plan.extend([P::I64Const(refs.max_end()), P::I64GtU, P::If(empty()), P::Unreachable, P::End]);
        plan.extend(end.iter().cloned());
        plan.extend([P::MemorySize, P::I64Const(i64::from(LINEAR_MEMORY_BYTES)), P::I64Mul, P::I64GtU, P::If(empty())]);
        plan.extend(end.iter().cloned());
        plan.extend([
            P::I64Const(i64::from(LINEAR_MEMORY_BYTES - 1)), P::I64Add,
            P::I64Const(i64::from(LINEAR_MEMORY_BYTES)), P::I64DivU,
            P::MemorySize, P::I64Sub, P::MemoryGrow, P::I64Const(-1), P::I64Eq,
            P::If(empty()), P::Unreachable, P::End, P::End,
        ]);
    }
    let load = || P::I32Load { offset: 0, align: 2, memory: 0 };
    let object = |plan: &mut Vec<PlanToken>, slot: u32| {
        plan.push(P::LocalGet(slot));
        refs.plan_object(plan);
    };
    plan.extend([P::LocalGet(s[1]), P::I32Const(refs.prefix_units()), P::I32Add, P::LocalSet(s[2]), P::LocalGet(s[2]), P::LocalGet(s[0])]);
    refs.plan_units(plan);
    plan.extend([P::I32Add, P::GlobalSet(0), P::Br(2), P::End, P::LocalGet(s[2])]);
    refs.plan_base(plan);
    plan.push(P::I32Const(4));
    refs.plan_sub(plan);
    refs.plan_finish(plan);
    plan.extend([load(), P::LocalGet(s[0]), P::I32Eq, P::If(empty()), P::LocalGet(s[3]), P::I32Eqz, P::If(empty())]);
    object(plan, s[2]);
    plan.extend([load(), P::GlobalSet(1), P::Else]);
    object(plan, s[3]);
    object(plan, s[2]);
    plan.extend([
        load(), P::I32Store { offset: 0, align: 2, memory: 0 }, P::End,
        P::LocalGet(s[2]), P::I32Const(refs.prefix_units()), P::I32Sub, P::LocalSet(s[1]),
        P::Br(2), P::End,
        P::LocalGet(s[2]), P::LocalSet(s[3]),
    ]);
    object(plan, s[2]);
    plan.extend([load(), P::LocalSet(s[2]), P::Br(0), P::End, P::End]);
    object(plan, s[1]);
    plan.push(P::I32Const(1));
    push_store(plan);
    plan.push(P::LocalGet(s[1]));
    refs.plan_base(plan);
    plan.push(P::I32Const(4));
    refs.plan_add(plan);
    refs.plan_finish(plan);
    plan.push(P::LocalGet(s[0]));
    push_store(plan);
    if let Some(helper) = slow {
        plan.extend([P::Br(1), P::End, P::LocalGet(s[0]), P::Call(helper), P::LocalSet(s[2]), P::End]);
    }
}

pub(super) fn expected_block_operations_plan(
    function: &Function,
    block: &BasicBlock,
    layout: &LocalLayout,
    layouts: &LayoutInterner,
    function_indices: &BTreeMap<FunctionId, u32>,
    signature_indices: &BTreeMap<crate::canonical_safewir::FunctionSignatureId, u32>,
    imports: &BTreeMap<ImportId, ImportContract>,
    import_indices: &BTreeMap<ImportId, u32>,
    static_data_offsets: &BTreeMap<crate::canonical_safewir::StaticDataId, u32>,
    aggregates: &mut BTreeMap<ValueId, Vec<ValueId>>,
    owner_cleanup_function: Option<u32>,
) -> Result<Vec<PlanToken>, CanonicalAdmissionError> {
    // The independent function planner has already derived and checked this
    // whole-function layout. Borrow it and the original block: rebuilding a
    // synthetic function here cloned every value type and owner recipe once
    // per block. CFG terminators remain the independent PlanEmitter's job.
    let mut plan = Vec::new();
    for operation in &block.operations {
        match operation {
            // Checker-only proof transition: admission records the source
            // correspondence, but the validated Wasm plan intentionally
            // emits no instruction.
            Operation::ReauthenticateRawHeaderExtent { .. } => {}
            Operation::ClosureOwner(operation) => { owner_operation_kernel::plan(&mut plan, function, layouts, &layout, operation, import_indices, owner_cleanup_function)?; }
            Operation::Dictionary(operation) => { dictionary_kernel::plan(&mut plan, function, layouts, &layout, operation)?; }
            Operation::ConstInt { destination, value } => {
                plan.push(PlanToken::I64Const(*value));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Arithmetic { destination, operator, left, right, overflow } => {
                if matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Int),
                        ArithmeticOperator::Add,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    )
                ) {
                    push_get(&mut plan, &layout, *left)?;
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64Add,
                        PlanToken::LocalTee(plan_local(&layout, *destination)?),
                    ]);
                    push_get(&mut plan, &layout, *left)?;
                    plan.push(PlanToken::I64Xor);
                    push_get(&mut plan, &layout, *destination)?;
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64Xor,
                        PlanToken::I64And,
                        PlanToken::I64Const(0),
                        PlanToken::I64LtS,
                        PlanToken::If("Empty".to_owned()),
                        PlanToken::Unreachable,
                        PlanToken::End,
                    ]);
                    continue;
                } else if matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Size),
                        ArithmeticOperator::Add,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    )
                ) {
                    push_get(&mut plan, &layout, *left)?;
                    plan.push(PlanToken::I64ExtendI32U);
                    push_get(&mut plan, &layout, *right)?;
                    plan.extend([
                        PlanToken::I64ExtendI32U,
                        PlanToken::I64Add,
                        PlanToken::I64Const(i64::from(u32::MAX)),
                        PlanToken::I64GtU,
                        PlanToken::If("Empty".to_owned()),
                        PlanToken::Unreachable,
                        PlanToken::End,
                    ]);
                } else if !matches!(
                    (function.value_types.get(destination), operator, overflow),
                    (
                        Some(ValueType::Int | ValueType::Size),
                        ArithmeticOperator::DivideSigned
                            | ArithmeticOperator::DivideUnsigned
                            | ArithmeticOperator::RemainderSigned
                            | ArithmeticOperator::RemainderUnsigned,
                        crate::canonical_safewir::OverflowBehavior::Trap,
                    ) | (_, _, crate::canonical_safewir::OverflowBehavior::Wrap)
                ) {
                    // Mirror of `emit_checked_arithmetic`: integer division
                    // and remainder trap natively; everything else must wrap.
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                let value_type = function
                    .value_types
                    .get(destination)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(planned_arithmetic_token(value_type, *operator)?);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstUnit { destination } => {
                plan.push(PlanToken::I32Const(0));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstBool { destination, value } => {
                plan.push(PlanToken::I32Const(i32::from(*value)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstFloat { destination, bits } => {
                plan.push(PlanToken::F64Const(*bits));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::ConstSize { destination, value } => {
                let value = u32::try_from(*value)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(value as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityNone { destination, .. } => {
                plan.push(PlanToken::RefNull(format!("{:?}", wasmparser::HeapType::EXTERN)));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilitySome { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityIsPresent { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                plan.extend([PlanToken::RefIsNull, PlanToken::I32Eqz]);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::CapabilityUnwrap { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                plan.push(PlanToken::RefAsNonNull);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::StaticData { destination, data, .. } => {
                let offset = static_data_offsets
                    .get(data)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::I32Const(offset as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::InitNullableOwned { place } => {
                push_init_nullable_owned_plan(&mut plan, function, layouts, &layout, place)?;
            }
            Operation::NarrowSize { destination, value, exact } => {
                if !*exact {
                    push_get(&mut plan, &layout, *value)?;
                    plan.extend([
                        PlanToken::I64Const(i64::from(u32::MAX)),
                        PlanToken::I64GtU,
                        PlanToken::If("Empty".to_owned()),
                        PlanToken::Unreachable,
                        PlanToken::End,
                    ]);
                }
                push_get(&mut plan, &layout, *value)?;
                plan.extend([PlanToken::I32WrapI64, PlanToken::LocalSet(plan_local(&layout, *destination)?)])
            }
            Operation::WidenSize { destination, value } => {
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    PlanToken::I64ExtendI32U,
                    PlanToken::LocalSet(plan_local(&layout, *destination)?),
                ]);
            }
            Operation::FloatUnary { destination, operator, value } => {
                push_get(&mut plan, &layout, *value)?;
                plan.extend([
                    match operator {
                        crate::canonical_safewir::FloatUnaryOperator::Sqrt => PlanToken::F64Sqrt,
                        crate::canonical_safewir::FloatUnaryOperator::TruncateToInt => PlanToken::I64TruncSatF64S,
                        crate::canonical_safewir::FloatUnaryOperator::FromInt => PlanToken::F64ConvertI64S,
                    },
                    PlanToken::LocalSet(plan_local(&layout, *destination)?),
                ]);
            }
            Operation::MoveValue { destination, source } => {
                push_get(&mut plan, &layout, *source)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::MakeInlineAggregate { destination, fields, .. } => {
                aggregates.insert(*destination, fields.clone());
            }
            Operation::ExtractInlineField { destination, aggregate, index } => {
                let fields = aggregates.get(aggregate).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                let field = *fields.get(*index as usize).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                push_get(&mut plan, &layout, field)?;
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::FunctionValue { destination, function: target } => {
                let table_index = checked_table_index(function_indices, *target)?;
                plan.push(PlanToken::I32Const(table_index as i32));
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Compare { destination, operator, left, right } => {
                let value_type = function
                    .value_types
                    .get(left)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                push_get(&mut plan, &layout, *left)?;
                push_get(&mut plan, &layout, *right)?;
                plan.push(planned_compare_token(value_type, *operator)?);
                push_set(&mut plan, &layout, *destination)?;
            }
            Operation::Guard { condition, .. } => {
                push_get(&mut plan, &layout, *condition)?;
                plan.extend([
                    PlanToken::I32Eqz,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
            }
            Operation::Alloc { destination, layout: layout_id, extent, class } => {
                if *class != crate::canonical_safewir::AllocationClass::ReferenceCounted {
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                let collection = collection_layout(function.id, layouts, *layout_id).ok();
                expected_alloc_plan(&mut plan, function, &layout, collection, *destination, extent)?;
            }
            Operation::Load { destination, place, transfer }
                if matches!(
                    transfer,
                    LoadTransfer::BorrowAffine
                        | LoadTransfer::CopyScalar
                        | LoadTransfer::CloneShared
                        | LoadTransfer::ClonePresent
                        | LoadTransfer::MoveUnique
                        | LoadTransfer::TakeLast
                        | LoadTransfer::TakeNullableUnique
                        | LoadTransfer::TakeOwnedField
                ) =>
            {
                if is_raw_allocation_capacity_load(function.id, &function.value_types, layouts, place) {
                    push_get(&mut plan, &layout, place.object())?;
                    layout.refs.plan_base(&mut plan);
                    plan.push(PlanToken::I32Const(4));
                    layout.refs.plan_sub(&mut plan);
                    layout.refs.plan_finish(&mut plan);
                    plan.extend([
                        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                        PlanToken::I32Const(4), PlanToken::I32Sub,
                    ]);
                    push_set(&mut plan, &layout, *destination)?;
                    continue;
                }
                push_memory_load_plan(&mut plan, function, layouts, &layout, place, *destination)?;
                match transfer {
                    LoadTransfer::BorrowAffine => {
                        push_require_present_plan(&mut plan, &layout, *destination)?;
                    },
                    LoadTransfer::CloneShared => push_share_plan(&mut plan, &layout, *destination, *destination)?,
                    LoadTransfer::ClonePresent => {
                        push_require_present_plan(&mut plan, &layout, *destination)?;
                        push_share_plan(&mut plan, &layout, *destination, *destination)?;
                    }
                    LoadTransfer::TakeNullableUnique | LoadTransfer::TakeOwnedField => {
                        push_require_present_plan(&mut plan, &layout, *destination)?;
                        let writable = crate::canonical_safewir::Place::projected(
                            place.object(), place.projection().clone(), place.value_type(),
                            crate::canonical_safewir::AccessMode::Write,
                        );
                        if *transfer == LoadTransfer::TakeOwnedField {
                            push_clear_owned_field_plan(&mut plan, function, layouts, &layout, &writable)?;
                        } else {
                            push_init_nullable_owned_plan(&mut plan, function, layouts, &layout, &writable)?;
                        }
                    }
                    LoadTransfer::TakeLast => {
                        let Projection::Element { index } = place.projection() else { return Err(CanonicalAdmissionError::UnsupportedPlan); };
                        push_update_count_plan(&mut plan, function, layouts, &layout, place.object(), *index)?;
                    }
                    LoadTransfer::CopyScalar | LoadTransfer::MoveUnique => {}
                }
            }
            Operation::Store { place, value, transfer }
                if matches!(transfer, StoreTransfer::CopyScalar | StoreTransfer::MoveUnique | StoreTransfer::CloneShared) =>
            {
                if *transfer == StoreTransfer::CloneShared {
                    push_share_plan(&mut plan, &layout, *value, *value)?;
                }
                push_memory_store_plan(&mut plan, function, layouts, &layout, place, *value)?;
            }
            Operation::CopyBytes { destination, source, length } => {
                push_copy_bytes_plan(
                    &mut plan,
                    function,
                    &layout,
                    layouts,
                    destination,
                    source,
                    *length,
                )?;
            }
            Operation::MoveRange { destination, source, count } => {
                push_move_range_plan(&mut plan, function, &layout, layouts, destination, source, *count)?;
            }
            Operation::CloneRange { destination, source, count } => {
                push_clone_range_plan(&mut plan, function, &layout, layouts, destination, source, *count)?;
            }
            Operation::UpdateCount { object, count } => {
                push_update_count_plan(&mut plan, function, layouts, &layout, *object, *count)?;
            }
            Operation::CloneRef { destination, source } => {
                push_share_plan(&mut plan, &layout, *destination, *source)?;
            }
            Operation::Reclaim { object } => {
                push_reclaim_plan(&mut plan, &layout, *object)?;
            }
            Operation::Release { object } => {
                push_drop_ref_plan(&mut plan, function, layouts, &layout, *object)?;
            }
            Operation::DropRef { object } => {
                push_drop_ref_plan(&mut plan, function, layouts, &layout, *object)?;
            }
            Operation::Call { destinations, callee, arguments } => {
                for argument in arguments {
                    push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
                }
                let index = function_indices
                    .get(callee)
                    .copied()
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
                plan.push(PlanToken::Call(index));
                for destination in destinations.iter().rev() {
                    push_set(&mut plan, &layout, *destination)?;
                }
            }
            Operation::ExternalCall { destinations, import, arguments } => {
                for argument in arguments {
                    let value = plan_call_argument(argument)?;
                    if function.value_types.get(&value) != Some(&ValueType::Unit) {
                        push_get(&mut plan, &layout, value)?;
                    }
                }
                plan.push(PlanToken::Call(*import_indices.get(import).ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
                for destination in destinations.iter().rev() {
                    if function.value_types.get(destination) != Some(&ValueType::Unit) {
                        push_set(&mut plan, &layout, *destination)?;
                    }
                }
                for destination in destinations {
                    if function.value_types.get(destination) == Some(&ValueType::Unit) {
                        plan.push(PlanToken::I32Const(0));
                        push_set(&mut plan, &layout, *destination)?;
                    }
                }
            }
            Operation::VarCall { destinations, callee, signature: call_signature, arguments } => {
                if function.value_types.get(callee) != Some(&ValueType::Callable(*call_signature)) {
                    return Err(CanonicalAdmissionError::UnsupportedPlan);
                }
                for argument in arguments {
                    push_get(&mut plan, &layout, plan_call_argument(argument)?)?;
                }
                push_get(&mut plan, &layout, *callee)?;
                plan.push(PlanToken::CallIndirect {
                    type_index: *signature_indices.get(call_signature).ok_or(CanonicalAdmissionError::UnsupportedPlan)?,
                    table_index: 0,
                });
                for destination in destinations.iter().rev() {
                    push_set(&mut plan, &layout, *destination)?;
                }
            }
            Operation::HostCall { destinations, import, arguments } => {
                push_host_call_plan(&mut plan, function, &layout, imports, import_indices, layouts, destinations, *import, arguments)?;
            }
            _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
        }
    }
    Ok(plan)
}
