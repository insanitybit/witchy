//! Private, module-local outlines of repeated memory kernels. These functions
//! are not exports, table entries, imports, or source callables. Their indices
//! follow all source functions and the optional owner cleanup helper.
use super::*;

#[derive(Debug)]
pub(super) struct Helpers {
    pub address: u32,
    pub drops: BTreeMap<crate::layout::LayoutId, u32>,
    pub retain: u32,
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
        Ok(std::sync::Arc::new(Self { address: first, drops, retain }))
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
    for instruction in [
        I::LocalGet(0), I::GlobalGet(2), I::I32LtU, I::If(BlockType::Empty), I::Else,
        I::LocalGet(0), I::I32Const(8), I::I32Sub, I::I32Load(memarg(4)), I::LocalSet(1),
        I::LocalGet(1), I::I32Eqz, I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(1), I::I32Const(-1), I::I32Eq, I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(0), I::I32Const(8), I::I32Sub, I::LocalGet(1), I::I32Const(1), I::I32Add,
        I::I32Store(memarg(4)), I::End, I::LocalGet(0), I::End,
    ] { body.instruction(&instruction); }
    body
}

pub(super) fn plan_retain() -> (Vec<(u32, String)>, Vec<PlanToken>) {
    use PlanToken as P;
    (vec![(1, "i32".into())], vec![
        P::LocalGet(0), P::GlobalGet(2), P::I32LtU, P::If("Empty".into()), P::Else,
        P::LocalGet(0), P::I32Const(8), P::I32Sub,
        P::I32Load { offset: 0, align: 2, memory: 0 }, P::LocalSet(1),
        P::LocalGet(1), P::I32Eqz, P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(1), P::I32Const(-1), P::I32Eq, P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(0), P::I32Const(8), P::I32Sub, P::LocalGet(1), P::I32Const(1), P::I32Add,
        P::I32Store { offset: 0, align: 2, memory: 0 }, P::End, P::LocalGet(0), P::End,
    ])
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;

    #[test]
    fn outlined_address_retains_unsigned_overflow_and_exact_end() {
        // Kernel-only arithmetic test. Production does not export this helper;
        // the full admitted-program and tampered-byte tests cover its binding.
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32; 3], [ValType::I32]);
        let mut functions = FunctionSection::new();
        functions.function(0);
        let mut exports = ExportSection::new();
        exports.export("address", ExportKind::Func, 0);
        let mut code = CodeSection::new();
        code.function(&emit_address());
        let mut module = WasmModule::new();
        module.section(&types).section(&functions).section(&exports).section(&code);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, module.finish()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let address = instance.get_typed_func::<(i32, i32, i32), i32>(&mut store, "address").unwrap();
        let max_for_word = (u32::MAX - 4) as i32;
        assert_eq!(address.call(&mut store, (32, 8, max_for_word)).unwrap(), 40);
        assert_eq!(address.call(&mut store, (max_for_word, 0, max_for_word)).unwrap(), max_for_word);
        assert!(address.call(&mut store, (max_for_word, 1, max_for_word)).is_err());
        assert!(address.call(&mut store, (-1, 1, max_for_word)).is_err());
        assert!(address.call(&mut store, (1, -1, max_for_word)).is_err());
    }
}
