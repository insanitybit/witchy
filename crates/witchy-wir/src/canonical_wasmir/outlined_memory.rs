//! Private, module-local outlines of repeated memory kernels. These functions
//! are not exports, table entries, imports, or source callables. Their indices
//! follow all source functions and the optional owner cleanup helper.
use super::*;

#[derive(Debug)]
pub(super) struct Helpers {
    pub address: u32,
    pub drops: BTreeMap<crate::layout::LayoutId, u32>,
    pub retain: u32,
    pub alloc: u32,
}

impl Helpers {
    pub fn new(source: &CanonicalModule<InitialValid>, first: u32) -> Result<std::sync::Arc<Self>, EncodeError> {
        let mut roots = BTreeSet::new();
        for function in source.functions() {
            for block in &function.blocks {
                for operation in &block.operations {
                    if let Operation::DropRef { object } = operation {
                        let Some(ValueType::Ref(root)) = function.value_types.get(object) else {
                            return Err(EncodeError::UnsupportedLayout { function: function.id, operation: "DropRef object" });
                        };
                        roots.insert(*root);
                    }
                }
            }
        }
        let mut drops = BTreeMap::new();
        for (offset, root) in roots.into_iter().enumerate() {
            let index = u32::try_from(offset).ok()
                .and_then(|offset| first.checked_add(1)?.checked_add(offset))
                .ok_or(EncodeError::UnsupportedLayout { function: FunctionId::new(u32::MAX), operation: "memory helper index" })?;
            drops.insert(root, index);
        }
        let retain = u32::try_from(drops.len()).ok()
            .and_then(|count| first.checked_add(1)?.checked_add(count))
            .ok_or(EncodeError::UnsupportedLayout { function: FunctionId::new(u32::MAX), operation: "memory helper index" })?;
        let alloc = retain.checked_add(1)
            .ok_or(EncodeError::UnsupportedLayout { function: FunctionId::new(u32::MAX), operation: "memory helper index" })?;
        Ok(std::sync::Arc::new(Self { address: first, drops, retain, alloc }))
    }
}

// The third argument is the already checked u32::MAX - access.width. Keeping
// the sum in i64 preserves the old trap even when base + offset wraps an i32.
pub(super) fn emit_address() -> WasmFunctionEncoder {
    use wasm_encoder::Instruction as I;
    let mut body = WasmFunctionEncoder::new([]);
    for instruction in [
        I::LocalGet(0), I::I64ExtendI32U, I::LocalGet(1), I::I64ExtendI32U,
        I::I64Add, I::LocalGet(2), I::I64ExtendI32U, I::I64GtU,
        I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(0), I::LocalGet(1), I::I32Add, I::End,
    ] { body.instruction(&instruction); }
    body
}

// Independent expected mapping, not decoded encoder output. Both mappings
// retain the exact unsigned address predicate, including its failure path.
pub(super) fn plan_address() -> (Vec<(u32, String)>, Vec<PlanToken>) {
    use PlanToken as P;
    (Vec::new(), vec![
        P::LocalGet(0), P::I64ExtendI32U, P::LocalGet(1), P::I64ExtendI32U,
        P::I64Add, P::LocalGet(2), P::I64ExtendI32U, P::I64GtU,
        P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(0), P::LocalGet(1), P::I32Add, P::End,
    ])
}

fn drop_locals() -> LocalLayout<'static> {
    // The object argument is scratch slot zero. Destruction still uses the
    // existing iterative worklist, not recursive Wasm calls. Helpers cannot
    // call user code or reenter the instance while that worklist is live.
    LocalLayout {
        memory_helpers: None,
        owner_scratch: None,
        owner_bridges: Vec::new(),
        indices: BTreeMap::new(),
        extra_types: vec![ValType::I32; drop_worklist::SCRATCH - 1],
        scratch: (0..drop_worklist::SCRATCH as u32).collect(),
        owner_context: Default::default(),
    }
}

pub(super) fn emit_drop(layouts: &LayoutInterner, root: crate::layout::LayoutId) -> Result<WasmFunctionEncoder, EncodeError> {
    let mut body = WasmFunctionEncoder::new([(drop_worklist::SCRATCH as u32 - 1, ValType::I32)]);
    drop_worklist::emit(&mut body, &drop_locals(), FunctionId::new(u32::MAX), layouts, root)?;
    body.instruction(&wasm_encoder::Instruction::End);
    Ok(body)
}

pub(super) fn plan_drop(layouts: &LayoutInterner, root: crate::layout::LayoutId) -> Result<(Vec<(u32, String)>, Vec<PlanToken>), CanonicalAdmissionError> {
    let mut plan = Vec::new();
    drop_worklist::plan(&mut plan, FunctionId::new(u32::MAX), layouts, &drop_locals(), root)?;
    plan.push(PlanToken::End);
    Ok((vec![(drop_worklist::SCRATCH as u32 - 1, "i32".into())], plan))
}

// Retain is also used implicitly by Load(CloneShared/ClonePresent) and
// Store(CloneShared). Keep presence checks and publication at their original
// call sites; this kernel only performs the existing RC transition.
pub(super) fn emit_retain() -> WasmFunctionEncoder {
    use wasm_encoder::Instruction as I;
    let mut body = WasmFunctionEncoder::new([(1, ValType::I32)]);
    memory_op_counters::emit_bump(&mut body, memory_op_counters::Counter::Retains);
    for instruction in [
        I::LocalGet(0), I::GlobalGet(2), I::I32LtU, I::If(BlockType::Empty), I::Else,
        I::LocalGet(0), I::I32Const(8), I::I32Sub, I::I64ExtendI32U, I::I32Load(memarg(4)), I::LocalSet(1),
        I::LocalGet(1), I::I32Eqz, I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(1), I::I32Const(-1), I::I32Eq, I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(0), I::I32Const(8), I::I32Sub, I::I64ExtendI32U, I::LocalGet(1), I::I32Const(1), I::I32Add,
        I::I32Store(memarg(4)), I::End, I::LocalGet(0), I::End,
    ] { body.instruction(&instruction); }
    body
}

pub(super) fn plan_retain() -> (Vec<(u32, String)>, Vec<PlanToken>) {
    use PlanToken as P;
    let mut plan = Vec::new();
    memory_op_counters::plan_bump(&mut plan, memory_op_counters::Counter::Retains);
    plan.extend([
        P::LocalGet(0), P::GlobalGet(2), P::I32LtU, P::If("Empty".into()), P::Else,
        P::LocalGet(0), P::I32Const(8), P::I32Sub,
        P::I64ExtendI32U, P::I32Load { offset: 0, align: 2, memory: 0 }, P::LocalSet(1),
        P::LocalGet(1), P::I32Eqz, P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(1), P::I32Const(-1), P::I32Eq, P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(0), P::I32Const(8), P::I32Sub, P::I64ExtendI32U, P::LocalGet(1), P::I32Const(1), P::I32Add,
        P::I32Store { offset: 0, align: 2, memory: 0 }, P::End, P::LocalGet(0), P::End,
    ]);
    (vec![(1, "i32".into())], plan)
}

// Allocation of `size` bytes (the parameter) returning the object address,
// including memory growth. Allocation sites inline everything but growth.
pub(super) fn emit_alloc() -> WasmFunctionEncoder {
    let mut body = WasmFunctionEncoder::new([(3, ValType::I32)]);
    emit_alloc_core(&mut body, [0, 1, 2, 3], None);
    body.instruction(&wasm_encoder::Instruction::LocalGet(2));
    body.instruction(&wasm_encoder::Instruction::End);
    body
}

pub(super) fn plan_alloc() -> (Vec<(u32, String)>, Vec<PlanToken>) {
    let mut plan = Vec::new();
    push_alloc_core_plan(&mut plan, [0, 1, 2, 3], None);
    plan.extend([PlanToken::LocalGet(2), PlanToken::End]);
    (vec![(3, "i32".into())], plan)
}
