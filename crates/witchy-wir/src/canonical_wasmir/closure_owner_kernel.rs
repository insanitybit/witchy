//! Mechanical opaque-owner teardown. The release bridge retires the owner and
//! returns its rooted payload only on the final release. Linear ownership stays
//! with the ordinary canonical destructor; no GC finalizer drops RC storage.
use super::*;
use crate::layout::LayoutId;

// All indices below are private encoder metadata, not guest operands. Source
// admission must bind this recipe to the exact constructor/family before use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CleanupFamilyBinding {
    pub tag: u32,
    pub payload_type: u32,
    pub environment: LayoutId,
    pub environment_field: u32,
}

pub(super) struct ReleaseBinding {
    pub release_import: u32,
    pub worklist_type: u32,
    pub item_type: u32,
    pub families: Vec<CleanupFamilyBinding>,
}

fn emit_worklist_item_field(
    body: &mut WasmFunctionEncoder,
    payload_local: u32,
    index: ValueId,
    local: &LocalLayout,
    function: FunctionId,
    binding: &ReleaseBinding,
    field: u32,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    body.instruction(&I::LocalGet(payload_local));
    body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(binding.worklist_type)));
    emit_get(body, local, function, index)?;
    body.instruction(&I::ArrayGet(binding.worklist_type));
    body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(binding.item_type)));
    body.instruction(&I::StructGet { struct_type_index: binding.item_type, field_index: field });
    Ok(())
}

fn emit_cleanup_worklist(
    body: &mut WasmFunctionEncoder,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    body.instruction(&I::LocalTee(payload_local));
    body.instruction(&I::RefIsNull);
    body.instruction(&I::If(BlockType::Empty));
    body.instruction(&I::Else);
    body.instruction(&I::I32Const(0));
    emit_set(body, local, function.id, index)?;
    body.instruction(&I::Block(BlockType::Empty));
    body.instruction(&I::Loop(BlockType::Empty));
    emit_get(body, local, function.id, index)?;
    body.instruction(&I::LocalGet(payload_local));
    body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(binding.worklist_type)));
    body.instruction(&I::ArrayLen);
    body.instruction(&I::I32GeU);
    body.instruction(&I::BrIf(1));
    body.instruction(&I::Block(BlockType::Empty));
    for family in &binding.families {
        emit_worklist_item_field(body, payload_local, index, local, function.id, binding, 0)?;
        body.instruction(&I::I32Const(family.tag as i32));
        body.instruction(&I::I32Eq);
        body.instruction(&I::If(BlockType::Empty));
        emit_worklist_item_field(body, payload_local, index, local, function.id, binding, 1)?;
        body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(family.payload_type)));
        body.instruction(&I::StructGet { struct_type_index: family.payload_type, field_index: family.environment_field });
        emit_set(body, local, function.id, environment)?;
        let mut family_function = function.clone();
        family_function.value_types.insert(environment, ValueType::Ref(family.environment));
        emit_release(body, local, &family_function, environment, layouts)?;
        body.instruction(&I::Br(1));
        body.instruction(&I::End);
    }
    body.instruction(&I::Unreachable);
    body.instruction(&I::End);
    emit_get(body, local, function.id, index)?;
    body.instruction(&I::I32Const(1));
    body.instruction(&I::I32Add);
    emit_set(body, local, function.id, index)?;
    body.instruction(&I::Br(0));
    body.instruction(&I::End);
    body.instruction(&I::End);
    body.instruction(&I::End);
    Ok(())
}

pub(super) fn emit_cleanup_helper(
    body: &mut WasmFunctionEncoder,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    payload_parameter: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), EncodeError> {
    body.instruction(&wasm_encoder::Instruction::LocalGet(payload_parameter));
    emit_cleanup_worklist(
        body,
        local,
        function,
        layouts,
        payload_parameter,
        environment,
        index,
        binding,
    )?;
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(())
}

pub(super) fn emit_final_release(
    body: &mut WasmFunctionEncoder,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    owner: ValueId,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    emit_get(body, local, function.id, owner)?;
    body.instruction(&I::Call(binding.release_import));
    emit_cleanup_worklist(body, local, function, layouts, payload_local, environment, index, binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn opaque_owner_cleanup_reclaims_once_and_rejects_unknown_family() -> Result<(), Box<dyn std::error::Error>> {
        let mut layouts = LayoutInterner::new();
        let byte = layouts.intern_scalar(ScalarKind::RawByte)?;
        let string = layouts.intern_runtime_buffer(byte)?;
        let slot = layouts.intern_runtime_pointer_value_slot(string, PointerSlot::Owned)?;
        let environment = layouts.intern_packed_record(vec![slot])?;
        let owner = ValueId::new(0);
        let env = ValueId::new(1);
        let original = ValueId::new(2);
        let index = ValueId::new(3);
        let function = Function {
            id: FunctionId::new(0), signature: crate::canonical_safewir::FunctionSignatureId::new(0),
            entry: crate::canonical_safewir::BlockId::new(0),
            value_types: [(owner, ValueType::Gc(crate::canonical_safewir::GcTypeId::new(0))), (env, ValueType::Ref(environment)), (original, ValueType::Ref(string))].into_iter().collect(),
            blocks: Vec::new(),
        };
        let count = drop_worklist::SCRATCH as u32;
        let local = LocalLayout { memory_helpers: None, owner_scratch: None, owner_bridges: Vec::new(),
            indices: [(owner, 0), (env, 2), (index, 3)].into_iter().collect(),
            extra_types: Vec::new(), scratch: (4..4 + count).collect(), owner_context: Default::default(),
        };
        let mut drop_owner = WasmFunctionEncoder::new([(1, ValType::Ref(RefType::ANYREF)), (count + 2, ValType::I32)]);
        emit_final_release(&mut drop_owner, &local, &function, &layouts, owner, 1, env,
            index, &ReleaseBinding {
                release_import: 0,
                worklist_type: 2,
                item_type: 1,
                families: vec![CleanupFamilyBinding {
                    tag: 7,
                    payload_type: 0,
                    environment,
                    environment_field: 0,
                }],
            }).unwrap();
        drop_owner.instruction(&wasm_encoder::Instruction::End);
        let original_local = LocalLayout { memory_helpers: None, owner_scratch: None, owner_bridges: Vec::new(),
            indices: [(original, 0)].into_iter().collect(), extra_types: Vec::new(), scratch: (1..1 + count).collect(),
            owner_context: Default::default(),
        };
        let mut drop_original = WasmFunctionEncoder::new([(count, ValType::I32)]);
        emit_release(&mut drop_original, &original_local, &function, original, &layouts).unwrap();
        drop_original.instruction(&wasm_encoder::Instruction::End);

        let mut types = TypeSection::new();
        types.ty().struct_([
            wasm_encoder::FieldType { element_type: wasm_encoder::StorageType::Val(ValType::I32), mutable: false },
            wasm_encoder::FieldType { element_type: wasm_encoder::StorageType::Val(ValType::EXTERNREF), mutable: false },
        ]);
        types.ty().struct_([
            wasm_encoder::FieldType { element_type: wasm_encoder::StorageType::Val(ValType::I32), mutable: false },
            wasm_encoder::FieldType { element_type: wasm_encoder::StorageType::Val(ValType::Ref(RefType::ANYREF)), mutable: false },
        ]);
        types.ty().array(&wasm_encoder::StorageType::Val(ValType::Ref(RefType::ANYREF)), false);
        types.ty().function([ValType::EXTERNREF], [ValType::Ref(RefType::ANYREF)]);
        types.ty().function([ValType::EXTERNREF], []);
        types.ty().function([ValType::I32], []);
        let mut imports = ImportSection::new();
        imports.import("kernel_test", "release", EntityType::Function(3));
        let mut functions = FunctionSection::new();
        functions.function(4);
        functions.function(5);
        let mut memory = MemorySection::new();
        memory.memory(MemoryType { minimum: 1, maximum: Some(1), memory64: false, shared: false, page_size_log2: None });
        let mut globals = GlobalSection::new();
        for (value, mutable) in [(160, true), (0, true), (16, false)] {
            globals.global(GlobalType { val_type: ValType::I32, mutable, shared: false }, &ConstExpr::i32_const(value));
        }
        let mut exports = ExportSection::new();
        exports.export("drop_owner", ExportKind::Func, 1);
        exports.export("drop_original", ExportKind::Func, 2);
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("free_head", ExportKind::Global, 1);
        let mut code = CodeSection::new();
        code.function(&drop_owner);
        code.function(&drop_original);
        // A real canonical String root at 64 and its owning environment at
        // 128. The original String and environment each own one String token.
        let mut bytes = vec![0_u8; 144];
        for (offset, value) in [(56, 2_u32), (60, 16), (64, 3), (68, 3), (120, 1), (124, 8), (128, 64)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[72..75].copy_from_slice(b"abc");
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(0), bytes);
        let mut module = WasmModule::new();
        module.section(&types).section(&imports).section(&functions).section(&memory).section(&globals).section(&exports).section(&code).section(&data);
        let mut config = wasmtime::Config::new();
        config.wasm_function_references(true).wasm_gc(true);
        let engine = wasmtime::Engine::new(&config)?;
        let module = wasmtime::Module::new(&engine, module.finish())
            .unwrap_or_else(|error| panic!("opaque owner kernel fixture module: {error:#}"));
        let mut store = wasmtime::Store::new(&engine, ());
        struct TestOwner { payload: wasmtime::OwnedRooted<wasmtime::AnyRef>, remaining: Mutex<usize>, tag: i32 }
        let mut linker = wasmtime::Linker::new(&engine);
        linker.func_wrap("kernel_test", "release", |mut caller: wasmtime::Caller<'_, ()>, owner: Option<wasmtime::Rooted<wasmtime::ExternRef>>| -> wasmtime::Result<Option<wasmtime::Rooted<wasmtime::AnyRef>>> {
            let owner = owner.ok_or_else(|| wasmtime::Error::msg("null owner"))?;
            let final_item = {
                let owner = owner.data(&caller)?.and_then(|data| data.downcast_ref::<TestOwner>()).ok_or_else(|| wasmtime::Error::msg("wrong owner"))?;
                let mut remaining = owner.remaining.lock().unwrap();
                if *remaining == 0 { wasmtime::bail!("dead owner"); }
                *remaining -= 1;
                (*remaining == 0).then(|| (owner.payload.clone(), owner.tag))
            };
            let Some((payload, tag)) = final_item else { return Ok(None); };
            let item_type = wasmtime::StructType::new(caller.engine(), [
                wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::I32.into()),
                wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::ANYREF.into()),
            ])?;
            let item_pre = wasmtime::StructRefPre::new(&mut caller, item_type);
            let payload = payload.to_rooted(&mut caller);
            let item = wasmtime::StructRef::new(&mut caller, &item_pre, &[
                wasmtime::Val::I32(tag),
                wasmtime::Val::AnyRef(Some(payload)),
            ])?;
            let array_type = wasmtime::ArrayType::new(
                caller.engine(),
                wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::ANYREF.into()),
            );
            let array_pre = wasmtime::ArrayRefPre::new(&mut caller, array_type);
            let array = wasmtime::ArrayRef::new_fixed(
                &mut caller,
                &array_pre,
                &[wasmtime::Val::AnyRef(Some(item.to_anyref()))],
            )?;
            Ok(Some(array.to_anyref()))
        })?;
        let instance = linker.instantiate(&mut store, &module)?;
        let payload_type = wasmtime::StructType::new(&engine, [
            wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::I32.into()),
            wasmtime::FieldType::new(wasmtime::Mutability::Const, wasmtime::ValType::EXTERNREF.into()),
        ])?;
        let pre = wasmtime::StructRefPre::new(&mut store, payload_type);
        let cap = wasmtime::ExternRef::new(&mut store, "opaque test capture")?;
        let payload = wasmtime::StructRef::new(&mut store, &pre, &[wasmtime::Val::I32(128), wasmtime::Val::ExternRef(Some(cap))])?.to_anyref().to_owned_rooted(&mut store)?;
        let owner = wasmtime::ExternRef::new(&mut store, TestOwner { payload: payload.clone(), remaining: Mutex::new(2), tag: 7 })?;
        let wrong_owner = wasmtime::ExternRef::new(&mut store, TestOwner { payload, remaining: Mutex::new(1), tag: 8 })?;
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let word = |store: &wasmtime::Store<()>, offset: usize| u32::from_le_bytes(memory.data(store)[offset..offset + 4].try_into().unwrap());
        instance.get_typed_func::<i32, ()>(&mut store, "drop_original")?.call(&mut store, 64)?;
        assert_eq!(word(&store, 56), 1);
        let drop = instance.get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "drop_owner")?;
        drop.call(&mut store, Some(owner))?;
        assert_eq!(word(&store, 56), 1);
        assert_eq!(&memory.data(&store)[72..75], b"abc");
        assert_eq!(instance.get_global(&mut store, "free_head").unwrap().get(&mut store).i32(), Some(0));
        drop.call(&mut store, Some(owner))?;
        // Reclaimed blocks are recorded in the free list; their dead RC
        // headers are not zeroed. Require both roots exactly once in the
        // terminated free chain, not a value in an obsolete header.
        assert_eq!(instance.get_global(&mut store, "free_head").unwrap().get(&mut store).i32(), Some(128));
        assert_eq!(word(&store, 128), 64);
        assert_eq!(word(&store, 64), 0);
        assert!(drop.call(&mut store, Some(owner)).is_err());
        assert!(drop.call(&mut store, Some(wrong_owner)).is_err());
        Ok(())
    }
}

/// The bridge retires the list and its element tokens. Its nullable array
/// contains only final payloads, each reclaimed once by the canonical RC walk.
pub(super) fn emit_list_release(
    body: &mut WasmFunctionEncoder,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    list: ValueId,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    emit_get(body, local, function.id, list)?;
    body.instruction(&I::Call(binding.release_import));
    emit_cleanup_worklist(body, local, function, layouts, payload_local, environment, index, binding)
}

pub(super) fn plan_list_release(
    plan: &mut Vec<PlanToken>,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    list: ValueId,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, list)?;
    plan.push(PlanToken::Call(binding.release_import));
    plan_cleanup_worklist(plan, local, function, layouts, payload_local, environment, index, binding)
}

pub(super) fn plan_final_release(
    plan: &mut Vec<PlanToken>,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    owner: ValueId,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), CanonicalAdmissionError> {
    push_get(plan, local, owner)?;
    plan.push(PlanToken::Call(binding.release_import));
    plan_cleanup_worklist(plan, local, function, layouts, payload_local, environment, index, binding)
}

fn plan_worklist_item_field(
    plan: &mut Vec<PlanToken>,
    local: &LocalLayout,
    payload_local: u32,
    index: ValueId,
    binding: &ReleaseBinding,
    field: u32,
) -> Result<(), CanonicalAdmissionError> {
    plan.extend([PlanToken::LocalGet(payload_local), PlanToken::RefCastNonNull(binding.worklist_type)]);
    push_get(plan, local, index)?;
    plan.extend([
        PlanToken::ArrayGet(binding.worklist_type),
        PlanToken::RefCastNonNull(binding.item_type),
        PlanToken::StructGet { struct_type_index: binding.item_type, field_index: field },
    ]);
    Ok(())
}

fn plan_cleanup_worklist(
    plan: &mut Vec<PlanToken>,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    payload_local: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), CanonicalAdmissionError> {
    plan.extend([
        PlanToken::LocalTee(payload_local), PlanToken::RefIsNull,
        PlanToken::If("Empty".into()), PlanToken::Else, PlanToken::I32Const(0),
    ]);
    push_set(plan, local, index)?;
    plan.extend([PlanToken::Block("Empty".into()), PlanToken::Loop("Empty".into())]);
    push_get(plan, local, index)?;
    plan.extend([
        PlanToken::LocalGet(payload_local), PlanToken::RefCastNonNull(binding.worklist_type),
        PlanToken::ArrayLen, PlanToken::I32GeU, PlanToken::BrIf(1), PlanToken::Block("Empty".into()),
    ]);
    for family in &binding.families {
        plan_worklist_item_field(plan, local, payload_local, index, binding, 0)?;
        plan.extend([PlanToken::I32Const(family.tag as i32), PlanToken::I32Eq, PlanToken::If("Empty".into())]);
        plan_worklist_item_field(plan, local, payload_local, index, binding, 1)?;
        plan.extend([
            PlanToken::RefCastNonNull(family.payload_type),
            PlanToken::StructGet { struct_type_index: family.payload_type, field_index: family.environment_field },
        ]);
        push_set(plan, local, environment)?;
        let mut family_function = function.clone();
        family_function.value_types.insert(environment, ValueType::Ref(family.environment));
        push_drop_ref_plan(plan, &family_function, layouts, local, environment)?;
        plan.extend([PlanToken::Br(1), PlanToken::End]);
    }
    plan.extend([PlanToken::Unreachable, PlanToken::End]);
    push_get(plan, local, index)?;
    plan.extend([PlanToken::I32Const(1), PlanToken::I32Add]);
    push_set(plan, local, index)?;
    plan.extend([PlanToken::Br(0), PlanToken::End, PlanToken::End, PlanToken::End]);
    Ok(())
}

pub(super) fn plan_cleanup_helper(
    plan: &mut Vec<PlanToken>,
    local: &LocalLayout,
    function: &Function,
    layouts: &LayoutInterner,
    payload_parameter: u32,
    environment: ValueId,
    index: ValueId,
    binding: &ReleaseBinding,
) -> Result<(), CanonicalAdmissionError> {
    plan.push(PlanToken::LocalGet(payload_parameter));
    plan_cleanup_worklist(
        plan,
        local,
        function,
        layouts,
        payload_parameter,
        environment,
        index,
        binding,
    )?;
    plan.push(PlanToken::End);
    Ok(())
}
