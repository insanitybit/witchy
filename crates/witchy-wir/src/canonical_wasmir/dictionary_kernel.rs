//! Guest-resident canonical dictionary operations.

use super::*;
use crate::canonical_safewir::dictionary::DictionaryOperation;

fn counted_extent(capacity: ValueId) -> Extent {
    Extent::Counted { header_bytes: 20, count: capacity, capacity, stride_bytes: 32 }
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

fn emit_capacity_guard(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, capacity: ValueId) -> Result<(), EncodeError> {
    emit_get(body, local, function.id, capacity)?;
    body.instruction(&wasm_encoder::Instruction::I32Const(8));
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
    plan.extend([PlanToken::I32Const(8), PlanToken::I32LtU, PlanToken::If("Empty".into()), PlanToken::Unreachable, PlanToken::End]);
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
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            emit_get(body, local, function.id, *dict)?;
            body.instruction(&wasm_encoder::Instruction::I32Const(header_offset(*field)));
            body.instruction(&wasm_encoder::Instruction::I32Add);
            body.instruction(&wasm_encoder::Instruction::I32Load(memarg(4)));
            emit_set(body, local, function.id, *destination)
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
            Ok(())
        }
        DictionaryOperation::ReadHeader { destination, dict, field } => {
            push_get(plan, local, *dict)?;
            plan.extend([PlanToken::I32Const(header_offset(*field)), PlanToken::I32Add,
                PlanToken::I32Load { offset: 0, align: 2, memory: 0 }]);
            push_set(plan, local, *destination)
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
            Ok(())
        }
        _ => Err(CanonicalAdmissionError::UnsupportedPlan),
    }
}
