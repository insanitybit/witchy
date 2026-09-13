//! Mechanical owner recipes. Opaque references never travel through linear
//! storage; linear environments use the ordinary allocator and RC destructor.
use super::*;
use std::borrow::Cow;
use crate::canonical_safewir::closure_owner_contract::{CaptureStorage, OwnerOperation as O};

const ENV: ValueId = ValueId::new(u32::MAX);
const TEMP: ValueId = ValueId::new(u32::MAX - 1);

fn context<'a>(function: &'a Function, local: &'a LocalLayout<'a>, op: &O) -> Result<(Cow<'a, Function>, Cow<'a, LocalLayout<'a>>, &'a AdmittedOwnerBridge, u32), EncodeError> {
    let error = || EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "owner kernel context" });
    let bridge = local.owner_bridges.iter().find(|b| b.recipe() == &OwnerBridgeRecipe::of(op)).ok_or_else(error)?;
    let scratch = local.owner_scratch.ok_or_else(error)?;
    if function.value_types.contains_key(&ENV) || function.value_types.contains_key(&TEMP) { return Err(error()); }
    // Only environment construction/extraction uses the synthetic locals.
    // Ordinary owner calls must not copy the source CFG and the complete
    // module-wide cleanup catalog once per instruction.
    if !matches!(op, O::Construct { .. } | O::CloneEnvironment { .. }) {
        return Ok((Cow::Borrowed(function), Cow::Borrowed(local), bridge, scratch[2]));
    }
    let mut function = function.clone();
    let mut local = LocalLayout { memory_helpers: local.memory_helpers.clone(),
        owner_scratch: local.owner_scratch,
        owner_bridges: Vec::new(),
        indices: local.indices.clone(),
        extra_types: local.extra_types.clone(),
        scratch: local.scratch.clone(),
    };
    function.value_types.insert(ENV, ValueType::Ref(bridge.family().environment));
    local.indices.insert(ENV, scratch[0]);
    local.indices.insert(TEMP, scratch[1]);
    Ok((Cow::Owned(function), Cow::Owned(local), bridge, scratch[2]))
}

fn release_binding(bridge: &AdmittedOwnerBridge, release_import: u32) -> closure_owner_kernel::ReleaseBinding {
    let families = bridge.cleanup_families().iter().map(|candidate| {
        closure_owner_kernel::CleanupFamilyBinding {
            tag: candidate.family().signature.get(),
            payload_type: candidate.payload_type_index(),
            environment: candidate.family().environment,
            environment_field: 0,
        }
    }).collect();
    closure_owner_kernel::ReleaseBinding {
        release_import,
        worklist_type: bridge.cleanup_type_index(),
        item_type: bridge.cleanup_item_type_index(),
        families,
    }
}

fn cleanup_helper_context(bridge: &AdmittedOwnerBridge) -> (Function, LocalLayout) {
    let environment = ENV;
    let index = TEMP;
    let scratch_start = 3;
    let function = Function {
        id: FunctionId::new(u32::MAX),
        signature: crate::canonical_safewir::FunctionSignatureId::new(u32::MAX),
        entry: crate::canonical_safewir::BlockId::new(0),
        value_types: [
            (environment, ValueType::Ref(bridge.family().environment)),
            (index, ValueType::Size),
        ]
        .into_iter()
        .collect(),
        blocks: Vec::new(),
    };
    let local = LocalLayout { memory_helpers: None,
        owner_scratch: None,
        owner_bridges: Vec::new(),
        indices: [(environment, 1), (index, 2)].into_iter().collect(),
        extra_types: vec![ValType::I32; 2 + drop_worklist::SCRATCH],
        scratch: (scratch_start..scratch_start + drop_worklist::SCRATCH as u32).collect(),
    };
    (function, local)
}

pub(super) fn emit_cleanup_helper(
    layouts: &LayoutInterner,
    bridge: &AdmittedOwnerBridge,
) -> Result<WasmFunctionEncoder, EncodeError> {
    let (function, local) = cleanup_helper_context(bridge);
    let mut body = WasmFunctionEncoder::new([(2 + drop_worklist::SCRATCH as u32, ValType::I32)]);
    closure_owner_kernel::emit_cleanup_helper(
        &mut body,
        &local,
        &function,
        layouts,
        0,
        ENV,
        TEMP,
        &release_binding(bridge, 0),
    )?;
    Ok(body)
}

pub(super) fn plan_cleanup_helper(
    layouts: &LayoutInterner,
    bridge: &AdmittedOwnerBridge,
) -> Result<(Vec<(u32, String)>, Vec<PlanToken>), CanonicalAdmissionError> {
    let (function, local) = cleanup_helper_context(bridge);
    let mut plan = Vec::new();
    closure_owner_kernel::plan_cleanup_helper(
        &mut plan,
        &local,
        &function,
        layouts,
        0,
        ENV,
        TEMP,
        &release_binding(bridge, 0),
    )?;
    Ok((vec![(2 + drop_worklist::SCRATCH as u32, "i32".to_owned())], plan))
}

pub(super) fn emit(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, layouts: &LayoutInterner, op: &O, imports: &BTreeMap<ImportId, u32>, cleanup_function: Option<u32>) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    let (mut function, local, bridge, _payload) = context(function, local, op)?;
    let import = *imports.get(&bridge.id()).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "owner bridge import" }))?;
    match op {
        O::Construct { destination, constructor, captures } => {
            let constructor = bridge.family().constructors.iter().find(|c| c.id == *constructor).expect("admitted constructor");
            let environment = bridge.family().environment;
            let physical = resolve_backend_layout(function.id, layouts, environment, "owner environment").map_err(EncodeError::Lowering)?; let descriptor = layouts.get(physical).ok_or(EncodeError::Lowering(LoweringError::UnsupportedLayout { function: function.id, operation: "owner environment" }))?;
            let LayoutSize::Fixed(size) = descriptor.size() else { return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "dynamic owner environment" })); };
            let extent = Extent::FixedBytes(u64::from(size));
            emit_alloc(body, &function, &local, layouts, ENV, environment, &extent)?;
            emit_get(body, &local, function.id, ENV)?;
            body.instruction(&I::I32Const(0));
            body.instruction(&I::I32Const(size as i32));
            body.instruction(&I::MemoryFill(0));
            let tag = field_access(function.id, layouts, ENV, environment, 0).map_err(EncodeError::Lowering)?;
            emit_projected_address(body, &function, &local, tag)?;
            body.instruction(&I::I64Const(constructor.environment_tag));
            body.instruction(&I::I64Store(memarg(8)));
            for (capture, value) in constructor.captures.iter().zip(captures) {
                let CaptureStorage::EnvironmentField(field) = capture.storage else { continue; };
                let value = if matches!(capture.value_type, ValueType::Ref(_)) {
                    function.to_mut().value_types.insert(TEMP, capture.value_type);
                    emit_clone_ref(body, &local, TEMP, *value, &function)?;
                    TEMP
                } else { *value };
                let access = field_access(function.id, layouts, ENV, environment, field).map_err(EncodeError::Lowering)?;
                emit_typed_store(body, &function, &local, access, value)?;
            }
            emit_get(body, &local, function.id, ENV)?;
            for (capture, value) in constructor.captures.iter().zip(captures) {
                if matches!(capture.storage, CaptureStorage::OwnerField(_)) { emit_get(body, &local, function.id, *value)?; }
            }
            body.instruction(&I::Call(import));
            emit_set(body, &local, function.id, *destination)?;
        }
        O::Drop { owner, .. } => {
            emit_get(body, &local, function.id, *owner)?;
            body.instruction(&I::Call(import));
            body.instruction(&I::Call(cleanup_function.ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "owner cleanup helper" },
            ))?));
        }
        O::CloneEnvironment { owner, destination, .. } => {
            emit_get(body, &local, function.id, *owner)?;
            body.instruction(&I::Call(import));
            body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(bridge.payload_type_index())));
            body.instruction(&I::StructGet { struct_type_index: bridge.payload_type_index(), field_index: 0 });
            emit_set(body, &local, function.id, ENV)?;
            emit_clone_ref(body, &local, *destination, ENV, &function)?;
        }
        O::ListPush { .. } | O::ListSet { .. } => {
            for value in op.operands() { emit_get(body, &local, function.id, value)?; }
            body.instruction(&I::Call(import));
            body.instruction(&I::Call(cleanup_function.ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "owner cleanup helper" },
            ))?));
            for value in op.destinations().into_iter().rev() { emit_set(body, &local, function.id, value)?; }
        }
        O::ReadCapability { .. } | O::ReadOwner { .. } | O::ReadTag { .. } | O::ReadCallable { .. } | O::Clone { .. }
        | O::NewList { .. } | O::ListGet { .. } | O::ListLength { .. } | O::CloneList { .. }
        => {
            for value in op.operands() { emit_get(body, &local, function.id, value)?; }
            body.instruction(&I::Call(import));
            for value in op.destinations().into_iter().rev() { emit_set(body, &local, function.id, value)?; }
        }
O::DropList { list, .. } => {
    emit_get(body, &local, function.id, *list)?;
    body.instruction(&I::Call(import));
    body.instruction(&I::Call(cleanup_function.ok_or(EncodeError::Lowering(
        LoweringError::UnsupportedOperation { function: function.id, operation: "owner cleanup helper" },
    ))?));
}

    }
    Ok(())
}

pub(super) fn plan(plan: &mut Vec<PlanToken>, function: &Function, layouts: &LayoutInterner, local: &LocalLayout, op: &O, imports: &BTreeMap<ImportId, u32>, cleanup_function: Option<u32>) -> Result<(), CanonicalAdmissionError> {
    let (mut function, local, bridge, _payload) = context(function, local, op).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let import = *imports.get(&bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    match op {
        O::Construct { destination, constructor, captures } => {
            let constructor = bridge.family().constructors.iter().find(|c| c.id == *constructor).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let environment = bridge.family().environment;
            let physical = resolve_backend_layout(function.id, layouts, environment, "owner environment").map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?; let descriptor = layouts.get(physical).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let LayoutSize::Fixed(size) = descriptor.size() else { return Err(CanonicalAdmissionError::UnsupportedPlan); };
            expected_alloc_plan(plan, &function, &local, None, ENV, &Extent::FixedBytes(u64::from(size)))?;
            push_get(plan, &local, ENV)?;
            plan.extend([PlanToken::I32Const(0), PlanToken::I32Const(size as i32), PlanToken::MemoryFill(0)]);
            let access = field_access(function.id, layouts, ENV, environment, 0).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_projected_address_plan(plan, &function, &local, access)?;
            plan.extend([PlanToken::I64Const(constructor.environment_tag), PlanToken::I64Store { offset: 0, align: 3, memory: 0 }]);
            for (capture, value) in constructor.captures.iter().zip(captures) {
                let CaptureStorage::EnvironmentField(field) = capture.storage else { continue; };
                let value = if matches!(capture.value_type, ValueType::Ref(_)) {
                    function.to_mut().value_types.insert(TEMP, capture.value_type);
                    push_share_plan(plan, &local, TEMP, *value)?;
                    TEMP
                } else { *value };
                let place = Place::projected(ENV, Projection::Field { index: field }, capture.value_type, AccessMode::Write);
                push_memory_store_plan(plan, &function, layouts, &local, &place, value)?;
            }
            push_get(plan, &local, ENV)?;
            for (capture, value) in constructor.captures.iter().zip(captures) {
                if matches!(capture.storage, CaptureStorage::OwnerField(_)) { push_get(plan, &local, *value)?; }
            }
            plan.push(PlanToken::Call(import));
            push_set(plan, &local, *destination)?;
        }
        O::CloneEnvironment { owner, destination, .. } => {
            push_get(plan, &local, *owner)?;
            plan.push(PlanToken::Call(import));
            plan.extend([
                PlanToken::RefCastNonNull(bridge.payload_type_index()),
                PlanToken::StructGet { struct_type_index: bridge.payload_type_index(), field_index: 0 },
            ]);
            push_set(plan, &local, ENV)?;
            push_share_plan(plan, &local, *destination, ENV)?;
        }
        O::Drop { owner, .. } => {
            push_get(plan, &local, *owner)?;
            plan.push(PlanToken::Call(import));
            plan.push(PlanToken::Call(cleanup_function.ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
        }
        O::ListPush { .. } | O::ListSet { .. } => {
            for value in op.operands() { push_get(plan, &local, value)?; }
            plan.push(PlanToken::Call(import));
            plan.push(PlanToken::Call(cleanup_function.ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
            for value in op.destinations().into_iter().rev() { push_set(plan, &local, value)?; }
        }
        O::ReadCapability { .. } | O::ReadOwner { .. } | O::ReadTag { .. } | O::ReadCallable { .. } | O::Clone { .. }
        | O::NewList { .. } | O::ListGet { .. } | O::ListLength { .. } | O::CloneList { .. }
        => {
            for value in op.operands() { push_get(plan, &local, value)?; }
            plan.push(PlanToken::Call(import));
            for value in op.destinations().into_iter().rev() { push_set(plan, &local, value)?; }
        }
O::DropList { list, .. } => {
    push_get(plan, &local, *list)?;
    plan.push(PlanToken::Call(import));
    plan.push(PlanToken::Call(cleanup_function.ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
}

    }
    Ok(())
}
