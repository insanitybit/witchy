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

// Interior addresses. Under `Byte` a cached slot or control address is the
// i32 byte address in a scratch slot, as before. Under `Scaled8` it is the
// i64 byte address in one of the function's wide locals, because the heap
// extends past 4 GiB. Each address is formed by `RefEncoding::base` of its
// dictionary reference plus `add` of each i32 offset, so `Byte` emits the
// exact i32 sums it always did.

fn wide_slot(slot: usize) -> Option<usize> {
    match slot { 4 => Some(0), 5 => Some(1), 7 => Some(2), 8 => Some(3), 11 => Some(4), _ => None }
}

/// The local that holds an interior address for scratch `slot`.
fn address_local(local: &LocalLayout, slot: usize) -> Option<u32> {
    match local.refs {
        ref_encoding::RefEncoding::Byte => local.scratch.get(slot).copied(),
        ref_encoding::RefEncoding::Scaled8 => wide_slot(slot).and_then(|wide| local.wide.get(wide)).copied(),
    }
}

fn emit_address_local(function: &Function, local: &LocalLayout, slot: usize) -> Result<u32, EncodeError> {
    address_local(local, slot).ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary address local" })
}

fn plan_address_local(local: &LocalLayout, slot: usize) -> Result<u32, CanonicalAdmissionError> {
    address_local(local, slot).ok_or(CanonicalAdmissionError::UnsupportedPlan)
}

/// Start an address at the object `value` names.
fn emit_start(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, value: ValueId) -> Result<(), EncodeError> {
    emit_get(body, local, function.id, value)?;
    local.refs.base(body);
    Ok(())
}

/// Add an i32 constant offset to the address being formed.
fn emit_offset(body: &mut WasmFunctionEncoder, local: &LocalLayout, offset: i32) {
    body.instruction(&wasm_encoder::Instruction::I32Const(offset));
    local.refs.add(body);
}

/// The memory index of `value`'s object plus `offset`; a zero offset adds
/// nothing.
fn emit_at(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, value: ValueId, offset: i32) -> Result<(), EncodeError> {
    if offset == 0 {
        emit_get(body, local, function.id, value)?;
        local.refs.object(body);
    } else {
        emit_start(body, function, local, value)?;
        emit_offset(body, local, offset);
        local.refs.finish(body);
    }
    Ok(())
}

/// As `emit_at`, adding the offset even when it is zero.
fn emit_at_const(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, value: ValueId, offset: i32) -> Result<(), EncodeError> {
    emit_start(body, function, local, value)?;
    emit_offset(body, local, offset);
    local.refs.finish(body);
    Ok(())
}

/// The memory index of the address in `address` plus `offset`; a zero
/// offset adds nothing.
fn emit_address_at(body: &mut WasmFunctionEncoder, local: &LocalLayout, address: u32, offset: i32) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(address));
    if offset != 0 { emit_offset(body, local, offset); }
    local.refs.finish(body);
}

/// As `emit_address_at`, adding the offset even when it is zero.
fn emit_address_at_const(body: &mut WasmFunctionEncoder, local: &LocalLayout, address: u32, offset: i32) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(address));
    emit_offset(body, local, offset);
    local.refs.finish(body);
}

/// `to` = the address in `from` plus `offset`.
fn emit_copy_address(body: &mut WasmFunctionEncoder, local: &LocalLayout, from: u32, offset: i32, to: u32) {
    body.instruction(&wasm_encoder::Instruction::LocalGet(from));
    emit_offset(body, local, offset);
    body.instruction(&wasm_encoder::Instruction::LocalSet(to));
}

fn plan_start(plan: &mut Vec<PlanToken>, local: &LocalLayout, value: ValueId) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, value)?;
    local.refs.plan_base(plan);
    Ok(())
}

fn plan_offset(plan: &mut Vec<PlanToken>, local: &LocalLayout, offset: i32) {
    plan.push(PlanToken::I32Const(offset));
    local.refs.plan_add(plan);
}

fn plan_at(plan: &mut Vec<PlanToken>, local: &LocalLayout, value: ValueId, offset: i32) -> Result<(), CanonicalAdmissionError> {
    if offset == 0 {
        push_get(plan, local, value)?;
        local.refs.plan_object(plan);
    } else {
        plan_start(plan, local, value)?;
        plan_offset(plan, local, offset);
        local.refs.plan_finish(plan);
    }
    Ok(())
}

fn plan_at_const(plan: &mut Vec<PlanToken>, local: &LocalLayout, value: ValueId, offset: i32) -> Result<(), CanonicalAdmissionError> {
    plan_start(plan, local, value)?;
    plan_offset(plan, local, offset);
    local.refs.plan_finish(plan);
    Ok(())
}

fn plan_address_at(plan: &mut Vec<PlanToken>, local: &LocalLayout, address: u32, offset: i32) {
    plan.push(PlanToken::LocalGet(address));
    if offset != 0 { plan_offset(plan, local, offset); }
    local.refs.plan_finish(plan);
}

fn plan_address_at_const(plan: &mut Vec<PlanToken>, local: &LocalLayout, address: u32, offset: i32) {
    plan.push(PlanToken::LocalGet(address));
    plan_offset(plan, local, offset);
    local.refs.plan_finish(plan);
}

fn plan_copy_address(plan: &mut Vec<PlanToken>, local: &LocalLayout, from: u32, offset: i32, to: u32) {
    plan.push(PlanToken::LocalGet(from));
    plan_offset(plan, local, offset);
    plan.push(PlanToken::LocalSet(to));
}

fn load32() -> wasm_encoder::Instruction<'static> { wasm_encoder::Instruction::I32Load(memarg(4)) }
fn pload32() -> PlanToken { PlanToken::I32Load { offset: 0, align: 2, memory: 0 } }
fn pstore32() -> PlanToken { PlanToken::I32Store { offset: 0, align: 2, memory: 0 } }

fn emit_trap_if(body: &mut WasmFunctionEncoder) {
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    body.instruction(&wasm_encoder::Instruction::Unreachable);
    body.instruction(&wasm_encoder::Instruction::End);
}

fn plan_trap_if(plan: &mut Vec<PlanToken>) {
    plan.extend([PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
}

/// Leaves the bucket's row address in address local 4.
fn emit_slot_address(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, bucket: ValueId, field: crate::canonical_safewir::dictionary::DictionarySlotField,
) -> Result<(), EncodeError> {
    // Bounds first. The rows occupy 20+32*capacity bytes inside the larger
    // authenticated allocation, so an in-range bucket cannot escape it.
    let row = emit_address_local(function, local, 4)?;
    emit_get(body, local, function.id, bucket)?;
    emit_at(body, function, local, dict, 4)?;
    body.instruction(&load32());
    body.instruction(&wasm_encoder::Instruction::I32GeU);
    emit_trap_if(body);
    emit_start(body, function, local, dict)?;
    emit_get(body, local, function.id, bucket)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    local.refs.add(body);
    emit_offset(body, local, 20);
    body.instruction(&wasm_encoder::Instruction::LocalSet(row));
    if field != crate::canonical_safewir::dictionary::DictionarySlotField::State {
        emit_address_at(body, local, row, 0);
        body.instruction(&load32());
        body.instruction(&wasm_encoder::Instruction::I32Const(1));
        body.instruction(&wasm_encoder::Instruction::I32Ne);
        emit_trap_if(body);
    }
    Ok(())
}

fn plan_slot_address(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId, bucket: ValueId,
    field: crate::canonical_safewir::dictionary::DictionarySlotField,
) -> Result<(), CanonicalAdmissionError> {
    let row = plan_address_local(local, 4)?;
    push_get(plan, local, bucket)?;
    plan_at(plan, local, dict, 4)?;
    plan.extend([pload32(), PlanToken::I32GeU]);
    plan_trap_if(plan);
    plan_start(plan, local, dict)?;
    push_get(plan, local, bucket)?;
    plan.extend([PlanToken::I32Const(32), PlanToken::I32Mul]);
    local.refs.plan_add(plan);
    plan_offset(plan, local, 20);
    plan.push(PlanToken::LocalSet(row));
    if field != crate::canonical_safewir::dictionary::DictionarySlotField::State {
        plan_address_at(plan, local, row, 0);
        plan.extend([pload32(), PlanToken::I32Const(1), PlanToken::I32Ne]);
        plan_trap_if(plan);
    }
    Ok(())
}

fn emit_store_value(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    value: ValueId, address: u32,
) -> Result<(), EncodeError> {
    emit_address_at(body, local, address, 0);
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
    plan_address_at(plan, local, address, 0);
    push_get(plan, local, value)?;
    plan.push(match function.value_types.get(&value) {
        Some(ValueType::Int) => PlanToken::I64Store { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Float) => PlanToken::F64Store { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => PlanToken::I32Store { offset: 0, align: 2, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    Ok(())
}

fn emit_i32_store(body: &mut WasmFunctionEncoder, local: &LocalLayout, address: u32, offset: i32, value: u32) {
    emit_address_at(body, local, address, offset);
    body.instruction(&wasm_encoder::Instruction::LocalGet(value));
    body.instruction(&wasm_encoder::Instruction::I32Store(memarg(4)));
}

fn plan_i32_store(plan: &mut Vec<PlanToken>, local: &LocalLayout, address: u32, offset: i32, value: u32) {
    plan_address_at(plan, local, address, offset);
    plan.extend([PlanToken::LocalGet(value), pstore32()]);
}

// The row region remains at +20. Controls follow all rows, and the first
// sixteen controls are mirrored after the array for a wrapped SIMD load.
// Leaves the unfinished control base address.
fn emit_control_base(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId,
) -> Result<(), EncodeError> {
    emit_start(body, function, local, dict)?;
    emit_at(body, function, local, dict, 4)?;
    body.instruction(&load32());
    body.instruction(&wasm_encoder::Instruction::I32Const(32));
    body.instruction(&wasm_encoder::Instruction::I32Mul);
    local.refs.add(body);
    emit_offset(body, local, 20);
    Ok(())
}

fn plan_control_base(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId)
    -> Result<(), CanonicalAdmissionError> {
    plan_start(plan, local, dict)?;
    plan_at(plan, local, dict, 4)?;
    plan.extend([pload32(), PlanToken::I32Const(32), PlanToken::I32Mul]);
    local.refs.plan_add(plan);
    plan_offset(plan, local, 20);
    Ok(())
}

fn emit_fill_controls(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, capacity: ValueId,
) -> Result<(), EncodeError> {
    emit_control_base(body, function, local, dict)?;
    local.refs.finish(body);
    body.instruction(&wasm_encoder::Instruction::I32Const(0x80));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(16));
    body.instruction(&wasm_encoder::Instruction::I32Add);
    body.instruction(&wasm_encoder::Instruction::I64ExtendI32U);
    body.instruction(&wasm_encoder::Instruction::MemoryFill(0));
    Ok(())
}

fn plan_fill_controls(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId,
    capacity: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    plan_control_base(plan, local, dict)?;
    local.refs.plan_finish(plan);
    plan.push(PlanToken::I32Const(0x80));
    push_get(plan, local, capacity)?;
    plan.extend([PlanToken::I32Const(16), PlanToken::I32Add, PlanToken::I64ExtendI32U, PlanToken::MemoryFill(0)]);
    Ok(())
}

// `bucket` and `control` are scalar scratch locals. Mutations mirror the
// first group so ProbeGroup can load sixteen bytes across capacity wrap.
fn emit_write_control(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    dict: ValueId, bucket: u32, control: u32,
) -> Result<(), EncodeError> {
    emit_control_base(body, function, local, dict)?;
    body.instruction(&wasm_encoder::Instruction::LocalGet(bucket));
    local.refs.add(body);
    local.refs.finish(body);
    body.instruction(&wasm_encoder::Instruction::LocalGet(control));
    body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
    body.instruction(&wasm_encoder::Instruction::LocalGet(bucket));
    body.instruction(&wasm_encoder::Instruction::I32Const(16));
    body.instruction(&wasm_encoder::Instruction::I32LtU);
    body.instruction(&wasm_encoder::Instruction::If(BlockType::Empty));
    emit_control_base(body, function, local, dict)?;
    emit_at(body, function, local, dict, 4)?;
    body.instruction(&load32());
    local.refs.add(body);
    body.instruction(&wasm_encoder::Instruction::LocalGet(bucket));
    local.refs.add(body);
    local.refs.finish(body);
    body.instruction(&wasm_encoder::Instruction::LocalGet(control));
    body.instruction(&wasm_encoder::Instruction::I32Store8(memarg(1)));
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

fn plan_write_control(plan: &mut Vec<PlanToken>, local: &LocalLayout, dict: ValueId,
    bucket: u32, control: u32,
) -> Result<(), CanonicalAdmissionError> {
    plan_control_base(plan, local, dict)?;
    plan.push(PlanToken::LocalGet(bucket));
    local.refs.plan_add(plan);
    local.refs.plan_finish(plan);
    plan.extend([PlanToken::LocalGet(control), PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 },
        PlanToken::LocalGet(bucket), PlanToken::I32Const(16), PlanToken::I32LtU, PlanToken::If("Empty".into())]);
    plan_control_base(plan, local, dict)?;
    plan_at(plan, local, dict, 4)?;
    plan.push(pload32());
    local.refs.plan_add(plan);
    plan.push(PlanToken::LocalGet(bucket));
    local.refs.plan_add(plan);
    local.refs.plan_finish(plan);
    plan.extend([PlanToken::LocalGet(control), PlanToken::I32Store8 { offset: 0, align: 0, memory: 0 }, PlanToken::End]);
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
    use wasm_encoder::Instruction as I;
    let offsets = pointer_offsets(function, layouts, source)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if !offsets.is_empty() && retain.is_none() {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary retain helper" });
    }
    emit_capacity_guard(body, function, local, capacity)?;
    emit_at(body, function, local, source, 4)?;
    body.instruction(&load32());
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&I::I32Ne);
    emit_trap_if(body);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary clone destination" });
    };
    emit_alloc(body, function, local, layouts, destination, layout_id, &counted_extent(capacity))?;
    emit_at(body, function, local, destination, 0)?;
    emit_at(body, function, local, source, 0)?;
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&I::I32Const(33));
    body.instruction(&I::I32Mul);
    body.instruction(&I::I32Const(36));
    body.instruction(&I::I32Add);
    body.instruction(&I::I64ExtendI32U);
    body.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
    if !offsets.is_empty() {
        let retain = retain.expect("owned dictionary slots require a retain helper");
        let row = emit_address_local(function, local, 5)?;
        body.instruction(&I::I32Const(0));
        emit_set_index(body, local.scratch[4]);
        body.instruction(&I::Block(BlockType::Empty));
        body.instruction(&I::Loop(BlockType::Empty));
        emit_get_index(body, local.scratch[4]);
        emit_get(body, local, function.id, capacity)?;
        body.instruction(&I::I32GeU);
        body.instruction(&I::BrIf(1));
        emit_start(body, function, local, destination)?;
        emit_offset(body, local, 20);
        emit_get_index(body, local.scratch[4]);
        body.instruction(&I::I32Const(32));
        body.instruction(&I::I32Mul);
        local.refs.add(body);
        body.instruction(&I::LocalSet(row));
        emit_address_at(body, local, row, 0);
        body.instruction(&load32());
        body.instruction(&I::I32Const(1));
        body.instruction(&I::I32Eq);
        body.instruction(&I::If(BlockType::Empty));
        for offset in offsets {
            emit_address_at(body, local, row, offset);
            body.instruction(&load32());
            body.instruction(&I::Call(retain));
            body.instruction(&I::Drop);
        }
        body.instruction(&I::End);
        emit_get_index(body, local.scratch[4]);
        body.instruction(&I::I32Const(1));
        body.instruction(&I::I32Add);
        emit_set_index(body, local.scratch[4]);
        body.instruction(&I::Br(0));
        body.instruction(&I::End);
        body.instruction(&I::End);
    }
    Ok(())
}

fn plan_clone_exact(plan: &mut Vec<PlanToken>, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    use PlanToken as P;
    let offsets = pointer_offsets(function, layouts, source).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if !offsets.is_empty() && retain.is_none() { return Err(CanonicalAdmissionError::UnsupportedPlan); }
    plan_capacity_guard(plan, local, capacity)?;
    plan_at(plan, local, source, 4)?;
    plan.push(pload32());
    push_get(plan, local, capacity)?;
    plan.push(P::I32Ne);
    plan_trap_if(plan);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    };
    let collection = collection_layout(function.id, layouts, layout_id).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    expected_alloc_plan(plan, function, local, Some(collection), destination, &counted_extent(capacity))?;
    plan_at(plan, local, destination, 0)?;
    plan_at(plan, local, source, 0)?;
    push_get(plan, local, capacity)?;
    plan.extend([P::I32Const(33), P::I32Mul, P::I32Const(36), P::I32Add,
        P::I64ExtendI32U, P::MemoryCopy { source: 0, destination: 0 }]);
    if !offsets.is_empty() {
        let retain = retain.expect("owned dictionary slots require a retain helper");
        let row = plan_address_local(local, 5)?;
        plan.extend([P::I32Const(0), P::LocalSet(local.scratch[4]),
            P::Block("Empty".into()), P::Loop("Empty".into()), P::LocalGet(local.scratch[4])]);
        push_get(plan, local, capacity)?;
        plan.extend([P::I32GeU, P::BrIf(1)]);
        plan_start(plan, local, destination)?;
        plan_offset(plan, local, 20);
        plan.extend([P::LocalGet(local.scratch[4]), P::I32Const(32), P::I32Mul]);
        local.refs.plan_add(plan);
        plan.push(P::LocalSet(row));
        plan_address_at(plan, local, row, 0);
        plan.extend([pload32(), P::I32Const(1), P::I32Eq, P::If("Empty".into())]);
        for offset in offsets {
            plan_address_at(plan, local, row, offset);
            plan.extend([pload32(), P::Call(retain), P::Drop]);
        }
        plan.extend([P::End, P::LocalGet(local.scratch[4]), P::I32Const(1),
            P::I32Add, P::LocalSet(local.scratch[4]), P::Br(0), P::End, P::End]);
    }
    Ok(())
}

fn emit_rehash(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout,
    layouts: &LayoutInterner, destination: ValueId, source: ValueId, capacity: ValueId,
    transfer: crate::canonical_safewir::dictionary::DictionaryRehashTransfer,
) -> Result<(), EncodeError> {
    use crate::canonical_safewir::dictionary::DictionaryRehashTransfer as T;
    use wasm_encoder::Instruction as I;
    let offsets = pointer_offsets(function, layouts, source)?;
    let retain = local.memory_helpers.as_ref().map(|helpers| helpers.retain);
    if transfer == T::CloneShared && !offsets.is_empty() && retain.is_none() {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary rehash retain" });
    }
    let (from_row, to_row, link) = (emit_address_local(function, local, 5)?, emit_address_local(function, local, 7)?,
        emit_address_local(function, local, 11)?);
    emit_capacity_guard(body, function, local, capacity)?;
    // A spare empty bucket guarantees termination of linear probing.
    emit_at(body, function, local, source, 0)?;
    body.instruction(&load32());
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&I::I32GeU);
    emit_trap_if(body);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else {
        return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary rehash destination" });
    };
    emit_alloc(body, function, local, layouts, destination, layout_id, &counted_extent(capacity))?;
    emit_at(body, function, local, destination, 20)?;
    body.instruction(&I::I32Const(0));
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&I::I32Const(32));
    body.instruction(&I::I32Mul);
    body.instruction(&I::I64ExtendI32U);
    body.instruction(&I::MemoryFill(0));
    emit_fill_controls(body, function, local, destination, capacity)?;
    body.instruction(&I::I32Const(0));
    emit_set_index(body, local.scratch[11]);
    emit_start(body, function, local, destination)?;
    body.instruction(&I::LocalSet(to_row));
    for offset in [0, 8, 12, 16] { emit_i32_store(body, local, to_row, offset, local.scratch[11]); }
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Sub);
    emit_set_index(body, local.scratch[10]);
    emit_at(body, function, local, source, 8)?;
    body.instruction(&load32());
    emit_set_index(body, local.scratch[4]);
    body.instruction(&I::Block(BlockType::Empty));
    body.instruction(&I::Loop(BlockType::Empty));
    emit_get_index(body, local.scratch[4]);
    body.instruction(&I::I32Eqz);
    body.instruction(&I::BrIf(1));
    emit_start(body, function, local, source)?;
    emit_get_index(body, local.scratch[4]);
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Sub);
    body.instruction(&I::I32Const(32));
    body.instruction(&I::I32Mul);
    local.refs.add(body);
    emit_offset(body, local, 20);
    body.instruction(&I::LocalSet(from_row));
    emit_address_at(body, local, from_row, 4);
    body.instruction(&load32());
    emit_get_index(body, local.scratch[10]);
    body.instruction(&I::I32And);
    emit_set_index(body, local.scratch[6]);
    body.instruction(&I::Block(BlockType::Empty));
    body.instruction(&I::Loop(BlockType::Empty));
    emit_start(body, function, local, destination)?;
    emit_get_index(body, local.scratch[6]);
    body.instruction(&I::I32Const(32));
    body.instruction(&I::I32Mul);
    local.refs.add(body);
    emit_offset(body, local, 20);
    body.instruction(&I::LocalSet(to_row));
    emit_address_at(body, local, to_row, 0);
    body.instruction(&load32());
    body.instruction(&I::I32Eqz);
    body.instruction(&I::BrIf(1));
    emit_get_index(body, local.scratch[6]);
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Add);
    emit_get_index(body, local.scratch[10]);
    body.instruction(&I::I32And);
    emit_set_index(body, local.scratch[6]);
    body.instruction(&I::Br(0));
    body.instruction(&I::End);
    body.instruction(&I::End);
    emit_address_at(body, local, to_row, 0);
    emit_address_at(body, local, from_row, 0);
    body.instruction(&I::I64Const(32));
    body.instruction(&I::MemoryCopy { src_mem: 0, dst_mem: 0 });
    emit_address_at(body, local, from_row, 0);
    body.instruction(&I::I32Load(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
    body.instruction(&I::I32Const(25));
    body.instruction(&I::I32ShrU);
    emit_set_index(body, local.scratch[11]);
    emit_write_control(body, function, local, destination, local.scratch[6], local.scratch[11])?;
    if transfer == T::CloneShared {
        if let Some(retain) = retain {
            for offset in offsets {
                emit_address_at(body, local, to_row, offset);
                body.instruction(&load32());
                body.instruction(&I::Call(retain));
                body.instruction(&I::Drop);
            }
        }
    }
    emit_at(body, function, local, destination, 12)?;
    body.instruction(&load32());
    emit_set_index(body, local.scratch[8]);
    emit_get_index(body, local.scratch[6]);
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Add);
    emit_set_index(body, local.scratch[9]);
    emit_i32_store(body, local, to_row, 12, local.scratch[8]);
    body.instruction(&I::I32Const(0));
    emit_set_index(body, local.scratch[11]);
    emit_i32_store(body, local, to_row, 8, local.scratch[11]);
    emit_get_index(body, local.scratch[8]);
    body.instruction(&I::I32Eqz);
    body.instruction(&I::If(BlockType::Empty));
    emit_start(body, function, local, destination)?;
    body.instruction(&I::LocalSet(link));
    emit_i32_store(body, local, link, 8, local.scratch[9]);
    body.instruction(&I::Else);
    emit_start(body, function, local, destination)?;
    emit_get_index(body, local.scratch[8]);
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Sub);
    body.instruction(&I::I32Const(32));
    body.instruction(&I::I32Mul);
    local.refs.add(body);
    emit_offset(body, local, 28);
    body.instruction(&I::LocalSet(link));
    emit_i32_store(body, local, link, 0, local.scratch[9]);
    body.instruction(&I::End);
    emit_start(body, function, local, destination)?;
    body.instruction(&I::LocalSet(link));
    emit_i32_store(body, local, link, 12, local.scratch[9]);
    emit_address_at(body, local, from_row, 8);
    body.instruction(&load32());
    emit_set_index(body, local.scratch[4]);
    body.instruction(&I::Br(0));
    body.instruction(&I::End);
    body.instruction(&I::End);
    emit_at(body, function, local, destination, 0)?;
    emit_at(body, function, local, source, 0)?;
    body.instruction(&load32());
    body.instruction(&I::I32Store(memarg(4)));
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
    let (from_row, to_row, link) = (plan_address_local(local, 5)?, plan_address_local(local, 7)?, plan_address_local(local, 11)?);
    plan_capacity_guard(plan, local, capacity)?;
    plan_at(plan, local, source, 0)?;
    plan.push(pload32());
    push_get(plan, local, capacity)?;
    plan.push(P::I32GeU);
    plan_trap_if(plan);
    let Some(ValueType::Ref(layout_id)) = function.value_types.get(&destination).copied() else { return Err(CanonicalAdmissionError::UnsupportedPlan); };
    let collection = collection_layout(function.id, layouts, layout_id).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    expected_alloc_plan(plan, function, local, Some(collection), destination, &counted_extent(capacity))?;
    plan_at(plan, local, destination, 20)?;
    plan.push(P::I32Const(0));
    push_get(plan, local, capacity)?;
    plan.extend([P::I32Const(32), P::I32Mul, P::I64ExtendI32U, P::MemoryFill(0)]);
    plan_fill_controls(plan, local, destination, capacity)?;
    plan.extend([P::I32Const(0), P::LocalSet(local.scratch[11])]);
    plan_start(plan, local, destination)?;
    plan.push(P::LocalSet(to_row));
    for offset in [0, 8, 12, 16] { plan_i32_store(plan, local, to_row, offset, local.scratch[11]); }
    push_get(plan, local, capacity)?;
    plan.extend([P::I32Const(1), P::I32Sub, P::LocalSet(local.scratch[10])]);
    plan_at(plan, local, source, 8)?;
    plan.extend([pload32(), P::LocalSet(local.scratch[4]), P::Block("Empty".into()), P::Loop("Empty".into()),
        P::LocalGet(local.scratch[4]), P::I32Eqz, P::BrIf(1)]);
    plan_start(plan, local, source)?;
    plan.extend([P::LocalGet(local.scratch[4]), P::I32Const(1), P::I32Sub, P::I32Const(32), P::I32Mul]);
    local.refs.plan_add(plan);
    plan_offset(plan, local, 20);
    plan.push(P::LocalSet(from_row));
    plan_address_at(plan, local, from_row, 4);
    plan.extend([pload32(), P::LocalGet(local.scratch[10]),
        P::I32And, P::LocalSet(local.scratch[6]), P::Block("Empty".into()), P::Loop("Empty".into())]);
    plan_start(plan, local, destination)?;
    plan.extend([P::LocalGet(local.scratch[6]), P::I32Const(32), P::I32Mul]);
    local.refs.plan_add(plan);
    plan_offset(plan, local, 20);
    plan.push(P::LocalSet(to_row));
    plan_address_at(plan, local, to_row, 0);
    plan.extend([pload32(), P::I32Eqz, P::BrIf(1), P::LocalGet(local.scratch[6]), P::I32Const(1), P::I32Add,
        P::LocalGet(local.scratch[10]), P::I32And, P::LocalSet(local.scratch[6]),
        P::Br(0), P::End, P::End]);
    plan_address_at(plan, local, to_row, 0);
    plan_address_at(plan, local, from_row, 0);
    plan.extend([P::I64Const(32), P::MemoryCopy { source: 0, destination: 0 }]);
    plan_address_at(plan, local, from_row, 0);
    plan.extend([P::I32Load { offset: 4, align: 2, memory: 0 },
        P::I32Const(25), P::I32ShrU, P::LocalSet(local.scratch[11])]);
    plan_write_control(plan, local, destination, local.scratch[6], local.scratch[11])?;
    if transfer == T::CloneShared {
        if let Some(retain) = retain {
            for offset in offsets {
                plan_address_at(plan, local, to_row, offset);
                plan.extend([pload32(), P::Call(retain), P::Drop]);
            }
        }
    }
    plan_at(plan, local, destination, 12)?;
    plan.extend([pload32(), P::LocalSet(local.scratch[8]), P::LocalGet(local.scratch[6]), P::I32Const(1),
        P::I32Add, P::LocalSet(local.scratch[9])]);
    plan_i32_store(plan, local, to_row, 12, local.scratch[8]);
    plan.extend([P::I32Const(0), P::LocalSet(local.scratch[11])]);
    plan_i32_store(plan, local, to_row, 8, local.scratch[11]);
    plan.extend([P::LocalGet(local.scratch[8]), P::I32Eqz, P::If("Empty".into())]);
    plan_start(plan, local, destination)?;
    plan.push(P::LocalSet(link));
    plan_i32_store(plan, local, link, 8, local.scratch[9]);
    plan.push(P::Else);
    plan_start(plan, local, destination)?;
    plan.extend([P::LocalGet(local.scratch[8]), P::I32Const(1), P::I32Sub, P::I32Const(32), P::I32Mul]);
    local.refs.plan_add(plan);
    plan_offset(plan, local, 28);
    plan.push(P::LocalSet(link));
    plan_i32_store(plan, local, link, 0, local.scratch[9]);
    plan.push(P::End);
    plan_start(plan, local, destination)?;
    plan.push(P::LocalSet(link));
    plan_i32_store(plan, local, link, 12, local.scratch[9]);
    plan_address_at(plan, local, from_row, 8);
    plan.extend([pload32(), P::LocalSet(local.scratch[4]), P::Br(0), P::End, P::End]);
    plan_at(plan, local, destination, 0)?;
    plan_at(plan, local, source, 0)?;
    plan.extend([pload32(), pstore32()]);
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

fn load_for(function: &Function, value: ValueId, operation: &'static str) -> Result<wasm_encoder::Instruction<'static>, EncodeError> {
    Ok(match function.value_types.get(&value) {
        Some(ValueType::Int) => wasm_encoder::Instruction::I64Load(memarg(8)),
        Some(ValueType::Float) => wasm_encoder::Instruction::F64Load(memarg(8)),
        Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => wasm_encoder::Instruction::I32Load(memarg(4)),
        _ => return Err(EncodeError::UnsupportedLayout { function: function.id, operation }),
    })
}

fn plan_load_for(function: &Function, value: ValueId) -> Result<PlanToken, CanonicalAdmissionError> {
    Ok(match function.value_types.get(&value) {
        Some(ValueType::Int) => PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Float) => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
        Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => pload32(),
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    })
}

pub(super) fn emit(
    body: &mut WasmFunctionEncoder,
    function: &Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    operation: &DictionaryOperation,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    match operation {
        DictionaryOperation::Rehash { destination, source, capacity, transfer } =>
            emit_rehash(body, function, local, layouts, *destination, *source, *capacity, *transfer),
        DictionaryOperation::CloneExact { destination, source, capacity } =>
            emit_clone_exact(body, function, local, layouts, *destination, *source, *capacity),
        DictionaryOperation::Initialize { destination, layout, capacity } => {
            emit_capacity_guard(body, function, local, *capacity)?;
            emit_alloc(body, function, local, layouts, *destination, *layout, &counted_extent(*capacity))?;
            for offset in [0, 8, 12, 16] {
                emit_at(body, function, local, *destination, offset)?;
                body.instruction(&I::I32Const(0));
                body.instruction(&I::I32Store(memarg(4)));
            }
            // Reused free-list blocks may carry live-looking row states.
            // Clearing the full row extent also gives scalar slots a defined
            // zero representation before any insertion.
            emit_at(body, function, local, *destination, 20)?;
            body.instruction(&I::I32Const(0));
            emit_get(body, local, function.id, *capacity)?;
            body.instruction(&I::I32Const(32));
            body.instruction(&I::I32Mul);
            body.instruction(&I::I64ExtendI32U);
            body.instruction(&I::MemoryFill(0));
            emit_fill_controls(body, function, local, *destination, *capacity)?;
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            emit_at_const(body, function, local, *dict, header_offset(*field))?;
            body.instruction(&load32());
            emit_set(body, local, function.id, *destination)
        }
        DictionaryOperation::ProbeGroup { dict, bucket, fingerprint, matching, special, first_event } => {
            let group = emit_address_local(function, local, 4)?;
            emit_at(body, function, local, *dict, 0)?;
            body.instruction(&I::I32Load(wasm_encoder::MemArg { offset: 4, align: 2, memory_index: 0 }));
            emit_set_index(body, local.scratch[5]);
            emit_get(body, local, function.id, *bucket)?;
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32GeU);
            emit_trap_if(body);
            emit_start(body, function, local, *dict)?;
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32Const(32));
            body.instruction(&I::I32Mul);
            local.refs.add(body);
            emit_offset(body, local, 20);
            emit_get(body, local, function.id, *bucket)?;
            local.refs.add(body);
            body.instruction(&I::LocalSet(group));
            let control_vector = *local.scratch.get(12).ok_or(EncodeError::UnsupportedLayout {
                function: function.id, operation: "Dictionary SIMD scratch",
            })?;
            emit_address_at(body, local, group, 0);
            body.instruction(&I::V128Load(memarg(1)));
            emit_set_index(body, control_vector);
            emit_get_index(body, control_vector);
            emit_get(body, local, function.id, *fingerprint)?;
            body.instruction(&I::I8x16Splat);
            body.instruction(&I::I8x16Eq);
            body.instruction(&I::I8x16Bitmask);
            emit_set(body, local, function.id, *matching)?;
            emit_get_index(body, control_vector);
            body.instruction(&I::I8x16Bitmask);
            emit_set(body, local, function.id, *special)?;
            emit_get(body, local, function.id, *matching)?;
            emit_get(body, local, function.id, *special)?;
            body.instruction(&I::I32Or);
            body.instruction(&I::I32Const(0x10000));
            body.instruction(&I::I32Or);
            body.instruction(&I::I32Ctz);
            emit_set(body, local, function.id, *first_event)?;
            Ok(())
        }
        DictionaryOperation::ReadSlot { destination, dict, bucket, field, transfer } => {
            emit_slot_address(body, function, local, *dict, *bucket, *field)?;
            let row = emit_address_local(function, local, 4)?;
            emit_address_at_const(body, local, row, slot_offset(*field));
            match function.value_types.get(destination) {
                Some(ValueType::Int) => {
                    if *field == crate::canonical_safewir::dictionary::DictionarySlotField::Hash {
                        body.instruction(&load32());
                        body.instruction(&I::I64ExtendI32U);
                    } else {
                        body.instruction(&I::I64Load(memarg(8)));
                    }
                }
                Some(ValueType::Float) => { body.instruction(&I::F64Load(memarg(8))); }
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => {
                    body.instruction(&load32());
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
            let (row, field) = (emit_address_local(function, local, 4)?, emit_address_local(function, local, 5)?);
            if *transfer == StoreTransfer::CloneShared {
                emit_clone_ref(body, local, *value, *value, function)?;
            }
            emit_address_at_const(body, local, row, 24);
            body.instruction(&load_for(function, *displaced, "Dictionary displaced type")?);
            emit_set(body, local, function.id, *displaced)?;
            emit_copy_address(body, local, row, 24, field);
            emit_store_value(body, function, local, *value, field)
        }
        DictionaryOperation::InsertSlot { dict, bucket, key, value, hash, key_transfer, value_transfer } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            emit_slot_address(body, function, local, *dict, *bucket, DictionarySlotField::State)?;
            let (row, field) = (emit_address_local(function, local, 4)?, emit_address_local(function, local, 8)?);
            emit_address_at(body, local, row, 0);
            body.instruction(&load32());
            emit_set_index(body, local.scratch[5]);
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Eq);
            emit_trap_if(body);
            emit_at(body, function, local, *dict, 0)?;
            body.instruction(&load32());
            emit_set_index(body, local.scratch[10]);
            emit_get_index(body, local.scratch[10]);
            emit_at(body, function, local, *dict, 4)?;
            body.instruction(&load32());
            body.instruction(&I::I32GeU);
            emit_trap_if(body);
            if *key_transfer == StoreTransfer::CloneShared { emit_clone_ref(body, local, *key, *key, function)?; }
            if *value_transfer == StoreTransfer::CloneShared { emit_clone_ref(body, local, *value, *value, function)?; }
            emit_at(body, function, local, *dict, 12)?;
            body.instruction(&load32());
            emit_set_index(body, local.scratch[6]);
            emit_get(body, local, function.id, *bucket)?;
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Add);
            emit_set_index(body, local.scratch[7]);
            body.instruction(&I::I32Const(0));
            emit_set_index(body, local.scratch[9]);
            emit_copy_address(body, local, row, 16, field);
            emit_store_value(body, function, local, *key, field)?;
            emit_copy_address(body, local, row, 24, field);
            emit_store_value(body, function, local, *value, field)?;
            emit_address_at(body, local, row, 4);
            emit_get(body, local, function.id, *hash)?;
            body.instruction(&I::I32WrapI64);
            body.instruction(&I::I32Store(memarg(4)));
            emit_i32_store(body, local, row, 8, local.scratch[9]);
            emit_i32_store(body, local, row, 12, local.scratch[6]);
            emit_get_index(body, local.scratch[6]);
            body.instruction(&I::I32Eqz);
            body.instruction(&I::If(BlockType::Empty));
            emit_start(body, function, local, *dict)?;
            body.instruction(&I::LocalSet(field));
            emit_i32_store(body, local, field, 8, local.scratch[7]);
            body.instruction(&I::Else);
            emit_start(body, function, local, *dict)?;
            emit_get_index(body, local.scratch[6]);
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Sub);
            body.instruction(&I::I32Const(32));
            body.instruction(&I::I32Mul);
            local.refs.add(body);
            emit_offset(body, local, 28);
            body.instruction(&I::LocalSet(field));
            emit_i32_store(body, local, field, 0, local.scratch[7]);
            body.instruction(&I::End);
            emit_start(body, function, local, *dict)?;
            body.instruction(&I::LocalSet(field));
            emit_i32_store(body, local, field, 12, local.scratch[7]);
            emit_get_index(body, local.scratch[10]);
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Add);
            emit_set_index(body, local.scratch[10]);
            emit_i32_store(body, local, field, 0, local.scratch[10]);
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32Const(2));
            body.instruction(&I::I32Eq);
            body.instruction(&I::If(BlockType::Empty));
            emit_address_at(body, local, field, 16);
            body.instruction(&load32());
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Sub);
            emit_set_index(body, local.scratch[11]);
            emit_i32_store(body, local, field, 16, local.scratch[11]);
            body.instruction(&I::End);
            body.instruction(&I::I32Const(1));
            emit_set_index(body, local.scratch[9]);
            emit_i32_store(body, local, row, 0, local.scratch[9]);
            emit_get(body, local, function.id, *hash)?;
            body.instruction(&I::I32WrapI64);
            body.instruction(&I::I32Const(25));
            body.instruction(&I::I32ShrU);
            emit_set_index(body, local.scratch[9]);
            emit_get(body, local, function.id, *bucket)?;
            emit_set_index(body, local.scratch[11]);
            emit_write_control(body, function, local, *dict, local.scratch[11], local.scratch[9])?;
            Ok(())
        }
        DictionaryOperation::RemoveSlot { removed_key, removed_value, dict, bucket } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            let key_drop = key_pointee(function, layouts, *dict)?.filter(|_| removed_key.is_none())
                .map(|pointee| local.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&pointee).copied())
                    .ok_or(EncodeError::UnsupportedLayout { function: function.id, operation: "Dictionary key drop" }))
                .transpose()?;
            emit_slot_address(body, function, local, *dict, *bucket, DictionarySlotField::Value)?;
            let (row, neighbour, header) = (emit_address_local(function, local, 4)?,
                emit_address_local(function, local, 7)?, emit_address_local(function, local, 8)?);
            emit_address_at_const(body, local, row, 24);
            body.instruction(&load_for(function, *removed_value, "Dictionary remove value")?);
            emit_set(body, local, function.id, *removed_value)?;
            if let Some(removed_key) = removed_key {
                emit_address_at_const(body, local, row, 16);
                body.instruction(&load_for(function, *removed_key, "Dictionary remove key")?);
                emit_set(body, local, function.id, *removed_key)?;
            }
            if key_drop.is_some() {
                emit_address_at_const(body, local, row, 16);
                body.instruction(&load32());
                emit_set_index(body, local.scratch[11]);
            }
            for (offset, scratch) in [(8, 5), (12, 6)] {
                emit_address_at_const(body, local, row, offset);
                body.instruction(&load32());
                emit_set_index(body, local.scratch[scratch]);
            }
            emit_start(body, function, local, *dict)?;
            body.instruction(&I::LocalSet(header));
            body.instruction(&I::I32Const(0));
            emit_set_index(body, local.scratch[9]);
            // Previous row's next, or header head.
            emit_get_index(body, local.scratch[6]);
            body.instruction(&I::I32Eqz);
            body.instruction(&I::If(BlockType::Empty));
            emit_i32_store(body, local, header, 8, local.scratch[5]);
            body.instruction(&I::Else);
            body.instruction(&I::LocalGet(header));
            emit_get_index(body, local.scratch[6]);
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Sub);
            body.instruction(&I::I32Const(32));
            body.instruction(&I::I32Mul);
            local.refs.add(body);
            emit_offset(body, local, 28);
            body.instruction(&I::LocalSet(neighbour));
            emit_i32_store(body, local, neighbour, 0, local.scratch[5]);
            body.instruction(&I::End);
            // Next row's prev, or header tail.
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32Eqz);
            body.instruction(&I::If(BlockType::Empty));
            emit_i32_store(body, local, header, 12, local.scratch[6]);
            body.instruction(&I::Else);
            body.instruction(&I::LocalGet(header));
            emit_get_index(body, local.scratch[5]);
            body.instruction(&I::I32Const(1));
            body.instruction(&I::I32Sub);
            body.instruction(&I::I32Const(32));
            body.instruction(&I::I32Mul);
            local.refs.add(body);
            emit_offset(body, local, 32);
            body.instruction(&I::LocalSet(neighbour));
            emit_i32_store(body, local, neighbour, 0, local.scratch[6]);
            body.instruction(&I::End);
            for (offset, delta) in [(0, -1), (16, 1)] {
                emit_address_at_const(body, local, header, offset);
                body.instruction(&load32());
                body.instruction(&I::I32Const(delta));
                body.instruction(&I::I32Add);
                emit_set_index(body, local.scratch[10]);
                emit_i32_store(body, local, header, offset, local.scratch[10]);
            }
            body.instruction(&I::I32Const(2));
            emit_set_index(body, local.scratch[10]);
            emit_i32_store(body, local, row, 0, local.scratch[10]);
            emit_get(body, local, function.id, *bucket)?;
            emit_set_index(body, local.scratch[7]);
            body.instruction(&I::I32Const(0xFE));
            emit_set_index(body, local.scratch[10]);
            emit_write_control(body, function, local, *dict, local.scratch[7], local.scratch[10])?;
            for offset in [16, 24] {
                emit_address_at_const(body, local, row, offset);
                body.instruction(&I::I64Const(0));
                body.instruction(&I::I64Store(memarg(8)));
            }
            if let Some(drop) = key_drop {
                emit_get_index(body, local.scratch[11]);
                body.instruction(&I::Call(drop));
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
    use PlanToken as P;
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
                plan_at(plan, local, *destination, offset)?;
                plan.extend([P::I32Const(0), pstore32()]);
            }
            plan_at(plan, local, *destination, 20)?;
            plan.push(P::I32Const(0));
            push_get(plan, local, *capacity)?;
            plan.extend([P::I32Const(32), P::I32Mul, P::I64ExtendI32U, P::MemoryFill(0)]);
            plan_fill_controls(plan, local, *destination, *capacity)?;
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            plan_at_const(plan, local, *dict, header_offset(*field))?;
            plan.push(pload32());
            push_set(plan, local, *destination)
        }
        DictionaryOperation::ProbeGroup { dict, bucket, fingerprint, matching, special, first_event } => {
            let group = plan_address_local(local, 4)?;
            plan_at(plan, local, *dict, 0)?;
            plan.extend([P::I32Load { offset: 4, align: 2, memory: 0 }, P::LocalSet(local.scratch[5])]);
            push_get(plan, local, *bucket)?;
            plan.extend([P::LocalGet(local.scratch[5]), P::I32GeU]);
            plan_trap_if(plan);
            plan_start(plan, local, *dict)?;
            plan.extend([P::LocalGet(local.scratch[5]), P::I32Const(32), P::I32Mul]);
            local.refs.plan_add(plan);
            plan_offset(plan, local, 20);
            push_get(plan, local, *bucket)?;
            local.refs.plan_add(plan);
            plan.push(P::LocalSet(group));
            let control_vector = *local.scratch.get(12).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            plan_address_at(plan, local, group, 0);
            plan.extend([P::V128Load { offset: 0, align: 0, memory: 0 }, P::LocalSet(control_vector),
                P::LocalGet(control_vector)]);
            push_get(plan, local, *fingerprint)?;
            plan.extend([P::I8x16Splat, P::I8x16Eq, P::I8x16Bitmask]);
            push_set(plan, local, *matching)?;
            plan.extend([P::LocalGet(control_vector), P::I8x16Bitmask]);
            push_set(plan, local, *special)?;
            push_get(plan, local, *matching)?;
            push_get(plan, local, *special)?;
            plan.extend([P::I32Or, P::I32Const(0x10000), P::I32Or, P::I32Ctz]);
            push_set(plan, local, *first_event)?;
            Ok(())
        }
        DictionaryOperation::ReadSlot { destination, dict, bucket, field, transfer } => {
            plan_slot_address(plan, local, *dict, *bucket, *field)?;
            let row = plan_address_local(local, 4)?;
            plan_address_at_const(plan, local, row, slot_offset(*field));
            let hash = *field == crate::canonical_safewir::dictionary::DictionarySlotField::Hash;
            plan.push(match function.value_types.get(destination) {
                Some(ValueType::Int) if hash => pload32(),
                Some(ValueType::Int) => P::I64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Float) => P::F64Load { offset: 0, align: 3, memory: 0 },
                Some(ValueType::Size | ValueType::Bool | ValueType::Unit | ValueType::Ref(_)) => pload32(),
                _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
            });
            if hash && function.value_types.get(destination) == Some(&ValueType::Int) {
                plan.push(P::I64ExtendI32U);
            }
            push_set(plan, local, *destination)?;
            if *transfer == LoadTransfer::CloneShared { push_share_plan(plan, local, *destination, *destination)?; }
            Ok(())
        }
        DictionaryOperation::ReplaceValue { displaced, dict, bucket, value, transfer } => {
            plan_slot_address(plan, local, *dict, *bucket,
                crate::canonical_safewir::dictionary::DictionarySlotField::Value)?;
            let (row, field) = (plan_address_local(local, 4)?, plan_address_local(local, 5)?);
            if *transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *value, *value)?; }
            plan_address_at_const(plan, local, row, 24);
            plan.push(plan_load_for(function, *displaced)?);
            push_set(plan, local, *displaced)?;
            plan_copy_address(plan, local, row, 24, field);
            plan_store_value(plan, function, local, *value, field)
        }
        DictionaryOperation::InsertSlot { dict, bucket, key, value, hash, key_transfer, value_transfer } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            plan_slot_address(plan, local, *dict, *bucket, DictionarySlotField::State)?;
            let (row, field) = (plan_address_local(local, 4)?, plan_address_local(local, 8)?);
            plan_address_at(plan, local, row, 0);
            plan.extend([pload32(), P::LocalSet(local.scratch[5]), P::LocalGet(local.scratch[5]),
                P::I32Const(1), P::I32Eq]);
            plan_trap_if(plan);
            plan_at(plan, local, *dict, 0)?;
            plan.extend([pload32(), P::LocalSet(local.scratch[10]), P::LocalGet(local.scratch[10])]);
            plan_at(plan, local, *dict, 4)?;
            plan.extend([pload32(), P::I32GeU]);
            plan_trap_if(plan);
            if *key_transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *key, *key)?; }
            if *value_transfer == StoreTransfer::CloneShared { push_share_plan(plan, local, *value, *value)?; }
            plan_at(plan, local, *dict, 12)?;
            plan.extend([pload32(), P::LocalSet(local.scratch[6])]);
            push_get(plan, local, *bucket)?;
            plan.extend([P::I32Const(1), P::I32Add, P::LocalSet(local.scratch[7]),
                P::I32Const(0), P::LocalSet(local.scratch[9])]);
            plan_copy_address(plan, local, row, 16, field);
            plan_store_value(plan, function, local, *key, field)?;
            plan_copy_address(plan, local, row, 24, field);
            plan_store_value(plan, function, local, *value, field)?;
            plan_address_at(plan, local, row, 4);
            push_get(plan, local, *hash)?;
            plan.extend([P::I32WrapI64, pstore32()]);
            plan_i32_store(plan, local, row, 8, local.scratch[9]);
            plan_i32_store(plan, local, row, 12, local.scratch[6]);
            plan.extend([P::LocalGet(local.scratch[6]), P::I32Eqz, P::If("Empty".into())]);
            plan_start(plan, local, *dict)?;
            plan.push(P::LocalSet(field));
            plan_i32_store(plan, local, field, 8, local.scratch[7]);
            plan.push(P::Else);
            plan_start(plan, local, *dict)?;
            plan.extend([P::LocalGet(local.scratch[6]), P::I32Const(1), P::I32Sub, P::I32Const(32), P::I32Mul]);
            local.refs.plan_add(plan);
            plan_offset(plan, local, 28);
            plan.push(P::LocalSet(field));
            plan_i32_store(plan, local, field, 0, local.scratch[7]);
            plan.push(P::End);
            plan_start(plan, local, *dict)?;
            plan.push(P::LocalSet(field));
            plan_i32_store(plan, local, field, 12, local.scratch[7]);
            plan.extend([P::LocalGet(local.scratch[10]), P::I32Const(1), P::I32Add,
                P::LocalSet(local.scratch[10])]);
            plan_i32_store(plan, local, field, 0, local.scratch[10]);
            plan.extend([P::LocalGet(local.scratch[5]), P::I32Const(2), P::I32Eq, P::If("Empty".into())]);
            plan_address_at(plan, local, field, 16);
            plan.extend([pload32(), P::I32Const(1), P::I32Sub, P::LocalSet(local.scratch[11])]);
            plan_i32_store(plan, local, field, 16, local.scratch[11]);
            plan.extend([P::End, P::I32Const(1), P::LocalSet(local.scratch[9])]);
            plan_i32_store(plan, local, row, 0, local.scratch[9]);
            push_get(plan, local, *hash)?;
            plan.extend([P::I32WrapI64, P::I32Const(25), P::I32ShrU, P::LocalSet(local.scratch[9])]);
            push_get(plan, local, *bucket)?;
            plan.push(P::LocalSet(local.scratch[11]));
            plan_write_control(plan, local, *dict, local.scratch[11], local.scratch[9])?;
            Ok(())
        }
        DictionaryOperation::RemoveSlot { removed_key, removed_value, dict, bucket } => {
            use crate::canonical_safewir::dictionary::DictionarySlotField;
            let key_drop = key_pointee(function, layouts, *dict).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?.filter(|_| removed_key.is_none())
                .map(|pointee| local.memory_helpers.as_ref().and_then(|helpers| helpers.drops.get(&pointee).copied())
                    .ok_or(CanonicalAdmissionError::UnsupportedPlan)).transpose()?;
            plan_slot_address(plan, local, *dict, *bucket, DictionarySlotField::Value)?;
            let (row, neighbour, header) = (plan_address_local(local, 4)?, plan_address_local(local, 7)?,
                plan_address_local(local, 8)?);
            plan_address_at_const(plan, local, row, 24);
            plan.push(plan_load_for(function, *removed_value)?);
            push_set(plan, local, *removed_value)?;
            if let Some(removed_key) = removed_key {
                plan_address_at_const(plan, local, row, 16);
                plan.push(plan_load_for(function, *removed_key)?);
                push_set(plan, local, *removed_key)?;
            }
            if key_drop.is_some() {
                plan_address_at_const(plan, local, row, 16);
                plan.extend([pload32(), P::LocalSet(local.scratch[11])]);
            }
            for (offset, scratch) in [(8, 5), (12, 6)] {
                plan_address_at_const(plan, local, row, offset);
                plan.extend([pload32(), P::LocalSet(local.scratch[scratch])]);
            }
            plan_start(plan, local, *dict)?;
            plan.extend([P::LocalSet(header), P::I32Const(0), P::LocalSet(local.scratch[9]),
                P::LocalGet(local.scratch[6]), P::I32Eqz, P::If("Empty".into())]);
            plan_i32_store(plan, local, header, 8, local.scratch[5]);
            plan.extend([P::Else, P::LocalGet(header), P::LocalGet(local.scratch[6]),
                P::I32Const(1), P::I32Sub, P::I32Const(32), P::I32Mul]);
            local.refs.plan_add(plan);
            plan_offset(plan, local, 28);
            plan.push(P::LocalSet(neighbour));
            plan_i32_store(plan, local, neighbour, 0, local.scratch[5]);
            plan.extend([P::End, P::LocalGet(local.scratch[5]), P::I32Eqz, P::If("Empty".into())]);
            plan_i32_store(plan, local, header, 12, local.scratch[6]);
            plan.extend([P::Else, P::LocalGet(header), P::LocalGet(local.scratch[5]),
                P::I32Const(1), P::I32Sub, P::I32Const(32), P::I32Mul]);
            local.refs.plan_add(plan);
            plan_offset(plan, local, 32);
            plan.push(P::LocalSet(neighbour));
            plan_i32_store(plan, local, neighbour, 0, local.scratch[6]);
            plan.push(P::End);
            for (offset, delta) in [(0, -1), (16, 1)] {
                plan_address_at_const(plan, local, header, offset);
                plan.extend([pload32(), P::I32Const(delta), P::I32Add, P::LocalSet(local.scratch[10])]);
                plan_i32_store(plan, local, header, offset, local.scratch[10]);
            }
            plan.extend([P::I32Const(2), P::LocalSet(local.scratch[10])]);
            plan_i32_store(plan, local, row, 0, local.scratch[10]);
            push_get(plan, local, *bucket)?;
            plan.extend([P::LocalSet(local.scratch[7]), P::I32Const(0xFE), P::LocalSet(local.scratch[10])]);
            plan_write_control(plan, local, *dict, local.scratch[7], local.scratch[10])?;
            for offset in [16, 24] {
                plan_address_at_const(plan, local, row, offset);
                plan.extend([P::I64Const(0), P::I64Store { offset: 0, align: 3, memory: 0 }]);
            }
            if let Some(drop) = key_drop { plan.extend([P::LocalGet(local.scratch[11]), P::Call(drop)]); }
            Ok(())
        }
        _ => Err(CanonicalAdmissionError::UnsupportedPlan),
    }
}
