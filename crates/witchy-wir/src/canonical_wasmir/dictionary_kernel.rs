//! Guest-resident canonical dictionary operations.

use super::*;
use crate::canonical_safewir::dictionary::DictionaryOperation;

fn counted_extent(capacity: ValueId) -> Extent {
    Extent::Counted { header_bytes: 36, count: capacity, capacity, stride_bytes: 33 }
}

fn header_offset(field: crate::canonical_safewir::dictionary::DictionaryHeaderField) -> i32 {
    use crate::canonical_safewir::dictionary::DictionaryHeaderField as F;
    match field { F::Count => 0, F::Capacity => 4, F::Head => 8, F::Tail => 12, F::Tombstones => 16 }
}

fn slot_offset(field: crate::canonical_safewir::dictionary::DictionarySlotField) -> i32 {
    use crate::canonical_safewir::dictionary::DictionarySlotField as F;
    match field { F::State => 0, F::Hash => 4, F::Next => 8, F::Prev => 12, F::Key => 16, F::Value => 24 }
}

fn emit_slot_address(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, bucket: ValueId, field: crate::canonical_safewir::dictionary::DictionarySlotField,
) -> Result<(), EncodeError> {
    // Bounds first. The authenticated extent is 20+32*capacity, so a bucket
    // below capacity cannot address beyond the dictionary allocation.
    emit_get(body, local, function.id, bucket)?;
    emit_get(body, local, function.id, dict)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32GeU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, local, function.id, dict)?;
    emit_get(body, local, function.id, bucket)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(20));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, local.scratch[4]);
    if field != crate::canonical_safewir::dictionary::DictionarySlotField::State {
        emit_get_index(body, local.scratch[4]);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Ne);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Unreachable);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    Ok(())
}

fn plan_slot_address(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId, bucket: ValueId,
    field: crate::canonical_safewir::dictionary::DictionarySlotField,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, bucket)?;
    push_get(plan, local, dict)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I32GeU,
        PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
    push_get(plan, local, dict)?;
    push_get(plan, local, bucket)?;
    plan.extend([PlanToken::I32Const(32), PlanToken::I32Mul, PlanToken::I32Add,
        PlanToken::I32Const(20), PlanToken::I32Add, PlanToken::LocalSet(local.scratch[4])]);
    if field != crate::canonical_safewir::dictionary::DictionarySlotField::State {
        plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
            PlanToken::I32Const(1), PlanToken::I32Ne, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
    }
    Ok(())
}

fn emit_store_value(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    value: ValueId, address: u32,
) -> Result<(), EncodeError> {
    emit_get_index(body, address);
    emit_get(body, local, function.id, value)?;
    body.instruction(&match function.value_types.get(&value) {
        Some(ValueType::Int) => wasm_encoder::Instruction::I64Store(memarg(8)),
        Some(ValueType::Float) => wasm_encoder::Instruction::F64Store(memarg(8)),
        Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => wasm_encoder::Instruction::I32Store(memarg(4)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary store type" }),
    });
    Ok(())
}

fn plan_store_value(plan: &mut Vec<PlanToken>, function: &Function, local: &LocalLayout,
    value: ValueId, address: u32,
) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalGet(address));
    push_get(plan, local, value)?;
    plan.push(match function.value_types.get(&value) {
        Some(ValueType::Int) => PlanToken::I64Store { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Float) => PlanToken::F64Store { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    Ok(())
}

fn emit_i32_store(body: &mut WasmFunctionEncoder, address: u32, offset: i32, value: u32) {
    emit_get_index(body, address);
    if offset != 0 {
        body.instruction(&wasm_encoder::Instruction::I32Const(offset));
        body.instruction(&wasm_encoder::Instruction::I32Add);
    }
    emit_get_index(body, value);
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
}

fn plan_i32_store(plan: &mut Vec<PlanToken>, address: u32, offset: i32, value: u32) {
    plan.push(PlanToken::LocalGet(address));
    if offset != 0 { plan.extend([PlanToken::I32Const(offset), PlanToken::I32Add]); }
    plan.extend([PlanToken::LocalGet(value), PlanToken::I32Store { offset: 0, align: 2, memory: 0 }]);
}

// The row region remains at +20. Controls follow all rows, and the first
// sixteen controls are mirrored after the array for a wrapped SIMD load.
fn emit_control_base(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId,
) -> Result<(), EncodeError> {
    emit_get(body, local, function.id, dict)?;
    emit_get(body, local, function.id, dict)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(20));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    Ok(())
}

fn plan_control_base(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId)
    -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, dict)?;
    push_get(plan, local, dict)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        PlanToken::I32Const(32), PlanToken::I32Mul, PlanToken::I32Add,
        PlanToken::I32Const(20), PlanToken::I32Add]);
    Ok(())
}

fn emit_fill_controls(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, capacity: ValueId,
) -> Result<(), EncodeError> {
    emit_control_base(body, function, local, dict)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0x80));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(16));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::MemoryFill(0));
    Ok(())
}

fn plan_fill_controls(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId,
    capacity: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    plan_control_base(plan, local, dict)?;
    plan.push(PlanToken::I32Const(0x80));
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Const(16), PlanToken::I32Add, PlanToken::MemoryFill(0)]);
    Ok(())
}

// `bucket` and `control` are scalar scratch locals. Mutations mirror the
// first group so ProbeGroup can load sixteen bytes across capacity wrap.
fn emit_write_control(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, bucket: u32, control: u32,
) -> Result<(), EncodeError> {
    emit_control_base(body, function, local, dict)?;
    emit_get_index(body, bucket);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, control);
    body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
    emit_get_index(body, bucket);
    body.instruction(&wasm_encoder::Instruction::I32Const(16));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_control_base(body, function, local, dict)?;
    emit_get(body, local, function.id, dict)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, bucket);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, control);
    body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn plan_write_control(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId,
    bucket: u32, control: u32,
) -> Result<(), CanonicalAdmissionError> {
    plan_control_base(plan, local, dict)?;
    plan.extend([PlanToken::LocalGet(bucket), PlanToken::I32Add, PlanToken::LocalGet(control),
        PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 }, PlanToken::LocalGet(bucket),
        PlanToken::I32Const(16), PlanToken::I32LtU, PlanToken::If("Empty".into())]);
    plan_control_base(plan, local, dict)?;
    push_get(plan, local, dict)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I32Add,
        PlanToken::LocalGet(bucket), PlanToken::I32Add, PlanToken::LocalGet(control),
        PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 }, PlanToken::End]);
    Ok(())
}

fn pointer_offsets(function: &Function, layouts: &LayoutInterner, source: ValueId) -> Result<Vec<i32>, EncodeError> {
    let Some(ValueType::Ref(layout)) = function.value_types.get(&source).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary source" });
    };
    let physical = resolve_backend_layout(function.id, layouts, layout, "Dictionary source")
        .map_err(EncodeError::Lowering)?;
    let Some(descriptor) = layouts.get(physical) else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary layout" });
    };
    let LayoutKind::CanonicalDictionary { key, value, .. } = descriptor.kind() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary layout" });
    };
    let mut offsets = Vec::new();
    for (offset, slot) in [(16, *key), (24, *value)] {
        let physical = resolve_backend_layout(function.id, layouts, slot, "Dictionary slot")
            .map_err(EncodeError::Lowering)?;
        match layouts.get(physical).map(|descriptor| descriptor.kind()) {
            Some(LayoutKind::RuntimePointerValueSlot { semantics: PointerSlot::Owned, .. }) => offsets.push(offset),
            Some(LayoutKind::Scalar(_)) => {}
            _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary slot" }),
        }
    }
    Ok(offsets)
}

fn key_pointee(function: &Function, layouts: &LayoutInterner, source: ValueId) -> Result<Option<crate::layout::LayoutId>, EncodeError> {
    let Some(ValueType::Ref(layout)) = function.value_types.get(&source).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary source" });
    };
    let physical = resolve_backend_layout(function.id, layouts, layout, "Dictionary key")
        .map_err(EncodeError::Lowering)?;
    let Some(descriptor) = layouts.get(physical) else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary layout" });
    };
    let LayoutKind::CanonicalDictionary { key, .. } = descriptor.kind() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary layout" });
    };
    let physical = resolve_backend_layout(function.id, layouts, *key, "Dictionary key")
        .map_err(EncodeError::Lowering)?;
    match layouts.get(physical).map(|descriptor| descriptor.kind()) {
        Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned }) => Ok(Some(*pointee)),
        Some(LayoutKind::Scalar(_)) => Ok(None),
        _ => Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary key" }),
    }
}

fn emit_clone_exact(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
) -> Result<(), EncodeError> {
    let offsets = pointer_offsets(function, layouts, source)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if !offsets.is_empty() && retain.is_none() {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary retain helper" });
    }
    emit_capacity_guard(body, function, local, capacity)?;
    emit_get(body, local, function.id, source)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Ne);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary clone destination" });
    };
    emit_alloc(body, function, local, layouts, destination, layout_id, &counted_extent(capacity))?;
    emit_get(body, local, function.id, destination)?;
    emit_get(body, local, function.id, source)?;
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(33));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Const(36));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    if !offsets.is_empty() {
        let retain = retain.expect("owned dictionary slots require a retain helper");
        body.instruction(&wasm_encoder::Instruction::I32Const(0));
        emit_set_index(body, local.scratch[4]);
        body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
        body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
        emit_get_index(body, local.scratch[4]);
        emit_get(body, local, function.id, capacity)?;
        body.instruction(&wasm_encoder::Instruction::I32GeU);
        body.instruction(&wasm_encoder::Instruction::BrIf(1));
        emit_get(body, local, function.id, destination)?;
        body.instruction(&wasm_encoder::Instruction::I32Const(20));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_get_index(body, local.scratch[4]);
        body.instruction(&wasm_encoder::Instruction::I32Const(32));
        body.instruction(&wasm_encoder::Instruction::I32Mul);
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_set_index(body, local.scratch[5]);
        emit_get_index(body, local.scratch[5]);
        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Eq);
        body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
        for offset in offsets {
            emit_get_index(body, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::I32Const(offset));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            body.instruction(&wasm_encoder::Instruction::Call(retain));
            body.instruction(&wasm_encoder::Instruction::Drop);
        }
        body.instruction(&wasm_encoder::Instruction::End);
        emit_get_index(body, local.scratch[4]);
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Add);
        emit_set_index(body, local.scratch[4]);
        body.instruction(&wasm_encoder::Instruction::Br(0));
        body.instruction(&wasm_encoder::Instruction::End);
        body.instruction(&wasm_encoder::Instruction::End);
    }
    Ok(())
}

fn plan_clone_exact(plan: &mut Vec<PlanToken>, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    let offsets = pointer_offsets(function, layouts, source).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if !offsets.is_empty() && retain.is_none() { return Err(CanonicalAdmissionError::UnsupportedPlan); }
    plan_capacity_guard(plan, local, capacity)?;
    push_get(plan, local, source)?;
    plan.extend([PlanToken::I32Const(4), PlanToken::I32Add,
        PlanToken::I32Load { offset: 0, align: 2, memory: 0 }]);
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Ne, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    let collection = collection_layout(function.id, layouts, layout_id).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    expected_alloc_plan(plan, function, local, Some(collection), destination, &counted_extent(capacity))?;
    push_get(plan, local, destination)?;
    push_get(plan, local, source)?;
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Const(33), PlanToken::I32Mul, PlanToken::I32Const(36), PlanToken::I32Add,
        PlanToken::MemoryCopy { source: 0, destination: 0 }]);
    if !offsets.is_empty() {
        let retain = retain.expect("owned dictionary slots require a retain helper");
        plan.extend([PlanToken::I32Const(0), PlanToken::LocalSet(local.scratch[4]),
            PlanToken::Block("Empty".into()), PlanToken::Loop("Empty".into()),
            PlanToken::LocalGet(local.scratch[4])]);
        push_get(plan, local, capacity)?;
        plan.extend([PlanToken::I32GeU, PlanToken::BrIf(1)]);
        push_get(plan, local, destination)?;
        plan.extend([PlanToken::I32Const(20), PlanToken::I32Add,
            PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(32), PlanToken::I32Mul,
            PlanToken::I32Add, PlanToken::LocalSet(local.scratch[5]),
            PlanToken::LocalGet(local.scratch[5]), PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
            PlanToken::I32Const(1), PlanToken::I32Eq, PlanToken::If("Empty".into())]);
        for offset in offsets {
            plan.extend([PlanToken::LocalGet(local.scratch[5]), PlanToken::I32Const(offset), PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::Call(retain), PlanToken::Drop]);
        }
        plan.extend([PlanToken::End, PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(1),
            PlanToken::I32Add, PlanToken::LocalSet(local.scratch[4]), PlanToken::Br(0), PlanToken::End, PlanToken::End]);
    }
    Ok(())
}

fn emit_rehash(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
    transfer: crate::canonical_safewir::dictionary::DictionaryRehashTransfer,
) -> Result<(), EncodeError> {
    use crate::canonical_safewir::dictionary::DictionaryRehashTransfer as T;
    let offsets = pointer_offsets(function, layouts, source)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if transfer == T::CloneShared && !offsets.is_empty() && retain.is_none() {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary rehash retain" });
    }
    emit_capacity_guard(body, function, local, capacity)?;
    // A spare empty bucket guarantees termination of linear probing.
    emit_get(body, local, function.id, source)?;
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32GeU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary rehash destination" });
    };
    emit_alloc(body, function, local, layouts, destination, layout_id, &counted_extent(capacity))?;
    emit_get(body, local, function.id, destination)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(20));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::MemoryFill(0));
    emit_fill_controls(body, function, local, destination, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    emit_set_index(body, local.scratch[11]);
    emit_get(body, local, function.id, destination)?;
    emit_set_index(body, local.scratch[7]);
    for offset in [0, 8, 12, 16] { emit_i32_store(body, local.scratch[7], offset, local.scratch[11]); }
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    emit_set_index(body, local.scratch[10]);
    emit_get(body, local, function.id, source)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, local.scratch[4]);
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    emit_get_index(body, local.scratch[4]);
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::BrIf(1));
    emit_get(body, local, function.id, source)?;
    emit_get_index(body, local.scratch[4]);
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(20));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, local.scratch[5]);
    emit_get_index(body, local.scratch[5]);
    body.instruction(&wasm_encoder::Instruction::I32Const(4));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_get_index(body, local.scratch[10]);
    body.instruction(&wasm_encoder::Instruction::I32And);
    emit_set_index(body, local.scratch[6]);
    body.instruction(&wasm_encoder::Instruction::Block(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Loop(BlockType::Empty));
    emit_get(body, local, function.id, destination)?;
    emit_get_index(body, local.scratch[6]);
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(20));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, local.scratch[7]);
    emit_get_index(body, local.scratch[7]);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::BrIf(1));
    emit_get_index(body, local.scratch[6]);
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_get_index(body, local.scratch[10]);
    body.instruction(&wasm_encoder::Instruction::I32And);
    emit_set_index(body, local.scratch[6]);
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get_index(body, local.scratch[7]);
    emit_get_index(body, local.scratch[5]);
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
    emit_get_index(body, local.scratch[5]);
    body.instruction(&wasm_encoder::Instruction::I32Load(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
    body.instruction(&wasm_encoder::Instruction::I32Const(25));
    body.instruction(&wasm_encoder::Instruction::I32ShrU);
    emit_set_index(body, local.scratch[11]);
    emit_write_control(body, function, local, destination, local.scratch[6], local.scratch[11])?;
    if transfer == T::CloneShared {
        if let Some(retain) = retain {
            for offset in offsets {
                emit_get_index(body, local.scratch[7]);
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                body.instruction(&wasm_encoder::Instruction::Call(retain));
                body.instruction(&wasm_encoder::Instruction::Drop);
            }
        }
    }
    emit_get(body, local, function.id, destination)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(12));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, local.scratch[8]);
    emit_get_index(body, local.scratch[6]);
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, local.scratch[9]);
    emit_i32_store(body, local.scratch[7], 12, local.scratch[8]);
    body.instruction(&wasm_encoder::Instruction::I32Const(0));
    emit_set_index(body, local.scratch[11]);
    emit_i32_store(body, local.scratch[7], 8, local.scratch[11]);
    emit_get_index(body, local.scratch[8]);
    body.instruction(&wasm_encoder::Instruction::I32Eqz);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_get(body, local, function.id, destination)?;
    emit_set_index(body, local.scratch[11]);
    emit_i32_store(body, local.scratch[11], 8, local.scratch[9]);
    body.instruction(&wasm_encoder::Instruction::Else);
    emit_get(body, local, function.id, destination)?;
    emit_get_index(body, local.scratch[8]);
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Const(28));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    emit_set_index(body, local.scratch[11]);
    emit_i32_store(body, local.scratch[11], 0, local.scratch[9]);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, local, function.id, destination)?;
    emit_set_index(body, local.scratch[11]);
    emit_i32_store(body, local.scratch[11], 12, local.scratch[9]);
    emit_get_index(body, local.scratch[5]);
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    emit_set_index(body, local.scratch[4]);
    body.instruction(&wasm_encoder::Instruction::Br(0));
    body.instruction(&wasm_encoder::Instruction::End);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, local, function.id, destination)?;
    emit_get(body, local, function.id, source)?;
    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
    if transfer == T::MoveUnique { emit_reclaim(body, local, function, source)?; }
    Ok(())
}

fn plan_rehash(plan: &mut Vec<PlanToken>, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
    transfer: crate::canonical_safewir::dictionary::DictionaryRehashTransfer,
) -> Result<(), CanonicalAdmissionError> {
    use crate::canonical_safewir::dictionary::DictionaryRehashTransfer as T;
    use PlanToken as P;
    let offsets = pointer_offsets(function, layouts, source).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if transfer == T::CloneShared && !offsets.is_empty() && retain.is_none() { return Err(CanonicalAdmissionError::UnsupportedPlan); }
    plan_capacity_guard(plan, local, capacity)?;
    push_get(plan, local, source)?;
    plan.push(P::I32Load { offset: 0, align: 2, memory: 0 });
    push_get(plan, local, capacity)?;
    plan.extend([P::I32GeU, P::If("Empty".into()), P::Unreachable, P::End]);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else { return Err(CanonicalAdmissionError::UnsupportedPlan); };
    let collection = collection_layout(function.id, layouts, layout_id).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    expected_alloc_plan(plan, function, local, Some(collection), destination, &counted_extent(capacity))?;
    push_get(plan, local, destination)?;
    plan.extend([P::I32Const(20), P::I32Add, P::I32Const(0)]);
    push_get(plan, local, capacity)?;
    plan.extend([P::I32Const(32), P::I32Mul, P::MemoryFill(0)]);
    plan_fill_controls(plan, local, destination, capacity)?;
    plan.extend([P::I32Const(0), P::LocalSet(local.scratch[11])]);
    push_get(plan, local, destination)?;
    plan.push(P::LocalSet(local.scratch[7]));
    for offset in [0, 8, 12, 16] { plan_i32_store(plan, local.scratch[7], offset, local.scratch[11]); }
    push_get(plan, local, capacity)?;
    plan.extend([P::I32Const(1), P::I32Sub, P::LocalSet(local.scratch[10])]);
    push_get(plan, local, source)?;
    plan.extend([P::I32Const(8), P::I32Add, P::I32Load { offset: 0, align: 2, memory: 0 },
        P::LocalSet(local.scratch[4]), P::Block("Empty".into()), P::Loop("Empty".into()),
        P::LocalGet(local.scratch[4]), P::I32Eqz, P::BrIf(1)]);
    push_get(plan, local, source)?;
    plan.extend([P::LocalGet(local.scratch[4]), P::I32Const(1), P::I32Sub, P::I32Const(32),
        P::I32Mul, P::I32Add, P::I32Const(20), P::I32Add, P::LocalSet(local.scratch[5]),
        P::LocalGet(local.scratch[5]), P::I32Const(4), P::I32Add,
        P::I32Load { offset: 0, align: 2, memory: 0 }, P::LocalGet(local.scratch[10]),
        P::I32And, P::LocalSet(local.scratch[6]), P::Block("Empty".into()), P::Loop("Empty".into())]);
    push_get(plan, local, destination)?;
    plan.extend([P::LocalGet(local.scratch[6]), P::I32Const(32), P::I32Mul, P::I32Add,
        P::I32Const(20), P::I32Add, P::LocalSet(local.scratch[7]),
        P::LocalGet(local.scratch[7]), P::I32Load { offset: 0, align: 2, memory: 0 },
        P::I32Eqz, P::BrIf(1), P::LocalGet(local.scratch[6]), P::I32Const(1), P::I32Add,
        P::LocalGet(local.scratch[10]), P::I32And, P::LocalSet(local.scratch[6]),
        P::Br(0), P::End, P::End,
        P::LocalGet(local.scratch[7]), P::LocalGet(local.scratch[5]), P::I32Const(32),
        P::MemoryCopy { source: 0, destination: 0 }]);
    plan.extend([P::LocalGet(local.scratch[5]), P::I32Load { offset: 4, align: 2, memory: 0 },
        P::I32Const(25), P::I32ShrU, P::LocalSet(local.scratch[11])]);
    plan_write_control(plan, local, destination, local.scratch[6], local.scratch[11])?;
    if transfer == T::CloneShared {
        if let Some(retain) = retain {
            for offset in offsets {
                plan.extend([P::LocalGet(local.scratch[7]), P::I32Const(offset), P::I32Add,
                    P::I32Load { offset: 0, align: 2, memory: 0 }, P::Call(retain), P::Drop]);
            }
        }
    }
    push_get(plan, local, destination)?;
    plan.extend([P::I32Const(12), P::I32Add, P::I32Load { offset: 0, align: 2, memory: 0 },
        P::LocalSet(local.scratch[8]), P::LocalGet(local.scratch[6]), P::I32Const(1),
        P::I32Add, P::LocalSet(local.scratch[9])]);
    plan_i32_store(plan, local.scratch[7], 12, local.scratch[8]);
    plan.extend([P::I32Const(0), P::LocalSet(local.scratch[11])]);
    plan_i32_store(plan, local.scratch[7], 8, local.scratch[11]);
    plan.extend([P::LocalGet(local.scratch[8]), P::I32Eqz, P::If("Empty".into())]);
    push_get(plan, local, destination)?;
    plan.push(P::LocalSet(local.scratch[11]));
    plan_i32_store(plan, local.scratch[11], 8, local.scratch[9]);
    plan.push(P::Else);
    push_get(plan, local, destination)?;
    plan.extend([P::LocalGet(local.scratch[8]), P::I32Const(1), P::I32Sub,
        P::I32Const(32), P::I32Mul, P::I32Add, P::I32Const(28), P::I32Add,
        P::LocalSet(local.scratch[11])]);
    plan_i32_store(plan, local.scratch[11], 0, local.scratch[9]);
    plan.push(P::End);
    push_get(plan, local, destination)?;
    plan.push(P::LocalSet(local.scratch[11]));
    plan_i32_store(plan, local.scratch[11], 12, local.scratch[9]);
    plan.extend([P::LocalGet(local.scratch[5]), P::I32Const(8), P::I32Add,
        P::I32Load { offset: 0, align: 2, memory: 0 }, P::LocalSet(local.scratch[4]),
        P::Br(0), P::End, P::End]);
    push_get(plan, local, destination)?;
    push_get(plan, local, source)?;
    plan.extend([P::I32Load { offset: 0, align: 2, memory: 0 },
        P::I32Store { offset: 0, align: 2, memory: 0 }]);
    if transfer == T::MoveUnique { push_reclaim_plan(plan, local, source)?; }
    Ok(())
}

fn emit_capacity_guard(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, capacity: ValueId) -> Result<(), EncodeError> {
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(16));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    emit_get(body, local, function.id, capacity)?;
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(1));
    body.instruction(&wasm_encoder::Instruction::I32Sub);
    body.instruction(&wasm_encoder::Instruction::I32And);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn plan_capacity_guard(plan: &mut Vec<PlanToken>, local: &LocalLayout, capacity: ValueId) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Const(16), PlanToken::I32LtU, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
    push_get(plan, local, capacity)?;
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Const(1), PlanToken::I32Sub, PlanToken::I32And, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
    Ok(())
}

pub(super) fn emit(
    body: &mut WasmFunctionEncoder,
    function: &Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    operation: &DictionaryOperation,
) -> Result<(), EncodeError> {
    match operation {
        DictionaryOperation::Rehash { destination, source, capacity, transfer } =>
            emit_rehash(body, function, local, layouts, *destination, *source, *capacity, *transfer),
        DictionaryOperation::CloneExact { destination, source, capacity } =>
            emit_clone_exact(body, function, local, layouts, *destination, *source, *capacity),
        DictionaryOperation::Initialize { destination, layout, capacity } => {
            emit_capacity_guard(body, function, local, *capacity)?;
            emit_alloc(body, function, local, layouts, *destination, *layout, &counted_extent(*capacity))?;
            emit_get(body, local, function.id, *destination)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            emit_get(body, local, function.id, *destination)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(8));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            emit_get(body, local, function.id, *destination)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(12));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            emit_get(body, local, function.id, *destination)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(16));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            // Reused free-list blocks may carry live-looking row states.
            // Clearing the full row extent also gives scalar slots a defined
            // zero representation before any insertion.
            emit_get(body, local, function.id, *destination)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(20));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_get(body, local, function.id, *capacity)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::MemoryFill(0));
            emit_fill_controls(body, function, local, *destination, *capacity)?;
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(header_offset(*field)));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set(body, local, function.id, *destination)
        }
        DictionaryOperation::ProbeGroup { dict, bucket, fingerprint, matching, empty, deleted } => {
            emit_get(body, local, function.id, *bucket)?;
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Load(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
            body.instruction(&wasm_encoder::Instruction::I32GeU);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            emit_control_base(body, function, local, *dict)?;
            emit_get(body, local, function.id, *bucket)?;
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[4]);
            for (destination, marker) in [(*matching, None), (*empty, Some(0x80)), (*deleted, Some(0xFE))] {
                emit_get_index(body, local.scratch[4]);
                body.instruction(&wasm_encoder::Instruction::V128Load(memarg(1)));
                if let Some(marker) = marker {
                    body.instruction(&wasm_encoder::Instruction::I32Const(marker));
                } else {
                    emit_get(body, local, function.id, *fingerprint)?;
                }
                body.instruction(&wasm_encoder::Instruction::I8x16Splat);
                body.instruction(&wasm_encoder::Instruction::I8x16Eq);
                body.instruction(&wasm_encoder::Instruction::I8x16Bitmask);
                emit_set(body, local, function.id, destination)?;
            }
            Ok(())
        }
        DictionaryOperation::ReadSlot { destination, dict, bucket, field, transfer } => {
            emit_slot_address(body, function, local, *dict, *bucket, *field)?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(slot_offset(*field)));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            match function.value_types.get(destination) {
                Some(ValueType::Int) => {
                    if *field == crate::canonical_safewir::dictionary::DictionarySlotField::Hash {
                        body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                        body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
                    } else {
                        body.instruction(&wasm_encoder::Instruction::I64Load(memarg(8)));
                    }
                }
                Some(ValueType::Float) => { body.instruction(&wasm_encoder::Instruction::F64Load(memarg(8))); }
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => {
                    body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                }
                _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary ReadSlot type" }),
            };
            emit_set(body, local, function.id, *destination)?;
            if *transfer == LoadTransfer::CloneShared {
                emit_clone_ref(body, local, *destination, *destination, function)?;
            }
            Ok(())
        }
        DictionaryOperation::ReplaceValue { displaced, dict, bucket, value, transfer } => {
            emit_slot_address(body, function, local, *dict, *bucket,
                crate::canonical_safewir::dictionary::DictionarySlotField::Value)?;
            if *transfer == StoreTransfer::CloneShared {
                emit_clone_ref(body, local, *value, *value, function)?;
            }
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(24));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&match function.value_types.get(displaced) {
                Some(ValueType::Int) => wasm_encoder::Instruction::I64Load(memarg(8)),
                Some(ValueType::Float) => wasm_encoder::Instruction::F64Load(memarg(8)),
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => wasm_encoder::Instruction::I32Load(memarg(4)),
                _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary displaced type" }),
            });
            emit_set(body, local, function.id, *displaced)?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(24));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[5]);
            emit_store_value(body, function, local, *value, local.scratch[5])
        }
        DictionaryOperation::InsertSlot { dict, bucket, key, value, hash, key_transfer, value_transfer } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            emit_slot_address(body, function, local, *dict, *bucket, DictionarySlotField::State)?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set_index(body, local.scratch[5]);
            emit_get_index(body, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Eq);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set_index(body, local.scratch[10]);
            emit_get_index(body, local.scratch[10]);
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(4));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            body.instruction(&wasm_encoder::Instruction::I32GeU);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            body.instruction(&wasm_encoder::Instruction::Unreachable);
            body.instruction(&wasm_encoder::Instruction::End);
            if *key_transfer == StoreTransfer::CloneShared { emit_clone_ref(body, local, *key, *key, function)?; }
            if *value_transfer == StoreTransfer::CloneShared { emit_clone_ref(body, local, *value, *value, function)?; }
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(12));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set_index(body, local.scratch[6]);
            emit_get(body, local, function.id, *bucket)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[7]);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_set_index(body, local.scratch[9]);
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(16));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[8]);
            emit_store_value(body, function, local, *key, local.scratch[8])?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(24));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[8]);
            emit_store_value(body, function, local, *value, local.scratch[8])?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(4));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_get(body, local, function.id, *hash)?;
            body.instruction(&wasm_encoder::Instruction::I32WrapI64);
            body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
            emit_i32_store(body, local.scratch[4], 8, local.scratch[9]);
            emit_i32_store(body, local.scratch[4], 12, local.scratch[6]);
            emit_get_index(body, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::I32Eqz);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            emit_get(body, local, function.id, *dict)?;
            emit_set_index(body, local.scratch[8]);
            emit_i32_store(body, local.scratch[8], 8, local.scratch[7]);
            body.instruction(&wasm_encoder::Instruction::Else);
            emit_get(body, local, function.id, *dict)?;
            emit_get_index(body, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Sub);
            body.instruction(&wasm_encoder::Instruction::I32Const(32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(28));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[8]);
            emit_i32_store(body, local.scratch[8], 0, local.scratch[7]);
            body.instruction(&wasm_encoder::Instruction::End);
            emit_get(body, local, function.id, *dict)?;
            emit_set_index(body, local.scratch[8]);
            emit_i32_store(body, local.scratch[8], 12, local.scratch[7]);
            emit_get_index(body, local.scratch[10]);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[10]);
            emit_i32_store(body, local.scratch[8], 0, local.scratch[10]);
            emit_get_index(body, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::I32Const(2));
            body.instruction(&wasm_encoder::Instruction::I32Eq);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            emit_get_index(body, local.scratch[8]);
            body.instruction(&wasm_encoder::Instruction::I32Const(16));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Sub);
            emit_set_index(body, local.scratch[11]);
            emit_i32_store(body, local.scratch[8], 16, local.scratch[11]);
            body.instruction(&wasm_encoder::Instruction::End);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            emit_set_index(body, local.scratch[9]);
            emit_i32_store(body, local.scratch[4], 0, local.scratch[9]);
            emit_get(body, local, function.id, *hash)?;
            body.instruction(&wasm_encoder::Instruction::I32WrapI64);
            body.instruction(&wasm_encoder::Instruction::I32Const(25));
            body.instruction(&wasm_encoder::Instruction::I32ShrU);
            emit_set_index(body, local.scratch[9]);
            emit_get(body, local, function.id, *bucket)?;
            emit_set_index(body, local.scratch[11]);
            emit_write_control(body, function, local, *dict, local.scratch[11], local.scratch[9])?;
            Ok(())
        }
        DictionaryOperation::RemoveSlot { removed_value, dict, bucket } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            let key_drop = key_pointee(function, layouts, *dict)?
                .map(|pointee| local.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&pointee).copied())
                    .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary key drop" }))
                .transpose()?;
            emit_slot_address(body, function, local, *dict, *bucket, DictionarySlotField::Value)?;
            emit_get_index(body, local.scratch[4]);
            body.instruction(&wasm_encoder::Instruction::I32Const(24));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&match function.value_types.get(removed_value) {
                Some(ValueType::Int) => wasm_encoder::Instruction::I64Load(memarg(8)),
                Some(ValueType::Float) => wasm_encoder::Instruction::F64Load(memarg(8)),
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => wasm_encoder::Instruction::I32Load(memarg(4)),
                _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary remove value" }),
            });
            emit_set(body, local, function.id, *removed_value)?;
            if key_drop.is_some() {
                emit_get_index(body, local.scratch[4]);
                body.instruction(&wasm_encoder::Instruction::I32Const(16));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                emit_set_index(body, local.scratch[11]);
            }
            for (offset, scratch) in [(8, 5), (12, 6)] {
                emit_get_index(body, local.scratch[4]);
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                emit_set_index(body, local.scratch[scratch]);
            }
            emit_get(body, local, function.id, *dict)?;
            emit_set_index(body, local.scratch[8]);
            body.instruction(&wasm_encoder::Instruction::I32Const(0));
            emit_set_index(body, local.scratch[9]);
            // Previous row's next, or header head.
            emit_get_index(body, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::I32Eqz);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            emit_i32_store(body, local.scratch[8], 8, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::Else);
            emit_get_index(body, local.scratch[8]);
            emit_get_index(body, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Sub);
            body.instruction(&wasm_encoder::Instruction::I32Const(32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(28));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[7]);
            emit_i32_store(body, local.scratch[7], 0, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::End);
            // Next row's prev, or header tail.
            emit_get_index(body, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::I32Eqz);
            body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
            emit_i32_store(body, local.scratch[8], 12, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::Else);
            emit_get_index(body, local.scratch[8]);
            emit_get_index(body, local.scratch[5]);
            body.instruction(&wasm_encoder::Instruction::I32Const(1));
            body.instruction(&wasm_encoder::Instruction::I32Sub);
            body.instruction(&wasm_encoder::Instruction::I32Const(32));
            body.instruction(&wasm_encoder::Instruction::I32Mul);
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Const(32));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            emit_set_index(body, local.scratch[7]);
            emit_i32_store(body, local.scratch[7], 0, local.scratch[6]);
            body.instruction(&wasm_encoder::Instruction::End);
            for (offset, delta) in [(0, -1), (16, 1)] {
                emit_get_index(body, local.scratch[8]);
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
                body.instruction(&wasm_encoder::Instruction::I32Const(delta));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                emit_set_index(body, local.scratch[10]);
                emit_i32_store(body, local.scratch[8], offset, local.scratch[10]);
            }
            body.instruction(&wasm_encoder::Instruction::I32Const(2));
            emit_set_index(body, local.scratch[10]);
            emit_i32_store(body, local.scratch[4], 0, local.scratch[10]);
            emit_get(body, local, function.id, *bucket)?;
            emit_set_index(body, local.scratch[7]);
            body.instruction(&wasm_encoder::Instruction::I32Const(0xFE));
            emit_set_index(body, local.scratch[10]);
            emit_write_control(body, function, local, *dict, local.scratch[7], local.scratch[10])?;
            for offset in [16, 24] {
                emit_get_index(body, local.scratch[4]);
                body.instruction(&wasm_encoder::Instruction::I32Const(offset));
                body.instruction(&wasm_encoder::Instruction::I32Add);
                body.instruction(&wasm_encoder::Instruction::I64Const(0));
                body.instruction(&wasm_encoder::Instruction::I64Store(memarg(8)));
            }
            if let Some(drop) = key_drop {
                emit_get_index(body, local.scratch[11]);
                body.instruction(&wasm_encoder::Instruction::Call(drop));
            }
            Ok(())
        }
        _ => Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary" }),
    }
}

pub(super) fn plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    layouts: &LayoutInterner,
    local: &LocalLayout,
    operation: &DictionaryOperation,
) -> Result<(), CanonicalAdmissionError> {
    match operation {
        DictionaryOperation::Rehash { destination, source, capacity, transfer } =>
            plan_rehash(plan, function, local, layouts, *destination, *source, *capacity, *transfer),
        DictionaryOperation::CloneExact { destination, source, capacity } =>
            plan_clone_exact(plan, function, local, layouts, *destination, *source, *capacity),
        DictionaryOperation::Initialize { destination, layout, capacity } => {
            plan_capacity_guard(plan, local, *capacity)?;
            let collection = collection_layout(function.id, layouts, *layout).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            expected_alloc_plan(plan, function, local, Some(collection), *destination, &counted_extent(*capacity))?;
            for offset in [0, 8, 12, 16] {
                push_get(plan, local, *destination)?;
                if offset != 0 { plan.extend([PlanToken::I32Const(offset), PlanToken::I32Add]); }
                plan.extend([PlanToken::I32Const(0), PlanToken::I32Store { offset: 0, align: 2, memory: 0 }]);
            }
            push_get(plan, local, *destination)?;
            plan.extend([PlanToken::I32Const(20), PlanToken::I32Add, PlanToken::I32Const(0)]);
            push_get(plan, local, *capacity)?;
            plan.extend([PlanToken::I32Const(32), PlanToken::I32Mul, PlanToken::MemoryFill(0)]);
            plan_fill_controls(plan, local, *destination, *capacity)?;
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Const(header_offset(*field)), PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 }]);
            push_set(plan, local, *destination)
        }
        DictionaryOperation::ProbeGroup { dict, bucket, fingerprint, matching, empty, deleted } => {
            push_get(plan, local, *bucket)?;
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Load { offset: 4, align: 2, memory: 0 },
                PlanToken::I32GeU, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
            plan_control_base(plan, local, *dict)?;
            push_get(plan, local, *bucket)?;
            plan.extend([PlanToken::I32Add, PlanToken::LocalSet(local.scratch[4])]);
            for (destination, marker) in [(*matching, None), (*empty, Some(0x80)), (*deleted, Some(0xFE))] {
                plan.extend([PlanToken::LocalGet(local.scratch[4]),
                    PlanToken::V128Load { offset: 0, align: 0, memory: 0 }]);
                if let Some(marker) = marker {
                    plan.push(PlanToken::I32Const(marker));
                } else {
                    push_get(plan, local, *fingerprint)?;
                }
                plan.extend([PlanToken::I8x16Splat, PlanToken::I8x16Eq,
                    PlanToken::I8x16Bitmask]);
                push_set(plan, local, destination)?;
            }
            Ok(())
        }
        DictionaryOperation::ReadSlot { destination, dict, bucket, field, transfer } => {
            plan_slot_address(plan, local, *dict, *bucket, *field)?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(slot_offset(*field)), PlanToken::I32Add]);
            match function.value_types.get(destination) {
                Some(ValueType::Int) => {
                    if *field == crate::canonical_safewir::dictionary::DictionarySlotField::Hash {
                        plan.extend([PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I64ExtendI32U]);
                    } else {
                        plan.push(PlanToken::I64Load { offset: 0, align: 3, memory: 0 });
                    }
                }
                Some(ValueType::Float) => plan.push(PlanToken::F64Load { offset: 0, align: 3, memory: 0 }),
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) =>
                    plan.push(PlanToken::I32Load { offset: 0, align: 2, memory: 0 }),
                _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
            }
            push_set(plan, local, *destination)?;
            if *transfer == LoadTransfer::CloneShared { push_share_plan(plan, local, *destination, *destination)?; }
            Ok(())
        }
        DictionaryOperation::ReplaceValue { displaced, dict, bucket, value, transfer } => {
            plan_slot_address(plan, local, *dict, *bucket,
                crate::canonical_safewir::dictionary::DictionarySlotField::Value)?;
            if *transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *value, *value)?; }
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(24), PlanToken::I32Add]);
            plan.push(match function.value_types.get(displaced) {
                Some(ValueType::Int) => PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Float) => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
            });
            push_set(plan, local, *displaced)?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(24),
                PlanToken::I32Add, PlanToken::LocalSet(local.scratch[5])]);
            plan_store_value(plan, function, local, *value, local.scratch[5])
        }
        DictionaryOperation::InsertSlot { dict, bucket, key, value, hash, key_transfer, value_transfer } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            plan_slot_address(plan, local, *dict, *bucket, DictionarySlotField::State)?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::LocalSet(local.scratch[5]), PlanToken::LocalGet(local.scratch[5]),
                PlanToken::I32Const(1), PlanToken::I32Eq, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::LocalSet(local.scratch[10]),
                PlanToken::LocalGet(local.scratch[10])]);
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Const(4), PlanToken::I32Add, PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::I32GeU, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
            if *key_transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *key, *key)?; }
            if *value_transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *value, *value)?; }
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Const(12), PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::LocalSet(local.scratch[6])]);
            push_get(plan, local, *bucket)?;
            plan.extend([PlanToken::I32Const(1), PlanToken::I32Add, PlanToken::LocalSet(local.scratch[7]),
                PlanToken::I32Const(0), PlanToken::LocalSet(local.scratch[9]),
                PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(16), PlanToken::I32Add,
                PlanToken::LocalSet(local.scratch[8])]);
            plan_store_value(plan, function, local, *key, local.scratch[8])?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(24), PlanToken::I32Add,
                PlanToken::LocalSet(local.scratch[8])]);
            plan_store_value(plan, function, local, *value, local.scratch[8])?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(4), PlanToken::I32Add]);
            push_get(plan, local, *hash)?;
            plan.extend([PlanToken::I32WrapI64, PlanToken::I32Store { offset: 0, align: 2, memory: 0 }]);
            plan_i32_store(plan, local.scratch[4], 8, local.scratch[9]);
            plan_i32_store(plan, local.scratch[4], 12, local.scratch[6]);
            plan.extend([PlanToken::LocalGet(local.scratch[6]), PlanToken::I32Eqz, PlanToken::If("Empty".into())]);
            push_get(plan, local, *dict)?;
            plan.push(PlanToken::LocalSet(local.scratch[8]));
            plan_i32_store(plan, local.scratch[8], 8, local.scratch[7]);
            plan.push(PlanToken::Else);
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::LocalGet(local.scratch[6]), PlanToken::I32Const(1), PlanToken::I32Sub,
                PlanToken::I32Const(32), PlanToken::I32Mul, PlanToken::I32Add,
                PlanToken::I32Const(28), PlanToken::I32Add, PlanToken::LocalSet(local.scratch[8])]);
            plan_i32_store(plan, local.scratch[8], 0, local.scratch[7]);
            plan.push(PlanToken::End);
            push_get(plan, local, *dict)?;
            plan.push(PlanToken::LocalSet(local.scratch[8]));
            plan_i32_store(plan, local.scratch[8], 12, local.scratch[7]);
            plan.extend([PlanToken::LocalGet(local.scratch[10]), PlanToken::I32Const(1), PlanToken::I32Add,
                PlanToken::LocalSet(local.scratch[10])]);
            plan_i32_store(plan, local.scratch[8], 0, local.scratch[10]);
            plan.extend([PlanToken::LocalGet(local.scratch[5]), PlanToken::I32Const(2), PlanToken::I32Eq,
                PlanToken::If("Empty".into()), PlanToken::LocalGet(local.scratch[8]),
                PlanToken::I32Const(16), PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                PlanToken::I32Const(1), PlanToken::I32Sub, PlanToken::LocalSet(local.scratch[11])]);
            plan_i32_store(plan, local.scratch[8], 16, local.scratch[11]);
            plan.extend([PlanToken::End, PlanToken::I32Const(1), PlanToken::LocalSet(local.scratch[9])]);
            plan_i32_store(plan, local.scratch[4], 0, local.scratch[9]);
            push_get(plan, local, *hash)?;
            plan.extend([PlanToken::I32WrapI64, PlanToken::I32Const(25), PlanToken::I32ShrU,
                PlanToken::LocalSet(local.scratch[9])]);
            push_get(plan, local, *bucket)?;
            plan.push(PlanToken::LocalSet(local.scratch[11]));
            plan_write_control(plan, local, *dict, local.scratch[11], local.scratch[9])?;
            Ok(())
        }
        DictionaryOperation::RemoveSlot { removed_value, dict, bucket } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            let key_drop = key_pointee(function, layouts, *dict).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?
                .map(|pointee| local.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&pointee).copied())
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)).transpose()?;
            plan_slot_address(plan, local, *dict, *bucket, DictionarySlotField::Value)?;
            plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(24), PlanToken::I32Add]);
            plan.push(match function.value_types.get(removed_value) {
                Some(ValueType::Int) => PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Float) => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) =>
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
            });
            push_set(plan, local, *removed_value)?;
            if key_drop.is_some() {
                plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(16), PlanToken::I32Add,
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::LocalSet(local.scratch[11])]);
            }
            for (offset, scratch) in [(8, 5), (12, 6)] {
                plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(offset), PlanToken::I32Add,
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::LocalSet(local.scratch[scratch])]);
            }
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::LocalSet(local.scratch[8]), PlanToken::I32Const(0), PlanToken::LocalSet(local.scratch[9]),
                PlanToken::LocalGet(local.scratch[6]), PlanToken::I32Eqz, PlanToken::If("Empty".into())]);
            plan_i32_store(plan, local.scratch[8], 8, local.scratch[5]);
            plan.push(PlanToken::Else);
            plan.extend([PlanToken::LocalGet(local.scratch[8]), PlanToken::LocalGet(local.scratch[6]),
                PlanToken::I32Const(1), PlanToken::I32Sub, PlanToken::I32Const(32), PlanToken::I32Mul,
                PlanToken::I32Add, PlanToken::I32Const(28), PlanToken::I32Add,
                PlanToken::LocalSet(local.scratch[7])]);
            plan_i32_store(plan, local.scratch[7], 0, local.scratch[5]);
            plan.extend([PlanToken::End, PlanToken::LocalGet(local.scratch[5]), PlanToken::I32Eqz,
                PlanToken::If("Empty".into())]);
            plan_i32_store(plan, local.scratch[8], 12, local.scratch[6]);
            plan.push(PlanToken::Else);
            plan.extend([PlanToken::LocalGet(local.scratch[8]), PlanToken::LocalGet(local.scratch[5]),
                PlanToken::I32Const(1), PlanToken::I32Sub, PlanToken::I32Const(32), PlanToken::I32Mul,
                PlanToken::I32Add, PlanToken::I32Const(32), PlanToken::I32Add,
                PlanToken::LocalSet(local.scratch[7])]);
            plan_i32_store(plan, local.scratch[7], 0, local.scratch[6]);
            plan.push(PlanToken::End);
            for (offset, delta) in [(0, -1), (16, 1)] {
                plan.extend([PlanToken::LocalGet(local.scratch[8]), PlanToken::I32Const(offset), PlanToken::I32Add,
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I32Const(delta),
                    PlanToken::I32Add, PlanToken::LocalSet(local.scratch[10])]);
                plan_i32_store(plan, local.scratch[8], offset, local.scratch[10]);
            }
            plan.extend([PlanToken::I32Const(2), PlanToken::LocalSet(local.scratch[10])]);
            plan_i32_store(plan, local.scratch[4], 0, local.scratch[10]);
            push_get(plan, local, *bucket)?;
            plan.extend([PlanToken::LocalSet(local.scratch[7]), PlanToken::I32Const(0xFE),
                PlanToken::LocalSet(local.scratch[10])]);
            plan_write_control(plan, local, *dict, local.scratch[7], local.scratch[10])?;
            for offset in [16, 24] {
                plan.extend([PlanToken::LocalGet(local.scratch[4]), PlanToken::I32Const(offset),
                    PlanToken::I32Add, PlanToken::I64Const(0),
                    PlanToken::I64Store { offset: 0, align: 3, memory: 0 }]);
            }
            if let Some(drop) = key_drop { plan.extend([PlanToken::LocalGet(local.scratch[11]), PlanToken::Call(drop)]); }
            Ok(())
        }
        _ => Err(CanonicalAdmissionError::UnsupportedPlan),
    }
}
