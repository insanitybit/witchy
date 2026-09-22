//! Mechanical owner recipes. Opaque references never travel through linear
//! storage; linear environments use the ordinary allocator and RC destructor.
use super::*;
use std::borrow::Cow;
use crate::canonical_safewir::closure_owner_contract::{
    CaptureStorage, OwnerFamilySource, OwnerOperation as O, SharedPlaceBounds,
    SharedReferenceConstructor,
};

const ENV: ValueId = ValueId::new(u32::MAX);
const TEMP: ValueId = ValueId::new(u32::MAX - 1);
const INDEX: ValueId = ValueId::new(u32::MAX - 2);
const SLICE: ValueId = ValueId::new(u32::MAX - 3);
const START: ValueId = ValueId::new(u32::MAX - 4);

/// The lifetime-free parts of an owner-kernel context that are expensive to
/// build: the function's value types and the layout's local tables. They are
/// copied once per function into its `LocalLayout` and reused by every
/// environment operation of that function, which previously copied them per
/// operation (quadratic on the std executor's hundreds of constructs).
pub(super) struct OwnerScratch {
    function: Function,
    indices: BTreeMap<ValueId, u32>,
    extra_types: Vec<ValType>,
    scratch: Vec<u32>,
}

/// A function's parked context. Cloning a layout does not clone it: a copy
/// starts empty and builds its own on first use.
#[derive(Default)]
pub(super) struct ParkedContext(std::cell::RefCell<Option<Box<OwnerScratch>>>);

impl Clone for ParkedContext {
    fn clone(&self) -> Self { Self::default() }
}

const SYNTHETIC: [ValueId; 5] = [ENV, TEMP, INDEX, SLICE, START];

fn context<'a>(function: &'a Function, local: &'a LocalLayout<'a>, op: &O) -> Result<(Cow<'a, Function>, Cow<'a, LocalLayout<'a>>, &'a AdmittedOwnerBridge, u32), EncodeError> {
    let error = || EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "owner kernel context" });
    let bridge = local.owner_bridges.iter().find(|b| b.recipe() == &OwnerBridgeRecipe::of(op)).ok_or_else(error)?;
    let scratch = local.owner_scratch.ok_or_else(error)?;
    if SYNTHETIC.iter().any(|id| function.value_types.contains_key(id)) { return Err(error()); }
    // Only environment construction/extraction uses the synthetic locals.
    // Ordinary owner calls borrow the function and layout unchanged.
    if !matches!(op, O::Construct { .. } | O::CloneEnvironment { .. } | O::ReadSharedPlace { .. } | O::ReadSharedSliceLength { .. }) {
        return Ok((Cow::Borrowed(function), Cow::Borrowed(local), bridge, scratch[2]));
    }
    // Reuse this function's parked copy when there is one; the base tables
    // never change once emission starts, so only the synthetic entries a
    // previous operation added need clearing. The recipes read only the
    // function's identity and value types, never its CFG.
    let parked = local.owner_context.0.borrow_mut().take()
        .filter(|parked| parked.function.id == function.id
            && parked.function.value_types.len() >= function.value_types.len()
            && parked.indices.len() >= local.indices.len());
    let OwnerScratch { function: mut function, mut indices, extra_types, scratch: local_scratch } = match parked {
        Some(parked) => {
            let mut parked = *parked;
            for id in SYNTHETIC {
                parked.function.value_types.remove(&id);
                parked.indices.remove(&id);
            }
            parked
        }
        None => OwnerScratch {
            function: Function {
                id: function.id,
                signature: function.signature,
                entry: function.entry,
                value_types: function.value_types.clone(),
                blocks: Vec::new(),
            },
            indices: local.indices.clone(),
            extra_types: local.extra_types.clone(),
            scratch: local.scratch.clone(),
        },
    };
    let owner_bridges = if matches!(op, O::ReadSharedPlace { .. } | O::ReadSharedSliceLength { .. }) {
        local.owner_bridges.clone()
    } else {
        Vec::new()
    };
    function.value_types.insert(ENV, ValueType::Ref(bridge.family().environment));
    indices.insert(ENV, scratch[0]);
    indices.insert(TEMP, scratch[1]);
    if matches!(op, O::ReadSharedPlace { .. }) {
        function.value_types.insert(INDEX, ValueType::Size);
        // DropRef uses the ordinary worklist scratch locals, including slot
        // zero. Keep a slice coordinate in the dedicated owner slot so an
        // outer environment release cannot replace it with an address.
        indices.insert(INDEX, scratch[3]);
        if let Some(ValueType::Gc(slice_type)) = bridge.family().constructors.iter()
            .find_map(|constructor| match constructor.shared_reference_constructor() {
                Some(SharedReferenceConstructor::ListIntElementFromSlice { slice, .. }) =>
                    constructor.captures.get(slice.0 as usize).map(|capture| capture.value_type),
                _ => None,
            })
        {
            function.value_types.insert(SLICE, ValueType::Gc(slice_type));
            indices.insert(SLICE, scratch[2]);
            function.value_types.insert(START, ValueType::Size);
            indices.insert(START, local_scratch[1]);
        }
    }
    let local = LocalLayout { memory_helpers: local.memory_helpers.clone(),
        owner_scratch: local.owner_scratch,
        owner_bridges,
        indices,
        extra_types,
        scratch: local_scratch,
        owner_context: Default::default(),
    };
    Ok((Cow::Owned(function), Cow::Owned(local), bridge, scratch[2]))
}

/// Return an owned context to `original` for the next operation of the same
/// function. An operation that failed part-way simply does not park; the
/// next one rebuilds.
fn park(original: &LocalLayout<'_>, function: Cow<'_, Function>, local: Cow<'_, LocalLayout<'_>>) {
    if let (Cow::Owned(function), Cow::Owned(local)) = (function, local) {
        *original.owner_context.0.borrow_mut() = Some(Box::new(OwnerScratch {
            function,
            indices: local.indices,
            extra_types: local.extra_types,
            scratch: local.scratch,
        }));
    }
}

fn shared_place_capture<'a>(
    constructor: &'a crate::canonical_safewir::closure_owner_contract::OwnerConstructor,
    ordinal: crate::canonical_safewir::closure_owner_contract::CaptureOrdinal,
) -> Result<&'a crate::canonical_safewir::closure_owner_contract::OwnerCapture, LoweringError> {
    constructor.captures.get(ordinal.0 as usize).ok_or(LoweringError::UnsupportedOperation {
        function: FunctionId::new(u32::MAX),
        operation: "shared-place capture ordinal",
    })
}

fn shared_place_root(
    function: &mut Function,
    constructor: &crate::canonical_safewir::closure_owner_contract::OwnerConstructor,
    projection: SharedReferenceConstructor,
) -> Result<(u32, crate::layout::LayoutId), LoweringError> {
    let root = match projection {
        SharedReferenceConstructor::WholeIntCell { root }
        | SharedReferenceConstructor::ListIntElement { root, .. } => root,
        SharedReferenceConstructor::ListIntElementFromSlice { .. }
        | SharedReferenceConstructor::ListIntSlice { .. } => {
            return Err(LoweringError::UnsupportedOperation { function: function.id, operation: "shared-place slice carrier" });
        }
    };
    let capture = shared_place_capture(constructor, root)?;
    let (ValueType::Ref(layout), CaptureStorage::EnvironmentField(field)) =
        (capture.value_type, capture.storage)
    else {
        return Err(LoweringError::UnsupportedOperation { function: function.id, operation: "shared-place root capture" });
    };
    function.value_types.insert(TEMP, ValueType::Ref(layout));
    Ok((field, layout))
}

fn shared_place_index(
    function: FunctionId,
    constructor: &crate::canonical_safewir::closure_owner_contract::OwnerConstructor,
    ordinal: crate::canonical_safewir::closure_owner_contract::CaptureOrdinal,
) -> Result<u32, LoweringError> {
    let capture = shared_place_capture(constructor, ordinal)?;
    let (ValueType::Size, CaptureStorage::EnvironmentField(field)) = (capture.value_type, capture.storage) else {
        return Err(LoweringError::UnsupportedOperation { function, operation: "shared-place index capture" });
    };
    Ok(field)
}

/// Plan the exact typed memory load used by `emit_typed_load` when the
/// authenticated carrier supplies a layout that is more precise than the
/// synthetic local's outer type.  The ordinary place resolver cannot recover
/// that nested layout from `ENV` alone.
fn push_typed_load_plan(
    plan: &mut Vec<PlanToken>,
    function: &Function,
    local: &LocalLayout,
    access: MemoryAccess,
    destination: ValueId,
) -> Result<(), CanonicalAdmissionError> {
    push_projected_address_plan(plan, function, local, access)?;
    plan.push(match access.scalar {
        ScalarKind::Bool | ScalarKind::Tag8 | ScalarKind::RawByte =>
            PlanToken::I32Load8U { offset: 0, align: 0, memory: 0 },
        ScalarKind::Tag16 => PlanToken::I32Load16U { offset: 0, align: 1, memory: 0 },
        ScalarKind::Unit | ScalarKind::U32 | ScalarKind::Tag32 | ScalarKind::Callable =>
            PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
        ScalarKind::Int | ScalarKind::Duration =>
            PlanToken::I64Load { offset: 0, align: 3, memory: 0 },
        ScalarKind::Float => PlanToken::F64Load { offset: 0, align: 3, memory: 0 },
        _ => return Err(CanonicalAdmissionError::UnsupportedPlan),
    });
    if access.scalar == ScalarKind::Unit {
        plan.extend([PlanToken::Drop, PlanToken::I32Const(0)]);
    }
    push_set(plan, local, destination)
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
        owner_context: Default::default(),
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

fn emit_read_shared_place(
    body: &mut WasmFunctionEncoder,
    function: &mut Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    owner: ValueId,
    bridge: &AdmittedOwnerBridge,
    imports: &BTreeMap<ImportId, u32>,
    cleanup_function: Option<u32>,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    if !matches!(bridge.family().source, OwnerFamilySource::SharedReference(_)) {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id,
            operation: "shared-place family",
        }));
    }
    let tag_import = *imports.get(&bridge.id()).ok_or(EncodeError::Lowering(
        LoweringError::UnsupportedOperation { function: function.id, operation: "shared-place tag bridge" },
    ))?;
    body.instruction(&I::Block(BlockType::Empty));
    for constructor in &bridge.family().constructors {
        let projection = constructor.shared_reference_constructor().ok_or(EncodeError::Lowering(
            LoweringError::UnsupportedOperation { function: function.id, operation: "shared-place constructor schema" },
        ))?;
        let clone_bridge = local.owner_bridges.iter().find(|candidate| {
            candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(constructor.id)
        }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id,
            operation: "shared-place environment bridge",
        }))?;
        let clone_import = *imports.get(&clone_bridge.id()).ok_or(EncodeError::Lowering(
            LoweringError::UnsupportedOperation { function: function.id, operation: "shared-place environment import" },
        ))?;

        emit_get(body, local, function.id, owner)?;
        body.instruction(&I::Call(tag_import));
        body.instruction(&I::I64Const(constructor.environment_tag));
        body.instruction(&I::I64Eq);
        body.instruction(&I::If(BlockType::Empty));

        emit_get(body, local, function.id, owner)?;
        body.instruction(&I::Call(clone_import));
        body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(clone_bridge.payload_type_index())));
        body.instruction(&I::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 });
        emit_set(body, local, function.id, ENV)?;
        emit_clone_ref(body, local, ENV, ENV, function)?;

        if let SharedReferenceConstructor::ListIntElementFromSlice { slice, index, bounds: SharedPlaceBounds::CurrentLogicalLength } = projection {
            let index_field = shared_place_index(function.id, constructor, index)
                .map_err(EncodeError::Lowering)?;
            let index_access = field_access(function.id, layouts, ENV, bridge.family().environment, index_field)
                .map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, local, index_access, INDEX)?;
            // The child environment is no longer needed once the checked
            // local coordinate is copied out. The nested owner is obtained
            // through its artifact-derived bridge, never a payload address.
            emit_release(body, local, function, ENV, layouts)?;
            let read_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::ReadOwner(constructor.id, slice)
            }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice nested owner bridge",
            }))?;
            let read_import = *imports.get(&read_bridge.id()).ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice nested owner import" },
            ))?;
            let ValueType::Gc(nested_type) = function.value_types[&SLICE] else { unreachable!() };
            let slice_clone_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.family().owner_type == nested_type
                    && matches!(candidate.recipe(), OwnerBridgeRecipe::CloneEnvironment(_))
            }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice nested environment bridge",
            }))?;
            let nested_constructor = slice_clone_bridge.family().constructors.iter().find(|candidate| matches!(
                candidate.shared_reference_constructor(), Some(SharedReferenceConstructor::ListIntSlice { .. })
            )).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice nested constructor",
            }))?;
            let clone_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(nested_constructor.id)
            }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice exact nested environment bridge",
            }))?;
            let clone_import = *imports.get(&clone_bridge.id()).ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice nested environment import" },
            ))?;
            emit_get(body, local, function.id, owner)?;
            body.instruction(&I::Call(read_import));
            emit_set(body, local, function.id, SLICE)?;
            emit_get(body, local, function.id, SLICE)?;
            body.instruction(&I::Call(clone_import));
            body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(clone_bridge.payload_type_index())));
            body.instruction(&I::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 });
            emit_set(body, local, function.id, ENV)?;
            emit_clone_ref(body, local, ENV, ENV, function)?;
            let SharedReferenceConstructor::ListIntSlice { root, start, length } =
                nested_constructor.shared_reference_constructor().expect("matched above") else { unreachable!() };
            let nested_root_capture = shared_place_capture(nested_constructor, root).map_err(EncodeError::Lowering)?;
            let (ValueType::Ref(root_layout), CaptureStorage::EnvironmentField(nested_root_field)) =
                (nested_root_capture.value_type, nested_root_capture.storage) else {
                return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice nested root capture" }));
            };
            function.value_types.insert(TEMP, ValueType::Ref(root_layout));
            let root_access = field_access(function.id, layouts, ENV, clone_bridge.family().environment, nested_root_field)
                .map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, local, root_access, TEMP)?;
            emit_clone_ref(body, local, TEMP, TEMP, function)?;
            let length_field = shared_place_index(function.id, nested_constructor, length).map_err(EncodeError::Lowering)?;
            emit_get(body, local, function.id, INDEX)?;
            let length_access = field_access(function.id, layouts, ENV, clone_bridge.family().environment, length_field).map_err(EncodeError::Lowering)?;
            emit_projected_address(body, function, local, length_access)?;
            body.instruction(&I::I32Load(memarg(4)));
            body.instruction(&I::I32LtU);
            body.instruction(&I::I32Eqz);
            body.instruction(&I::If(BlockType::Empty)); body.instruction(&I::Unreachable); body.instruction(&I::End);
            let start_field = shared_place_index(function.id, nested_constructor, start).map_err(EncodeError::Lowering)?;
            let start_access = field_access(function.id, layouts, ENV, clone_bridge.family().environment, start_field).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, local, start_access, START)?;
            emit_get(body, local, function.id, START)?;
            emit_get(body, local, function.id, INDEX)?;
            body.instruction(&I::I32Add);
            emit_set(body, local, function.id, INDEX)?;
            emit_get(body, local, function.id, INDEX)?;
            emit_get(body, local, function.id, START)?;
            body.instruction(&I::I32LtU); body.instruction(&I::If(BlockType::Empty)); body.instruction(&I::Unreachable); body.instruction(&I::End);
            emit_get(body, local, function.id, INDEX)?;
            let current_length = resolve_memory_place_for_values(function.id, &function.value_types, layouts,
                &Place::projected(TEMP, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read),
            ).map_err(EncodeError::Lowering)?;
            emit_projected_address(body, function, local, current_length)?;
            body.instruction(&I::I32Load(memarg(4))); body.instruction(&I::I32LtU);
            body.instruction(&I::I32Eqz); body.instruction(&I::If(BlockType::Empty)); body.instruction(&I::Unreachable); body.instruction(&I::End);
            let element = resolve_memory_place_for_values(function.id, &function.value_types, layouts,
                &Place::projected(TEMP, Projection::Element { index: INDEX }, ValueType::Int, AccessMode::Read),
            ).map_err(EncodeError::Lowering)?;
            emit_typed_load(body, function, local, element, destination)?;
            emit_release(body, local, function, TEMP, layouts)?;
            emit_release(body, local, function, ENV, layouts)?;
            let drop_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::Drop(clone_bridge.family().signature)
            }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice nested drop bridge",
            }))?;
            let drop_import = *imports.get(&drop_bridge.id()).ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice nested drop import" },
            ))?;
            emit_get(body, local, function.id, SLICE)?;
            body.instruction(&I::Call(drop_import));
            body.instruction(&I::Call(cleanup_function.ok_or(EncodeError::Lowering(
                LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice nested drop cleanup" },
            ))?));
            body.instruction(&I::Br(1));
            body.instruction(&I::End);
            continue;
        }

        let (root_field, root_layout) = shared_place_root(function, constructor, projection)
            .map_err(EncodeError::Lowering)?;
        let root_access = field_access(function.id, layouts, ENV, bridge.family().environment, root_field)
            .map_err(EncodeError::Lowering)?;
        emit_typed_load(body, function, local, root_access, TEMP)?;
        emit_clone_ref(body, local, TEMP, TEMP, function)?;

        match projection {
            SharedReferenceConstructor::WholeIntCell { .. } => {
                let value = field_access(function.id, layouts, TEMP, root_layout, 0)
                    .map_err(EncodeError::Lowering)?;
                emit_typed_load(body, function, local, value, destination)?;
            }
            SharedReferenceConstructor::ListIntElement {
                index,
                bounds: SharedPlaceBounds::CurrentLogicalLength,
                ..
            } => {
                let index_field = shared_place_index(function.id, constructor, index)
                    .map_err(EncodeError::Lowering)?;
                let index_access = field_access(function.id, layouts, ENV, bridge.family().environment, index_field)
                    .map_err(EncodeError::Lowering)?;
                emit_typed_load(body, function, local, index_access, INDEX)?;
                emit_get(body, local, function.id, INDEX)?;
                let length = resolve_memory_place_for_values(
                    function.id,
                    &function.value_types,
                    layouts,
                    &Place::projected(TEMP, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read),
                ).map_err(EncodeError::Lowering)?;
                emit_projected_address(body, function, local, length)?;
                body.instruction(&I::I32Load(memarg(4)));
                body.instruction(&I::I32LtU);
                body.instruction(&I::I32Eqz);
                body.instruction(&I::If(BlockType::Empty));
                body.instruction(&I::Unreachable);
                body.instruction(&I::End);
                let element = resolve_memory_place_for_values(
                    function.id,
                    &function.value_types,
                    layouts,
                    &Place::projected(TEMP, Projection::Element { index: INDEX }, ValueType::Int, AccessMode::Read),
                ).map_err(EncodeError::Lowering)?;
                emit_typed_load(body, function, local, element, destination)?;
            }
            SharedReferenceConstructor::ListIntSlice { .. } => {
                return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                    function: function.id, operation: "shared-place slice carrier",
                }));
            }
            SharedReferenceConstructor::ListIntElementFromSlice { .. } => unreachable!("handled above"),
        }
        emit_release(body, local, function, TEMP, layouts)?;
        emit_release(body, local, function, ENV, layouts)?;
        body.instruction(&I::Br(1));
        body.instruction(&I::End);
    }
    body.instruction(&I::Unreachable);
    body.instruction(&I::End);
    Ok(())
}

fn plan_read_shared_place(
    plan: &mut Vec<PlanToken>,
    function: &mut Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    owner: ValueId,
    bridge: &AdmittedOwnerBridge,
    imports: &BTreeMap<ImportId, u32>,
    cleanup_function: Option<u32>,
) -> Result<(), CanonicalAdmissionError> {
    if !matches!(bridge.family().source, OwnerFamilySource::SharedReference(_)) {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let tag_import = *imports.get(&bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    plan.push(PlanToken::Block("Empty".to_owned()));
    for constructor in &bridge.family().constructors {
        let projection = constructor.shared_reference_constructor().ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let clone_bridge = local.owner_bridges.iter().find(|candidate| {
            candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(constructor.id)
        }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let clone_import = *imports.get(&clone_bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;

        push_get(plan, local, owner)?;
        plan.extend([
            PlanToken::Call(tag_import),
            PlanToken::I64Const(constructor.environment_tag),
            PlanToken::I64Eq,
            PlanToken::If("Empty".to_owned()),
        ]);
        push_get(plan, local, owner)?;
        plan.extend([
            PlanToken::Call(clone_import),
            PlanToken::RefCastNonNull(clone_bridge.payload_type_index()),
            PlanToken::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 },
        ]);
        push_set(plan, local, ENV)?;
        push_share_plan(plan, local, ENV, ENV)?;

        if let SharedReferenceConstructor::ListIntElementFromSlice { slice, index, bounds: SharedPlaceBounds::CurrentLogicalLength } = projection {
            let index_field = shared_place_index(function.id, constructor, index)
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let index_place = Place::projected(ENV, Projection::Field { index: index_field }, ValueType::Size, AccessMode::Read);
            push_memory_load_plan(plan, function, layouts, local, &index_place, INDEX)?;
            push_drop_ref_plan(plan, function, layouts, local, ENV)?;
            let read_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::ReadOwner(constructor.id, slice)
            }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let read_import = *imports.get(&read_bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let ValueType::Gc(nested_type) = function.value_types[&SLICE] else { return Err(CanonicalAdmissionError::UnsupportedPlan) };
            let slice_clone_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.family().owner_type == nested_type
                    && matches!(candidate.recipe(), OwnerBridgeRecipe::CloneEnvironment(_))
            }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let nested_constructor = slice_clone_bridge.family().constructors.iter().find(|candidate| matches!(
                candidate.shared_reference_constructor(), Some(SharedReferenceConstructor::ListIntSlice { .. })
            )).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let clone_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(nested_constructor.id)
            }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let clone_import = *imports.get(&clone_bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            push_get(plan, local, owner)?;
            plan.push(PlanToken::Call(read_import));
            push_set(plan, local, SLICE)?;
            push_get(plan, local, SLICE)?;
            plan.extend([
                PlanToken::Call(clone_import),
                PlanToken::RefCastNonNull(clone_bridge.payload_type_index()),
                PlanToken::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 },
            ]);
            push_set(plan, local, ENV)?;
            push_share_plan(plan, local, ENV, ENV)?;
            let SharedReferenceConstructor::ListIntSlice { root, start, length } =
                nested_constructor.shared_reference_constructor().expect("matched above") else { unreachable!() };
            let root_capture = shared_place_capture(nested_constructor, root).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let (ValueType::Ref(root_layout), CaptureStorage::EnvironmentField(root_field)) =
                (root_capture.value_type, root_capture.storage) else { return Err(CanonicalAdmissionError::UnsupportedPlan) };
            function.value_types.insert(TEMP, ValueType::Ref(root_layout));
            let root_access = field_access(function.id, layouts, ENV, clone_bridge.family().environment, root_field)
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_typed_load_plan(plan, function, local, root_access, TEMP)?;
            push_share_plan(plan, local, TEMP, TEMP)?;
            let length_field = shared_place_index(function.id, nested_constructor, length).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_get(plan, local, INDEX)?;
            let length = field_access(function.id, layouts, ENV, clone_bridge.family().environment, length_field)
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_projected_address_plan(plan, function, local, length)?;
            plan.extend([PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I32LtU, PlanToken::I32Eqz,
                PlanToken::If("Empty".to_owned()), PlanToken::Unreachable, PlanToken::End]);
            let start_field = shared_place_index(function.id, nested_constructor, start).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            let start = field_access(function.id, layouts, ENV, clone_bridge.family().environment, start_field)
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_typed_load_plan(plan, function, local, start, START)?;
            push_get(plan, local, START)?;
            push_get(plan, local, INDEX)?;
            plan.extend([PlanToken::I32Add]);
            push_set(plan, local, INDEX)?;
            push_get(plan, local, INDEX)?;
            push_get(plan, local, START)?;
            plan.extend([PlanToken::I32LtU, PlanToken::If("Empty".to_owned()), PlanToken::Unreachable, PlanToken::End]);
            push_get(plan, local, INDEX)?;
            let current_length = Place::projected(TEMP, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read);
            let current_length = resolve_memory_place_for_values(function.id, &function.value_types, layouts, &current_length)
                .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
            push_projected_address_plan(plan, function, local, current_length)?;
            plan.extend([PlanToken::I32Load { offset: 0, align: 2, memory: 0 }, PlanToken::I32LtU, PlanToken::I32Eqz,
                PlanToken::If("Empty".to_owned()), PlanToken::Unreachable, PlanToken::End]);
            let element = Place::projected(TEMP, Projection::Element { index: INDEX }, ValueType::Int, AccessMode::Read);
            push_memory_load_plan(plan, function, layouts, local, &element, destination)?;
            push_drop_ref_plan(plan, function, layouts, local, TEMP)?;
            push_drop_ref_plan(plan, function, layouts, local, ENV)?;
            let drop_bridge = local.owner_bridges.iter().find(|candidate| {
                candidate.recipe() == &OwnerBridgeRecipe::Drop(clone_bridge.family().signature)
            }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            let drop_import = *imports.get(&drop_bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
            push_get(plan, local, SLICE)?;
            plan.push(PlanToken::Call(drop_import));
            plan.push(PlanToken::Call(cleanup_function.ok_or(CanonicalAdmissionError::UnsupportedPlan)?));
            plan.extend([PlanToken::Br(1), PlanToken::End]);
            continue;
        }

        let (root_field, root_layout) = shared_place_root(function, constructor, projection)
            .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let root = Place::projected(ENV, Projection::Field { index: root_field }, ValueType::Ref(root_layout), AccessMode::Read);
        push_memory_load_plan(plan, function, layouts, local, &root, TEMP)?;
        push_share_plan(plan, local, TEMP, TEMP)?;

        match projection {
            SharedReferenceConstructor::WholeIntCell { .. } => {
                let value = Place::projected(TEMP, Projection::Field { index: 0 }, ValueType::Int, AccessMode::Read);
                push_memory_load_plan(plan, function, layouts, local, &value, destination)?;
            }
            SharedReferenceConstructor::ListIntElement {
                index,
                bounds: SharedPlaceBounds::CurrentLogicalLength,
                ..
            } => {
                let index_field = shared_place_index(function.id, constructor, index)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                let index_place = Place::projected(ENV, Projection::Field { index: index_field }, ValueType::Size, AccessMode::Read);
                push_memory_load_plan(plan, function, layouts, local, &index_place, INDEX)?;
                push_get(plan, local, INDEX)?;
                let length = Place::projected(TEMP, Projection::Header(HeaderField::Length), ValueType::Size, AccessMode::Read);
                let length = resolve_memory_place_for_values(function.id, &function.value_types, layouts, &length)
                    .map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
                push_projected_address_plan(plan, function, local, length)?;
                plan.extend([
                    PlanToken::I32Load { offset: 0, align: 2, memory: 0 },
                    PlanToken::I32LtU,
                    PlanToken::I32Eqz,
                    PlanToken::If("Empty".to_owned()),
                    PlanToken::Unreachable,
                    PlanToken::End,
                ]);
                let element = Place::projected(TEMP, Projection::Element { index: INDEX }, ValueType::Int, AccessMode::Read);
                push_memory_load_plan(plan, function, layouts, local, &element, destination)?;
            }
            SharedReferenceConstructor::ListIntSlice { .. } => {
                return Err(CanonicalAdmissionError::UnsupportedPlan);
            }
            SharedReferenceConstructor::ListIntElementFromSlice { .. } => unreachable!("handled above"),
        }
        push_drop_ref_plan(plan, function, layouts, local, TEMP)?;
        push_drop_ref_plan(plan, function, layouts, local, ENV)?;
        plan.push(PlanToken::Br(1));
        plan.push(PlanToken::End);
    }
    plan.push(PlanToken::Unreachable);
    plan.push(PlanToken::End);
    Ok(())
}

fn emit_read_shared_slice_length(
    body: &mut WasmFunctionEncoder,
    function: &mut Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    owner: ValueId,
    bridge: &AdmittedOwnerBridge,
    imports: &BTreeMap<ImportId, u32>,
) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    if !matches!(bridge.family().source, OwnerFamilySource::SharedReference(_)) {
        return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id, operation: "shared-slice family",
        }));
    }
    let tag_import = *imports.get(&bridge.id()).ok_or(EncodeError::Lowering(
        LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice tag bridge" },
    ))?;
    body.instruction(&I::Block(BlockType::Empty));
    for constructor in &bridge.family().constructors {
        let SharedReferenceConstructor::ListIntSlice { length, .. } = constructor
            .shared_reference_constructor()
            .ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice constructor schema",
            }))?
        else {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "non-slice shared carrier",
            }));
        };
        let clone_bridge = local.owner_bridges.iter().find(|candidate| {
            candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(constructor.id)
        }).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation {
            function: function.id, operation: "shared-slice environment bridge",
        }))?;
        let clone_import = *imports.get(&clone_bridge.id()).ok_or(EncodeError::Lowering(
            LoweringError::UnsupportedOperation { function: function.id, operation: "shared-slice environment import" },
        ))?;
        let capture = shared_place_capture(constructor, length).map_err(EncodeError::Lowering)?;
        let (ValueType::Size, CaptureStorage::EnvironmentField(field)) = (capture.value_type, capture.storage) else {
            return Err(EncodeError::Lowering(LoweringError::UnsupportedOperation {
                function: function.id, operation: "shared-slice length capture",
            }));
        };
        emit_get(body, local, function.id, owner)?;
        body.instruction(&I::Call(tag_import));
        body.instruction(&I::I64Const(constructor.environment_tag));
        body.instruction(&I::I64Eq);
        body.instruction(&I::If(BlockType::Empty));
        emit_get(body, local, function.id, owner)?;
        body.instruction(&I::Call(clone_import));
        body.instruction(&I::RefCastNonNull(wasm_encoder::HeapType::Concrete(clone_bridge.payload_type_index())));
        body.instruction(&I::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 });
        emit_set(body, local, function.id, ENV)?;
        emit_clone_ref(body, local, ENV, ENV, function)?;
        let length = field_access(function.id, layouts, ENV, bridge.family().environment, field)
            .map_err(EncodeError::Lowering)?;
        emit_typed_load(body, function, local, length, destination)?;
        emit_release(body, local, function, ENV, layouts)?;
        body.instruction(&I::Br(1));
        body.instruction(&I::End);
    }
    body.instruction(&I::Unreachable);
    body.instruction(&I::End);
    Ok(())
}

fn plan_read_shared_slice_length(
    plan: &mut Vec<PlanToken>,
    function: &mut Function,
    local: &LocalLayout,
    layouts: &LayoutInterner,
    destination: ValueId,
    owner: ValueId,
    bridge: &AdmittedOwnerBridge,
    imports: &BTreeMap<ImportId, u32>,
) -> Result<(), CanonicalAdmissionError> {
    if !matches!(bridge.family().source, OwnerFamilySource::SharedReference(_)) {
        return Err(CanonicalAdmissionError::UnsupportedPlan);
    }
    let tag_import = *imports.get(&bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    plan.push(PlanToken::Block("Empty".to_owned()));
    for constructor in &bridge.family().constructors {
        let SharedReferenceConstructor::ListIntSlice { length, .. } = constructor
            .shared_reference_constructor().ok_or(CanonicalAdmissionError::UnsupportedPlan)?
        else { return Err(CanonicalAdmissionError::UnsupportedPlan) };
        let clone_bridge = local.owner_bridges.iter().find(|candidate| {
            candidate.recipe() == &OwnerBridgeRecipe::CloneEnvironment(constructor.id)
        }).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let clone_import = *imports.get(&clone_bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
        let capture = shared_place_capture(constructor, length).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
        let (ValueType::Size, CaptureStorage::EnvironmentField(field)) = (capture.value_type, capture.storage) else {
            return Err(CanonicalAdmissionError::UnsupportedPlan);
        };
        push_get(plan, local, owner)?;
        plan.extend([PlanToken::Call(tag_import), PlanToken::I64Const(constructor.environment_tag), PlanToken::I64Eq, PlanToken::If("Empty".to_owned())]);
        push_get(plan, local, owner)?;
        plan.extend([PlanToken::Call(clone_import), PlanToken::RefCastNonNull(clone_bridge.payload_type_index()), PlanToken::StructGet { struct_type_index: clone_bridge.payload_type_index(), field_index: 0 }]);
        push_set(plan, local, ENV)?;
        push_share_plan(plan, local, ENV, ENV)?;
        let length = Place::projected(ENV, Projection::Field { index: field }, ValueType::Size, AccessMode::Read);
        push_memory_load_plan(plan, function, layouts, local, &length, destination)?;
        push_drop_ref_plan(plan, function, layouts, local, ENV)?;
        plan.extend([PlanToken::Br(1), PlanToken::End]);
    }
    plan.extend([PlanToken::Unreachable, PlanToken::End]);
    Ok(())
}

pub(super) fn emit(body: &mut WasmFunctionEncoder, function: &Function, local: &LocalLayout, layouts: &LayoutInterner, op: &O, imports: &BTreeMap<ImportId, u32>, cleanup_function: Option<u32>) -> Result<(), EncodeError> {
    use wasm_encoder::Instruction as I;
    let original = local;
    let (mut function, local, bridge, _payload) = context(function, local, op)?;
    let import = *imports.get(&bridge.id()).ok_or(EncodeError::Lowering(LoweringError::UnsupportedOperation { function: function.id, operation: "owner bridge import" }))?;
    match op {
        O::ReadSharedPlace { destination, owner, .. } => {
            emit_read_shared_place(body, function.to_mut(), &local, layouts, *destination, *owner, bridge, imports, cleanup_function)?;
        }
        O::ReadSharedSliceLength { destination, owner, .. } => {
            emit_read_shared_slice_length(body, function.to_mut(), &local, layouts, *destination, *owner, bridge, imports)?;
        }
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
    park(original, function, local);
    Ok(())
}

pub(super) fn plan(plan: &mut Vec<PlanToken>, function: &Function, layouts: &LayoutInterner, local: &LocalLayout, op: &O, imports: &BTreeMap<ImportId, u32>, cleanup_function: Option<u32>) -> Result<(), CanonicalAdmissionError> {
    let original = local;
    let (mut function, local, bridge, _payload) = context(function, local, op).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let import = *imports.get(&bridge.id()).ok_or(CanonicalAdmissionError::UnsupportedPlan)?;
    match op {
        O::ReadSharedPlace { destination, owner, .. } => {
            plan_read_shared_place(plan, function.to_mut(), &local, layouts, *destination, *owner, bridge, imports, cleanup_function)?;
        }
        O::ReadSharedSliceLength { destination, owner, .. } => {
            plan_read_shared_slice_length(plan, function.to_mut(), &local, layouts, *destination, *owner, bridge, imports)?;
        }
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
    park(original, function, local);
    Ok(())
}
