//! WIR to wasm **binary** encoder: turns a [`crate::wir::WirModule`] directly
//! into a WebAssembly binary via the `wasm-encoder` crate. Its output matches the
//! WAT text path (`crate::wir::to_wat`) instruction-for-instruction;
//! `print_node`/`print_expr` there are the authoritative semantics this mirrors.
//!
//! Index spaces: imported functions come first (index = position in
//! `module.imports`), then defined functions (index = `imports.len()` + position
//! in `module.funcs`). Locals are params-then-body in declaration order.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

use wasm_encoder::{
    AbstractHeapType, ArrayType, BlockType, CodeSection, CompositeInnerType, CompositeType,
    ConstExpr, DataSection, ElementSection, Elements, EntityType, ExportKind, ExportSection,
    FieldType, Function, FunctionSection, GlobalSection, GlobalType, HeapType, ImportSection,
    Instruction, MemArg, MemorySection, MemoryType, Module, NameMap, NameSection, RefType,
    StorageType, StructType, SubType, TableSection, TableType, TypeSection, ValType,
};

use crate::wir::{
    BinOp, ClosureSignature, GlobalInit, Kind, UnOp, WirExpr, WirModule, WirNode, WirSeq,
    WirArrayDef, WirStructDef, slot_closure_signature,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError {
    pub message: String,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid WIR: {}", self.message)
    }
}

impl std::error::Error for EncodeError {}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else {
        "encoder invariant failed without a string diagnostic".to_string()
    }
}

struct Preflight<'a> {
    funcs: HashSet<&'a str>,
    imports: HashSet<&'a str>,
    globals: HashSet<&'a str>,
}

impl Preflight<'_> {
    fn reject(message: impl Into<String>) -> Result<(), EncodeError> {
        Err(EncodeError { message: message.into() })
    }

    fn expr(
        &self,
        expr: &WirExpr,
        locals: &HashSet<&str>,
        labels: &mut Vec<String>,
    ) -> Result<(), EncodeError> {
        match expr {
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize => {}
            WirExpr::GetLocal(name) => {
                if !locals.contains(name.as_str()) {
                    return Self::reject(format!("unknown local ${name}"));
                }
            }
            WirExpr::GetGlobal(name) => {
                if !self.globals.contains(name.as_str()) {
                    return Self::reject(format!("unknown global ${name}"));
                }
            }
            WirExpr::ToSlot(value, kind) | WirExpr::FromSlot(value, kind) => {
                if kind.is_ref() {
                    return Self::reject(format!(
                        "reference kind {kind:?} cannot cross the i64 slot boundary"
                    ));
                }
                self.expr(value, locals, labels)?;
            }
            WirExpr::Binary { op, kind, lhs, rhs } => {
                if !binop_supported(*op, *kind) {
                    return Self::reject(format!(
                        "no wasm instruction for {op:?} on {kind:?}"
                    ));
                }
                self.expr(lhs, locals, labels)?;
                self.expr(rhs, locals, labels)?;
            }
            WirExpr::Unary { op, kind, arg } => {
                let valid = match op {
                    UnOp::Neg => matches!(kind, Kind::I32 | Kind::I64 | Kind::F64),
                    UnOp::BitNot => matches!(kind, Kind::I32 | Kind::I64),
                    UnOp::Not => *kind == Kind::I32,
                    UnOp::ToFloat | UnOp::Sqrt => *kind == Kind::F64,
                    UnOp::ToInt => *kind == Kind::I64,
                };
                if !valid {
                    return Self::reject(format!(
                        "no wasm instruction for unary {op:?} on {kind:?}"
                    ));
                }
                self.expr(arg, locals, labels)?;
            }
            WirExpr::Convert { arg, .. }
            | WirExpr::MemoryGrow(arg)
            | WirExpr::ArrayLen(arg)
            | WirExpr::RefIsNull(arg) => self.expr(arg, locals, labels)?,
            WirExpr::Load { ptr, kind, .. } => {
                if kind.is_ref() {
                    return Self::reject(format!(
                        "reference kind {kind:?} cannot be loaded from linear memory"
                    ));
                }
                self.expr(ptr, locals, labels)?;
            }
            WirExpr::Load8U { ptr, .. } => self.expr(ptr, locals, labels)?,
            WirExpr::Call { func, args } => {
                if !self.funcs.contains(func.as_str()) {
                    return Self::reject(format!("call to unknown func ${func}"));
                }
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
            }
            WirExpr::CallHost { import, args } => {
                if !self.imports.contains(import.as_str()) {
                    return Self::reject(format!("call to unknown host import ${import}"));
                }
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
            }
            WirExpr::CallIndirect { signature, args, index } => {
                if signature.params.len() != args.len() {
                    return Self::reject(format!(
                        "indirect call has {} arguments but its signature has {} parameters",
                        args.len(),
                        signature.params.len()
                    ));
                }
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
                self.expr(index, locals, labels)?;
            }
            WirExpr::Control(node) => self.node(node, locals, labels)?,
            WirExpr::Seq(seq) => self.seq(seq, locals, labels)?,
            WirExpr::StructNew { args, .. } => {
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
            }
            WirExpr::StructGet { base, .. } | WirExpr::RefCast { value: base, .. } => {
                self.expr(base, locals, labels)?;
            }
            WirExpr::ArrayNew { value, len, .. } => {
                self.expr(value, locals, labels)?;
                self.expr(len, locals, labels)?;
            }
            WirExpr::ArrayNewFixed { items, .. } => {
                for item in items {
                    self.expr(item, locals, labels)?;
                }
            }
            WirExpr::ArrayGet { array, index, .. } => {
                self.expr(array, locals, labels)?;
                self.expr(index, locals, labels)?;
            }
            WirExpr::RefNull(kind) => {
                if !kind.is_ref() {
                    return Self::reject(format!("RefNull requires a reference kind, got {kind:?}"));
                }
            }
        }
        Ok(())
    }

    fn seq(
        &self,
        seq: &WirSeq,
        locals: &HashSet<&str>,
        labels: &mut Vec<String>,
    ) -> Result<(), EncodeError> {
        for node in seq {
            self.node(node, locals, labels)?;
        }
        Ok(())
    }

    fn node(
        &self,
        node: &WirNode,
        locals: &HashSet<&str>,
        labels: &mut Vec<String>,
    ) -> Result<(), EncodeError> {
        match node {
            WirNode::SetLocal { local, value } => {
                if !locals.contains(local.as_str()) {
                    return Self::reject(format!("unknown local ${local}"));
                }
                self.expr(value, locals, labels)?;
            }
            WirNode::SetGlobal { global, value } => {
                if !self.globals.contains(global.as_str()) {
                    return Self::reject(format!("unknown global ${global}"));
                }
                self.expr(value, locals, labels)?;
            }
            WirNode::Store { ptr, value, kind, .. } => {
                if kind.is_ref() {
                    return Self::reject(format!(
                        "reference kind {kind:?} cannot be stored to linear memory"
                    ));
                }
                self.expr(ptr, locals, labels)?;
                self.expr(value, locals, labels)?;
            }
            WirNode::CallStoreMulti { func, args, dests } => {
                if !self.funcs.contains(func.as_str()) {
                    return Self::reject(format!("call to unknown func ${func}"));
                }
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
                for dest in dests {
                    if !locals.contains(dest.as_str()) {
                        return Self::reject(format!("unknown local ${dest}"));
                    }
                }
            }
            WirNode::CallIndirectStoreMulti { signature, args, index, dests } => {
                if signature.params.len() != args.len() {
                    return Self::reject(format!(
                        "indirect call has {} arguments but its signature has {} parameters",
                        args.len(),
                        signature.params.len()
                    ));
                }
                if signature.results.len() != dests.len() {
                    return Self::reject(format!(
                        "indirect call has {} destinations but its signature has {} results",
                        dests.len(),
                        signature.results.len()
                    ));
                }
                for arg in args {
                    self.expr(arg, locals, labels)?;
                }
                self.expr(index, locals, labels)?;
                for dest in dests {
                    if !locals.contains(dest.as_str()) {
                        return Self::reject(format!("unknown local ${dest}"));
                    }
                }
            }
            WirNode::MemoryCopy { dest, src, len } => {
                self.expr(dest, locals, labels)?;
                self.expr(src, locals, labels)?;
                self.expr(len, locals, labels)?;
            }
            WirNode::MemoryFill { dest, value, len } => {
                self.expr(dest, locals, labels)?;
                self.expr(value, locals, labels)?;
                self.expr(len, locals, labels)?;
            }
            WirNode::Store8 { ptr, value, .. } => {
                self.expr(ptr, locals, labels)?;
                self.expr(value, locals, labels)?;
            }
            WirNode::If { cond, then_, els, .. } => {
                self.expr(cond, locals, labels)?;
                self.seq(then_, locals, labels)?;
                self.seq(els, locals, labels)?;
            }
            WirNode::Block { label, body, .. } | WirNode::Loop { label, body } => {
                labels.push(label.clone());
                self.seq(body, locals, labels)?;
                labels.pop();
            }
            WirNode::Br { target, cond } => {
                if !labels.contains(target) {
                    return Self::reject(format!(
                        "br target ${target} has no enclosing Block/Loop frame"
                    ));
                }
                if let Some(cond) = cond {
                    self.expr(cond, locals, labels)?;
                }
            }
            WirNode::Drop(expr)
            | WirNode::Do(expr)
            | WirNode::Push(expr)
            | WirNode::Return(Some(expr)) => self.expr(expr, locals, labels)?,
            WirNode::Return(None) | WirNode::Unreachable => {}
            WirNode::StructSet { base, value, .. } => {
                self.expr(base, locals, labels)?;
                self.expr(value, locals, labels)?;
            }
            WirNode::ArraySet { array, index, value, .. } => {
                self.expr(array, locals, labels)?;
                self.expr(index, locals, labels)?;
                self.expr(value, locals, labels)?;
            }
        }
        Ok(())
    }
}

fn binop_supported(op: BinOp, kind: Kind) -> bool {
    match op {
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Eq
        | BinOp::Ne
        | BinOp::Lt
        | BinOp::Le
        | BinOp::Gt
        | BinOp::Ge => matches!(kind, Kind::I32 | Kind::I64 | Kind::F64),
        BinOp::Rem | BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::Shr => {
            matches!(kind, Kind::I32 | Kind::I64)
        }
        BinOp::DivU
        | BinOp::RemU
        | BinOp::ShrU
        | BinOp::LtU
        | BinOp::LeU
        | BinOp::GtU
        | BinOp::GeU => matches!(kind, Kind::I32 | Kind::I64),
    }
}

fn preflight(module: &WirModule) -> Result<(), EncodeError> {
    let mut funcs = HashSet::new();
    for func in &module.funcs {
        if !funcs.insert(func.name.as_str()) {
            return Preflight::reject(format!("duplicate function name ${}", func.name));
        }
    }
    let mut imports = HashSet::new();
    for import in &module.imports {
        if !imports.insert(import.name.as_str()) {
            return Preflight::reject(format!("duplicate host import name ${}", import.name));
        }
    }
    let mut globals = HashSet::new();
    for global in &module.globals {
        if !globals.insert(global.name.as_str()) {
            return Preflight::reject(format!("duplicate global name ${}", global.name));
        }
    }
    let context = Preflight { funcs, imports, globals };

    for (_, func) in &module.exports {
        if !context.funcs.contains(func.as_str()) {
            return Preflight::reject(format!("export references unknown func ${func}"));
        }
    }
    if let Some(table) = &module.table {
        for func in &table.funcs {
            if !context.funcs.contains(func.as_str()) {
                return Preflight::reject(format!("elem references unknown func ${func}"));
            }
        }
    }
    for func in &module.funcs {
        if func.raw_body.is_some() {
            continue;
        }
        let mut locals = HashSet::new();
        for local in func.params.iter().chain(&func.locals) {
            if !locals.insert(local.name.as_str()) {
                return Preflight::reject(format!(
                    "duplicate local name ${} in function ${}",
                    local.name, func.name
                ));
            }
        }
        context.seq(&func.body, &locals, &mut Vec::new())?;
    }
    Ok(())
}

/// Map a WIR `Kind` to a wasm-encoder `ValType`. `gc_base` is the type-section
/// index where concrete GC definitions begin, so `Kind::GcRef(i)` resolves to
/// type `gc_base + i` (structs first, then arrays).
fn val_type(kind: Kind, gc_base: u32) -> ValType {
    match kind {
        Kind::I32 => ValType::I32,
        Kind::I64 => ValType::I64,
        Kind::F64 => ValType::F64,
        // (RFC-0005) An unforgeable, nullable host reference.
        Kind::ExternRef => ValType::EXTERNREF,
        // (RFC-0005 Stage 4) An erased nullable GC struct reference.
        Kind::StructRef => {
            ValType::Ref(RefType::new_abstract(AbstractHeapType::Struct, true, false))
        }
        // (RFC-0005) A nullable reference to the concrete GC struct type.
        Kind::GcRef(id) => ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(gc_base + id),
        }),
    }
}

/// Natural alignment exponent (log2 of byte size) for a `<kind>.load`.
fn load_align(kind: Kind) -> u32 {
    match kind {
        Kind::I32 => 2, // 4 bytes
        Kind::I64 => 3, // 8 bytes
        Kind::F64 => 3, // 8 bytes
        // (RFC-0005) Reference kinds are never a linear-memory load/store.
        Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
            unreachable!("reference-typed values are not linear-memory loads")
        }
    }
}

/// Encode a [`WirModule`] into a wasm binary. `structs` are the module's
/// cap-carrying GC struct definitions (RFC-0005); they are laid after the
/// reserved scalar closure-signature band and before other function signatures.
/// Pass `&[]` when the module lowers no cap-carrying aggregates.
pub fn encode(module: &WirModule, structs: &[WirStructDef]) -> Vec<u8> {
    encode_with_gc(module, structs, &[])
}

/// Fallible production boundary around the invariant-heavy encoder.
///
/// WIR builders use named locals, functions, globals, and labels. A compiler
/// defect can leave one of those references unresolved or attempt an illegal
/// reference/slot crossing. The historical `encode` API treats those as
/// internal invariant panics. The structural preflight mirrors every such
/// panic precondition so it also works on aborting wasm targets; `catch_unwind`
/// is a final native containment net for an unforeseen encoder defect.
pub fn try_encode(
    module: &WirModule,
    structs: &[WirStructDef],
) -> Result<Vec<u8>, EncodeError> {
    try_encode_with_gc(module, structs, &[])
}

/// Fallible production boundary for modules that declare both GC structs and
/// GC arrays. This is the checked counterpart to [`encode_with_gc`]; keeping
/// [`try_encode`] as the struct-only compatibility entrypoint lets existing
/// callers remain source-neutral while reference-bearing collection lowering
/// is wired into production.
pub fn try_encode_with_gc(
    module: &WirModule,
    structs: &[WirStructDef],
    arrays: &[WirArrayDef],
) -> Result<Vec<u8>, EncodeError> {
    preflight(module)?;
    catch_unwind(AssertUnwindSafe(|| encode_with_gc(module, structs, arrays)))
        .map_err(|payload| EncodeError { message: panic_message(payload) })
}

/// Encode a module with both GC struct and GC array declarations. Concrete
/// [`Kind::GcRef`] indices name structs first, then arrays. Keeping `encode` as
/// the struct-only compatibility entrypoint leaves the production lowering
/// source-neutral until reference-bearing collections are ready to consume the
/// array substrate.
pub fn encode_with_gc(
    module: &WirModule,
    structs: &[WirStructDef],
    arrays: &[WirArrayDef],
) -> Vec<u8> {
    // --- Type section: collect unique (params, results) signatures ---------
    // Imports carry their param/result `Kind`s directly. Funcs derive params
    // from `params[*].ty.kind()` and results from `ret[*].kind()`. Dedup uses a
    // small linear scan because the signature set is tiny.
    let mut sigs: Vec<(Vec<Kind>, Vec<Kind>)> = Vec::new();
    let mut intern = |params: Vec<Kind>, results: Vec<Kind>| -> u32 {
        if let Some(idx) = sigs.iter().position(|(p, r)| *p == params && *r == results) {
            idx as u32
        } else {
            let idx = sigs.len() as u32;
            sigs.push((params, results));
            idx
        }
    };

    // The legacy scalar closure signatures are interned first, preserving the
    // stable `$clos0..=MAX_CLOS` type band. Exact typed signatures are interned
    // below and may name GC structs.
    let mut clos_type_idx: HashMap<ClosureSignature, u32> = HashMap::new();
    for n in 0..=crate::wir::MAX_CLOS {
        let signature = slot_closure_signature(n, 1);
        let idx = intern(signature.params.clone(), signature.results.clone());
        clos_type_idx.insert(signature, idx);
    }

    // Type index for each import (in order).
    let import_type_idx: Vec<u32> = module
        .imports
        .iter()
        .map(|imp| intern(imp.params.clone(), imp.results.clone()))
        .collect();

    // Type index for each defined func (in order).
    let func_type_idx: Vec<u32> = module
        .funcs
        .iter()
        .map(|f| {
            let params: Vec<Kind> = f.params.iter().map(|l| l.ty.kind()).collect();
            let results: Vec<Kind> = f.ret.iter().map(|t| t.kind()).collect();
            intern(params, results)
        })
        .collect();

    // Every exact closure signature referenced by a visible `CallIndirect`
    // interns after the import/func types unless it matches the scalar band.
    let mut clos_signatures: Vec<ClosureSignature> = Vec::new();
    for f in &module.funcs {
        collect_clos_signatures(&f.body, &mut clos_signatures);
    }
    for signature in clos_signatures {
        clos_type_idx.entry(signature.clone()).or_insert_with(|| {
            intern(signature.params.clone(), signature.results.clone())
        });
    }

    // (RFC-0005) GC struct types sit immediately AFTER the reserved `$clos{N}`
    // band (type indices `0..=MAX_CLOS`) and BEFORE every other function signature.
    // Reason: a function type that takes a `GcRef` param references its struct by
    // type index, and GC recursion-group scoping forbids a *forward* reference
    // across singleton type defs — so the struct must precede any function type
    // that names it. `gc_base` is that first post-clos index; the non-clos
    // function signatures are shifted up by every concrete GC definition.
    // Reserved scalar indices stay put and never take a `GcRef`.
    let clos_band = crate::wir::MAX_CLOS as u32 + 1;
    let gc_shift = (structs.len() + arrays.len()) as u32;
    let gc_base = clos_band;
    let array_base = gc_base + structs.len() as u32;
    // sig-position -> emitted type-section index.
    let type_idx = |pos: u32| -> u32 {
        if pos < clos_band { pos } else { pos + gc_shift }
    };
    let split = (clos_band as usize).min(sigs.len());

    let mut type_section = TypeSection::new();
    // 1) The reserved clos band (indices `0..split`).
    for (params, results) in &sigs[..split] {
        type_section.ty().function(
            params.iter().map(|k| val_type(*k, gc_base)),
            results.iter().map(|k| val_type(*k, gc_base)),
        );
    }
    // 2) Concrete GC definitions form one recursion group. This makes every
    // `GcRef` edge in the combined struct/array band legal, including forward
    // references and cycles. Arrays are mutable for aggregate updates; each
    // struct declares whether its fields remain mutable after construction.
    let gc_types: Vec<SubType> = structs
        .iter()
        .map(|def| {
            let fields = def
                .fields
                .iter()
                .map(|kind| FieldType {
                    element_type: StorageType::Val(val_type(*kind, gc_base)),
                    mutable: def.mutable,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            SubType {
                is_final: true,
                supertype_idx: None,
                composite_type: CompositeType {
                    inner: CompositeInnerType::Struct(StructType { fields }),
                    shared: false,
                    descriptor: None,
                    describes: None,
                },
            }
        })
        .chain(arrays.iter().map(|def| SubType {
            is_final: true,
            supertype_idx: None,
            composite_type: CompositeType {
                inner: CompositeInnerType::Array(ArrayType(FieldType {
                    element_type: StorageType::Val(val_type(def.element, gc_base)),
                    mutable: true,
                })),
                shared: false,
                descriptor: None,
                describes: None,
            },
        }))
        .collect();
    if !gc_types.is_empty() {
        type_section.ty().rec(gc_types);
    }
    // 3) The remaining function signatures, shifted past all GC types.
    for (params, results) in &sigs[split..] {
        type_section.ty().function(
            params.iter().map(|k| val_type(*k, gc_base)),
            results.iter().map(|k| val_type(*k, gc_base)),
        );
    }

    // --- Function index maps -----------------------------------------------
    // Imports first, then defined funcs.
    let mut import_index: HashMap<&str, u32> = HashMap::new();
    for (i, imp) in module.imports.iter().enumerate() {
        import_index.insert(imp.name.as_str(), i as u32);
    }
    let mut func_index: HashMap<&str, u32> = HashMap::new();
    let import_count = module.imports.len() as u32;
    for (i, f) in module.funcs.iter().enumerate() {
        func_index.insert(f.name.as_str(), import_count + i as u32);
    }

    // --- Global index map (in declaration order) ----------------------------
    let mut global_index: HashMap<&str, u32> = HashMap::new();
    for (i, g) in module.globals.iter().enumerate() {
        global_index.insert(g.name.as_str(), i as u32);
    }

    // --- Import section -----------------------------------------------------
    let mut import_section = ImportSection::new();
    for (imp, &ty_idx) in module.imports.iter().zip(&import_type_idx) {
        import_section.import("witchy", &imp.name, EntityType::Function(type_idx(ty_idx)));
    }

    // --- Function section (declares each defined func's type) ---------------
    let mut function_section = FunctionSection::new();
    for &ty_idx in &func_type_idx {
        function_section.function(type_idx(ty_idx));
    }

    // --- Memory section -----------------------------------------------------
    let mut memory_section = MemorySection::new();
    memory_section.memory(MemoryType {
        minimum: module.memory_pages as u64,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    // --- Table section ------------------------------------------------------
    // table 0 holds `funcref`s; sized to the element-segment length (codegen's
    // `(table N funcref)`). Present iff the module declares a table.
    let mut table_section = TableSection::new();
    if let Some(table) = &module.table {
        table_section.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table.funcs.len() as u64,
            maximum: None,
            shared: false,
        });
    }

    // --- Global section -----------------------------------------------------
    let mut global_section = GlobalSection::new();
    for g in &module.globals {
        let init = match g.init {
            GlobalInit::I32(n) => ConstExpr::i32_const(n),
            GlobalInit::I64(n) => ConstExpr::i64_const(n),
        };
        global_section.global(
            GlobalType { val_type: val_type(g.kind, gc_base), mutable: g.mutable, shared: false },
            &init,
        );
    }

    // --- Export section -----------------------------------------------------
    // The `to_wat` printer always exports the single memory as "memory"; mirror
    // that, then exported globals (inline `(export "…")` on a `WirGlobal`), then
    // the explicit function exports.
    let mut export_section = ExportSection::new();
    export_section.export("memory", ExportKind::Memory, 0);
    for (i, g) in module.globals.iter().enumerate() {
        if let Some(name) = &g.export {
            export_section.export(name, ExportKind::Global, i as u32);
        }
    }
    for (export, func) in &module.exports {
        let idx = *func_index
            .get(func.as_str())
            .unwrap_or_else(|| panic!("export references unknown func ${func}"));
        export_section.export(export, ExportKind::Func, idx);
    }

    // --- Element section ----------------------------------------------------
    // Element segment 0 places the table's funcs at offset 0 (codegen's
    // `(elem (i32.const 0) $f0 $f1 …)`). Empty when no lambdas (the table can be
    // declared 0-sized just so a `call_indirect` references table 0).
    let mut element_section = ElementSection::new();
    let mut has_elements = false;
    if let Some(table) = &module.table {
        if !table.funcs.is_empty() {
            let func_refs: Vec<u32> = table
                .funcs
                .iter()
                .map(|name| {
                    *func_index
                        .get(name.as_str())
                        .unwrap_or_else(|| panic!("elem references unknown func ${name}"))
                })
                .collect();
            element_section.active(
                None, // MVP encoding: table 0, funcref
                &ConstExpr::i32_const(0),
                Elements::Functions(func_refs.into()),
            );
            has_elements = true;
        }
    }

    // --- Code section -------------------------------------------------------
    let mut code_section = CodeSection::new();
    for f in &module.funcs {
        // Raw-body splice: a pre-compiled function body (locals + instructions +
        // the trailing `End`, exactly as `Function::raw`/`CodeSection::raw`
        // expect). The func still contributed its type + name→index above; here
        // we copy the bytes verbatim instead of walking nodes. `CodeSection::raw`
        // prepends the length prefix, so the bytes must NOT include it.
        if let Some(raw) = &f.raw_body {
            code_section.raw(raw);
            continue;
        }

        // Local index map: params first (declaration order), then body locals.
        let mut local_index: HashMap<&str, u32> = HashMap::new();
        let mut next = 0u32;
        for p in &f.params {
            local_index.insert(p.name.as_str(), next);
            next += 1;
        }
        // Body locals as a flat list of ValTypes (one entry per local).
        let mut body_local_types: Vec<ValType> = Vec::new();
        for l in &f.locals {
            local_index.insert(l.name.as_str(), next);
            next += 1;
            body_local_types.push(val_type(l.ty.kind(), gc_base));
        }

        let mut function = Function::new_with_locals_types(body_local_types);
        let mut ctx = EncodeCtx {
            local_index: &local_index,
            func_index: &func_index,
            import_index: &import_index,
            global_index: &global_index,
            clos_type_idx: &clos_type_idx,
            gc_base,
            array_base,
            gc_shift,
            label_stack: Vec::new(),
        };
        ctx.encode_seq(&mut function, &f.body);
        function.instruction(&Instruction::End);
        code_section.function(&function);
    }

    // --- Data section -------------------------------------------------------
    let mut data_section = DataSection::new();
    for seg in &module.data {
        data_section.active(0, &ConstExpr::i32_const(seg.offset as i32), seg.bytes.iter().copied());
    }

    // --- Assemble in canonical section order --------------------------------
    // Type, Import, Function, Table, Memory, Global, Export, Element, Code, Data
    // (the wasm-mandated section ordering).
    let mut wasm = Module::new();
    wasm.section(&type_section);
    if !module.imports.is_empty() {
        wasm.section(&import_section);
    }
    wasm.section(&function_section);
    if module.table.is_some() {
        wasm.section(&table_section);
    }
    wasm.section(&memory_section);
    if !module.globals.is_empty() {
        wasm.section(&global_section);
    }
    wasm.section(&export_section);
    if has_elements {
        wasm.section(&element_section);
    }
    wasm.section(&code_section);
    if !module.data.is_empty() {
        wasm.section(&data_section);
    }

    // --- Name section (custom) ----------------------------------------------
    // Map every function index → its witchy name so wasmtime traps and
    // backtraces name the offending function instead of `wasm-function[N]`.
    // Pure metadata: wasmtime ignores it for execution, so it changes neither
    // behavior nor the heap layout (parity-safe).
    let mut func_names = NameMap::new();
    for (i, imp) in module.imports.iter().enumerate() {
        func_names.append(i as u32, &imp.name);
    }
    for (i, f) in module.funcs.iter().enumerate() {
        func_names.append(import_count + i as u32, &f.name);
    }
    let mut name_section = NameSection::new();
    name_section.functions(&func_names);
    wasm.section(&name_section);

    wasm.finish()
}

/// Collect the distinct closure signatures referenced by indirect calls for
/// type-section synthesis. Walks both nodes and their nested expressions.
fn collect_clos_signatures(seq: &WirSeq, out: &mut Vec<ClosureSignature>) {
    fn push(out: &mut Vec<ClosureSignature>, signature: &ClosureSignature) {
        if !out.contains(signature) {
            out.push(signature.clone());
        }
    }
    fn walk_expr(e: &WirExpr, out: &mut Vec<ClosureSignature>) {
        match e {
            WirExpr::CallIndirect {
                signature,
                args,
                index,
            } => {
                push(out, signature);
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
            }
            WirExpr::ToSlot(inner, _)
            | WirExpr::FromSlot(inner, _)
            | WirExpr::Unary { arg: inner, .. }
            | WirExpr::Convert { arg: inner, .. } => walk_expr(inner, out),
            WirExpr::Binary { lhs, rhs, .. } => {
                walk_expr(lhs, out);
                walk_expr(rhs, out);
            }
            WirExpr::Load { ptr, .. } | WirExpr::Load8U { ptr, .. } => walk_expr(ptr, out),
            WirExpr::MemoryGrow(pages) => walk_expr(pages, out),
            WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::Control(node) => walk_node(node, out),
            WirExpr::Seq(nodes) => collect_clos_signatures(nodes, out),
            WirExpr::StructNew { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::StructGet { base, .. }
            | WirExpr::RefCast { value: base, .. }
            | WirExpr::ArrayLen(base)
            | WirExpr::RefIsNull(base) => walk_expr(base, out),
            WirExpr::ArrayNew { value, len, .. } => {
                walk_expr(value, out);
                walk_expr(len, out);
            }
            WirExpr::ArrayNewFixed { items, .. } => {
                for item in items {
                    walk_expr(item, out);
                }
            }
            WirExpr::ArrayGet { array, index, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
            }
            // Leaves: no nested closure-arity references.
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize
            | WirExpr::GetLocal(_)
            | WirExpr::GetGlobal(_)
            | WirExpr::RefNull(_) => {}
        }
    }
    fn walk_node(node: &WirNode, out: &mut Vec<ClosureSignature>) {
        match node {
            WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
                walk_expr(value, out)
            }
            WirNode::StructSet { base, value, .. } => {
                walk_expr(base, out);
                walk_expr(value, out);
            }
            WirNode::ArraySet { array, index, value, .. } => {
                walk_expr(array, out);
                walk_expr(index, out);
                walk_expr(value, out);
            }
            WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
                walk_expr(ptr, out);
                walk_expr(value, out);
            }
            WirNode::CallStoreMulti { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirNode::CallIndirectStoreMulti {
                signature,
                args,
                index,
                dests: _,
            } => {
                push(out, signature);
                for a in args {
                    walk_expr(a, out);
                }
                walk_expr(index, out);
            }
            WirNode::MemoryCopy { dest, src, len } => {
                walk_expr(dest, out);
                walk_expr(src, out);
                walk_expr(len, out);
            }
            WirNode::MemoryFill { dest, value, len } => {
                walk_expr(dest, out);
                walk_expr(value, out);
                walk_expr(len, out);
            }
            WirNode::If { cond, then_, els, .. } => {
                walk_expr(cond, out);
                collect_clos_signatures(then_, out);
                collect_clos_signatures(els, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_clos_signatures(body, out)
            }
            WirNode::Br { cond: Some(c), .. } => walk_expr(c, out),
            WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
                walk_expr(e, out)
            }
            WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
        }
    }
    for node in seq {
        walk_node(node, out);
    }
}

/// Per-function emission context: resolves local/func/import names and maintains
/// the enclosing labeled-frame stack used to compute relative branch depths.
struct EncodeCtx<'a> {
    local_index: &'a HashMap<&'a str, u32>,
    func_index: &'a HashMap<&'a str, u32>,
    import_index: &'a HashMap<&'a str, u32>,
    global_index: &'a HashMap<&'a str, u32>,
    /// Exact closure function type -> synthesized type index.
    clos_type_idx: &'a HashMap<ClosureSignature, u32>,
    /// (RFC-0005) Type-section index where concrete GC types begin; a struct opcode's
    /// `struct_id` resolves to `gc_base + struct_id`. Also the boundary below
    /// which type indices are unshifted (the reserved clos band).
    gc_base: u32,
    /// Type-section index of array definition zero.
    array_base: u32,
    /// (RFC-0005) How far the non-clos function-signature indices are shifted up to
    /// make room for concrete GC types. A `call_indirect` type index at
    /// sig-position `pos` resolves to `pos` if `pos < gc_base`, else
    /// `pos + gc_shift`.
    gc_shift: u32,
    /// Names of enclosing Block/Loop frames, innermost LAST.
    label_stack: Vec<String>,
}

impl EncodeCtx<'_> {
    fn local(&self, name: &str) -> u32 {
        *self
            .local_index
            .get(name)
            .unwrap_or_else(|| panic!("unknown local ${name}"))
    }

    fn global(&self, name: &str) -> u32 {
        *self
            .global_index
            .get(name)
            .unwrap_or_else(|| panic!("unknown global ${name}"))
    }

    /// Relative branch depth for a label: the innermost enclosing frame is 0.
    fn branch_depth(&self, target: &str) -> u32 {
        for (i, label) in self.label_stack.iter().rev().enumerate() {
            if label == target {
                return i as u32;
            }
        }
        panic!("br target ${target} has no enclosing Block/Loop frame")
    }

    fn encode_seq(&mut self, func: &mut Function, seq: &WirSeq) {
        for node in seq {
            self.encode_node(func, node);
        }
    }

    fn encode_node(&mut self, func: &mut Function, node: &WirNode) {
        match node {
            WirNode::SetLocal { local, value } => {
                self.encode_expr(func, value);
                func.instruction(&Instruction::LocalSet(self.local(local)));
            }
            WirNode::SetGlobal { global, value } => {
                self.encode_expr(func, value);
                func.instruction(&Instruction::GlobalSet(self.global(global)));
            }
            WirNode::Store { ptr, value, kind, offset } => {
                self.encode_expr(func, ptr);
                self.encode_expr(func, value);
                let mem = MemArg {
                    offset: *offset as u64,
                    align: load_align(*kind),
                    memory_index: 0,
                };
                let instr = match kind {
                    Kind::I32 => Instruction::I32Store(mem),
                    Kind::I64 => Instruction::I64Store(mem),
                    Kind::F64 => Instruction::F64Store(mem),
                    Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
                        unreachable!("reference-typed values are not stored to linear memory")
                    }
                };
                func.instruction(&instr);
            }
            WirNode::CallStoreMulti { func: name, args, dests } => {
                for a in args {
                    self.encode_expr(func, a);
                }
                let idx = *self
                    .func_index
                    .get(name.as_str())
                    .unwrap_or_else(|| panic!("call to unknown func ${name}"));
                func.instruction(&Instruction::Call(idx));
                // Results are popped top-first → store in reverse declaration order.
                for d in dests.iter().rev() {
                    let li = self.local(d);
                    func.instruction(&Instruction::LocalSet(li));
                }
            }
            WirNode::CallIndirectStoreMulti {
                signature,
                args,
                index,
                dests,
            } => {
                assert_eq!(
                    signature.params.len(),
                    args.len(),
                    "indirect call parameter signature does not match its arguments"
                );
                assert_eq!(
                    signature.results.len(),
                    dests.len(),
                    "indirect call result signature does not match its destinations"
                );
                for a in args {
                    self.encode_expr(func, a);
                }
                self.encode_expr(func, index);
                let pos = *self.clos_type_idx.get(signature).unwrap_or_else(|| {
                    panic!(
                        "call_indirect references unsynthesized closure signature {signature:?}"
                    )
                });
                let type_index = if pos < self.gc_base {
                    pos
                } else {
                    pos + self.gc_shift
                };
                func.instruction(&Instruction::CallIndirect {
                    type_index,
                    table_index: 0,
                });
                for d in dests.iter().rev() {
                    func.instruction(&Instruction::LocalSet(self.local(d)));
                }
            }
            WirNode::MemoryCopy { dest, src, len } => {
                self.encode_expr(func, dest);
                self.encode_expr(func, src);
                self.encode_expr(func, len);
                func.instruction(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            WirNode::MemoryFill { dest, value, len } => {
                self.encode_expr(func, dest);
                self.encode_expr(func, value);
                self.encode_expr(func, len);
                func.instruction(&Instruction::MemoryFill(0));
            }
            WirNode::Store8 { ptr, value, offset } => {
                self.encode_expr(func, ptr);
                self.encode_expr(func, value);
                func.instruction(&Instruction::I32Store8(MemArg {
                    offset: *offset as u64,
                    align: 0,
                    memory_index: 0,
                }));
            }
            WirNode::If {
                cond,
                then_,
                els,
                result,
            } => {
                self.encode_expr(func, cond);
                let bt = match result {
                    Some(t) => BlockType::Result(val_type(t.kind(), self.gc_base)),
                    None => BlockType::Empty,
                };
                func.instruction(&Instruction::If(bt));
                // An `if` IS a control frame in wasm: a `br N` INSIDE it counts the
                // if as one level toward N. (The named-label WAT printer is immune —
                // the assembler computes depths — but the numeric encoder must count
                // it.) Push a sentinel frame that no real `Br` target ever matches,
                // so `branch_depth` counts the if level without ever resolving to it.
                self.label_stack.push("\u{0}if".to_string());
                self.encode_seq(func, then_);
                func.instruction(&Instruction::Else);
                self.encode_seq(func, els);
                func.instruction(&Instruction::End);
                self.label_stack.pop();
            }
            WirNode::Block {
                label,
                result,
                body,
            } => {
                let bt = match result {
                    Some(t) => BlockType::Result(val_type(t.kind(), self.gc_base)),
                    None => BlockType::Empty,
                };
                func.instruction(&Instruction::Block(bt));
                self.label_stack.push(label.clone());
                self.encode_seq(func, body);
                self.label_stack.pop();
                func.instruction(&Instruction::End);
            }
            WirNode::Loop { label, body } => {
                func.instruction(&Instruction::Loop(BlockType::Empty));
                self.label_stack.push(label.clone());
                self.encode_seq(func, body);
                self.label_stack.pop();
                func.instruction(&Instruction::End);
            }
            WirNode::Br { target, cond } => match cond {
                Some(c) => {
                    self.encode_expr(func, c);
                    func.instruction(&Instruction::BrIf(self.branch_depth(target)));
                }
                None => {
                    func.instruction(&Instruction::Br(self.branch_depth(target)));
                }
            },
            WirNode::Drop(e) => {
                self.encode_expr(func, e);
                func.instruction(&Instruction::Drop);
            }
            // Do/Push both just evaluate the inner expression (Do for void,
            // Push to leave a value), matching the printer.
            WirNode::Do(e) => self.encode_expr(func, e),
            WirNode::Push(e) => self.encode_expr(func, e),
            WirNode::Return(Some(e)) => {
                self.encode_expr(func, e);
                func.instruction(&Instruction::Return);
            }
            WirNode::Return(None) => {
                func.instruction(&Instruction::Return);
            }
            WirNode::Unreachable => {
                func.instruction(&Instruction::Unreachable);
            }
            WirNode::StructSet { struct_id, field, base, value } => {
                self.encode_expr(func, base);
                self.encode_expr(func, value);
                func.instruction(&Instruction::StructSet {
                    struct_type_index: self.gc_base + struct_id,
                    field_index: *field,
                });
            }
            WirNode::ArraySet { array_id, array, index, value } => {
                self.encode_expr(func, array);
                self.encode_expr(func, index);
                self.encode_expr(func, value);
                func.instruction(&Instruction::ArraySet(self.array_base + array_id));
            }
        }
    }

    fn encode_expr(&mut self, func: &mut Function, e: &WirExpr) {
        match e {
            WirExpr::ConstI64(n) => {
                func.instruction(&Instruction::I64Const(*n));
            }
            WirExpr::ConstI32(n) => {
                func.instruction(&Instruction::I32Const(*n));
            }
            WirExpr::ConstF64(x) => {
                func.instruction(&Instruction::F64Const((*x).into()));
            }
            // A string pointer is an i32 byte offset into linear memory.
            WirExpr::StrPtr(off) => {
                func.instruction(&Instruction::I32Const(*off as i32));
            }
            WirExpr::GetLocal(name) => {
                func.instruction(&Instruction::LocalGet(self.local(name)));
            }
            WirExpr::GetGlobal(name) => {
                func.instruction(&Instruction::GlobalGet(self.global(name)));
            }
            WirExpr::ToSlot(inner, kind) => {
                self.encode_expr(func, inner);
                if let Some(instr) = to_slot_instr(*kind) {
                    func.instruction(&instr);
                }
            }
            WirExpr::FromSlot(inner, kind) => {
                self.encode_expr(func, inner);
                if let Some(instr) = from_slot_instr(*kind) {
                    func.instruction(&instr);
                }
            }
            WirExpr::Binary { op, kind, lhs, rhs } => {
                self.encode_expr(func, lhs);
                self.encode_expr(func, rhs);
                func.instruction(&binop_instr(*op, *kind));
            }
            WirExpr::Unary { op, kind, arg } => match op {
                UnOp::Not => {
                    self.encode_expr(func, arg);
                    func.instruction(&Instruction::I32Eqz);
                }
                UnOp::Neg => match kind {
                    Kind::F64 => {
                        self.encode_expr(func, arg);
                        func.instruction(&Instruction::F64Neg);
                    }
                    // `-x` == `0 - x`: zero pushed before the operand.
                    _ => {
                        func.instruction(&const_zero(*kind));
                        self.encode_expr(func, arg);
                        func.instruction(&binop_instr(BinOp::Sub, *kind));
                    }
                },
                // `~x` == `x ^ -1` (all bits set).
                UnOp::BitNot => {
                    self.encode_expr(func, arg);
                    func.instruction(&const_neg_one(*kind));
                    func.instruction(&binop_instr(BinOp::Xor, *kind));
                }
                UnOp::ToFloat => {
                    self.encode_expr(func, arg);
                    func.instruction(&Instruction::F64ConvertI64S);
                }
                UnOp::ToInt => {
                    self.encode_expr(func, arg);
                    func.instruction(&Instruction::I64TruncSatF64S);
                }
                UnOp::Sqrt => {
                    self.encode_expr(func, arg);
                    func.instruction(&Instruction::F64Sqrt);
                }
            },
            WirExpr::Convert { from, to, arg } => {
                self.encode_expr(func, arg);
                // Only i32<->i64 emit; everything else (incl. any f64) is a no-op,
                // matching codegen's `kind_convert`.
                match (from, to) {
                    (Kind::I64, Kind::I32) => {
                        func.instruction(&Instruction::I32WrapI64);
                    }
                    (Kind::I32, Kind::I64) => {
                        func.instruction(&Instruction::I64ExtendI32S);
                    }
                    _ => {}
                }
            }
            WirExpr::Load { ptr, kind, offset } => {
                self.encode_expr(func, ptr);
                let mem = MemArg {
                    offset: *offset as u64,
                    align: load_align(*kind),
                    memory_index: 0,
                };
                let instr = match kind {
                    Kind::I32 => Instruction::I32Load(mem),
                    Kind::I64 => Instruction::I64Load(mem),
                    Kind::F64 => Instruction::F64Load(mem),
                    Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
                        unreachable!("reference-typed values are not loaded from linear memory")
                    }
                };
                func.instruction(&instr);
            }
            WirExpr::MemorySize => {
                func.instruction(&Instruction::MemorySize(0));
            }
            WirExpr::MemoryGrow(pages) => {
                self.encode_expr(func, pages);
                func.instruction(&Instruction::MemoryGrow(0));
            }
            WirExpr::Load8U { ptr, offset } => {
                self.encode_expr(func, ptr);
                func.instruction(&Instruction::I32Load8U(MemArg {
                    offset: *offset as u64,
                    align: 0,
                    memory_index: 0,
                }));
            }
            WirExpr::Call { func: name, args } => {
                for a in args {
                    self.encode_expr(func, a);
                }
                let idx = *self
                    .func_index
                    .get(name.as_str())
                    .unwrap_or_else(|| panic!("call to unknown func ${name}"));
                func.instruction(&Instruction::Call(idx));
            }
            WirExpr::CallHost { import, args } => {
                for a in args {
                    self.encode_expr(func, a);
                }
                let idx = *self
                    .import_index
                    .get(import.as_str())
                    .unwrap_or_else(|| panic!("call to unknown host import ${import}"));
                func.instruction(&Instruction::Call(idx));
            }
            WirExpr::CallIndirect {
                signature,
                args,
                index,
            } => {
                // Args first, then the code index, then `call_indirect` — operand
                // order and type/table indices match codegen.
                assert_eq!(
                    signature.params.len(),
                    args.len(),
                    "indirect call parameter signature does not match its arguments"
                );
                for a in args {
                    self.encode_expr(func, a);
                }
                self.encode_expr(func, index);
                let pos = *self.clos_type_idx.get(signature).unwrap_or_else(|| {
                    panic!(
                        "call_indirect references unsynthesized closure signature {signature:?}"
                    )
                });
                // Shift past the struct types for a sig outside the reserved band.
                let type_index =
                    if pos < self.gc_base { pos } else { pos + self.gc_shift };
                func.instruction(&Instruction::CallIndirect { type_index, table_index: 0 });
            }
            WirExpr::Control(node) => self.encode_node(func, node),
            WirExpr::Seq(nodes) => self.encode_seq(func, nodes),
            WirExpr::StructNew { struct_id, args } => {
                for a in args {
                    self.encode_expr(func, a);
                }
                func.instruction(&Instruction::StructNew(self.gc_base + struct_id));
            }
            WirExpr::StructGet { struct_id, field, base } => {
                self.encode_expr(func, base);
                func.instruction(&Instruction::StructGet {
                    struct_type_index: self.gc_base + struct_id,
                    field_index: *field,
                });
            }
            WirExpr::ArrayNew { array_id, value, len } => {
                self.encode_expr(func, value);
                self.encode_expr(func, len);
                func.instruction(&Instruction::ArrayNew(self.array_base + array_id));
            }
            WirExpr::ArrayNewFixed { array_id, items } => {
                for item in items {
                    self.encode_expr(func, item);
                }
                func.instruction(&Instruction::ArrayNewFixed {
                    array_type_index: self.array_base + array_id,
                    array_size: items.len() as u32,
                });
            }
            WirExpr::ArrayGet { array_id, array, index } => {
                self.encode_expr(func, array);
                self.encode_expr(func, index);
                func.instruction(&Instruction::ArrayGet(self.array_base + array_id));
            }
            WirExpr::ArrayLen(array) => {
                self.encode_expr(func, array);
                func.instruction(&Instruction::ArrayLen);
            }
            WirExpr::RefCast { struct_id, value } => {
                self.encode_expr(func, value);
                func.instruction(&Instruction::RefCastNonNull(HeapType::Concrete(
                    self.gc_base + struct_id,
                )));
            }
            WirExpr::RefNull(kind) => {
                let heap = match kind {
                    Kind::ExternRef => HeapType::EXTERN,
                    Kind::StructRef => HeapType::Abstract {
                        shared: false,
                        ty: AbstractHeapType::Struct,
                    },
                    Kind::GcRef(id) => HeapType::Concrete(self.gc_base + id),
                    _ => unreachable!("RefNull of a non-reference kind {kind:?}"),
                };
                func.instruction(&Instruction::RefNull(heap));
            }
            WirExpr::RefIsNull(expr) => {
                self.encode_expr(func, expr);
                func.instruction(&Instruction::RefIsNull);
            }
        }
    }
}

/// to-slot conversion instruction for a value of `kind` (None if already i64).
/// Mirrors `wir::to_slot_op` — note I32 is the SIGNED extend.
fn to_slot_instr(kind: Kind) -> Option<Instruction<'static>> {
    match kind {
        Kind::I64 => None,
        Kind::I32 => Some(Instruction::I64ExtendI32S),
        Kind::F64 => Some(Instruction::I64ReinterpretF64),
        // (RFC-0005) A reference cannot be boxed into the i64 slot (no bit-pattern);
        // the crossing is a `typeck` reject (§4.4), so this is unreachable.
        Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
            unreachable!("cannot box a reference-typed value (a capability) into the i64 slot")
        }
    }
}

/// from-slot conversion instruction (None if the slot already is the value).
/// Mirrors `wir::from_slot_op`.
fn from_slot_instr(kind: Kind) -> Option<Instruction<'static>> {
    match kind {
        Kind::I64 => None,
        Kind::I32 => Some(Instruction::I32WrapI64),
        Kind::F64 => Some(Instruction::F64ReinterpretI64),
        Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
            unreachable!("cannot recover a reference-typed value (a capability) from the i64 slot")
        }
    }
}

/// `<kind>.const 0`.
fn const_zero(kind: Kind) -> Instruction<'static> {
    match kind {
        Kind::I32 => Instruction::I32Const(0),
        Kind::I64 => Instruction::I64Const(0),
        Kind::F64 => Instruction::F64Const(0.0.into()),
        Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
            unreachable!("no `.const 0` for a reference kind")
        }
    }
}

/// `<kind>.const -1`.
fn const_neg_one(kind: Kind) -> Instruction<'static> {
    match kind {
        Kind::I32 => Instruction::I32Const(-1),
        Kind::I64 => Instruction::I64Const(-1),
        Kind::F64 => Instruction::F64Const((-1.0).into()),
        Kind::ExternRef | Kind::StructRef | Kind::GcRef(_) => {
            unreachable!("no `.const -1` for a reference kind")
        }
    }
}

/// The `Instruction` matching `BinOp::mnemonic(op, kind)` exactly.
fn binop_instr(op: BinOp, kind: Kind) -> Instruction<'static> {
    match (op, kind) {
        (BinOp::Add, Kind::I32) => Instruction::I32Add,
        (BinOp::Add, Kind::I64) => Instruction::I64Add,
        (BinOp::Add, Kind::F64) => Instruction::F64Add,
        (BinOp::Sub, Kind::I32) => Instruction::I32Sub,
        (BinOp::Sub, Kind::I64) => Instruction::I64Sub,
        (BinOp::Sub, Kind::F64) => Instruction::F64Sub,
        (BinOp::Mul, Kind::I32) => Instruction::I32Mul,
        (BinOp::Mul, Kind::I64) => Instruction::I64Mul,
        (BinOp::Mul, Kind::F64) => Instruction::F64Mul,
        // Div: f64 is `f64.div`, integers are `_s` (signed).
        (BinOp::Div, Kind::F64) => Instruction::F64Div,
        (BinOp::Div, Kind::I32) => Instruction::I32DivS,
        (BinOp::Div, Kind::I64) => Instruction::I64DivS,
        // Rem: `<p>.rem_s` for all kinds the printer supports (i32/i64).
        (BinOp::Rem, Kind::I32) => Instruction::I32RemS,
        (BinOp::Rem, Kind::I64) => Instruction::I64RemS,
        (BinOp::And, Kind::I32) => Instruction::I32And,
        (BinOp::And, Kind::I64) => Instruction::I64And,
        (BinOp::Or, Kind::I32) => Instruction::I32Or,
        (BinOp::Or, Kind::I64) => Instruction::I64Or,
        (BinOp::Xor, Kind::I32) => Instruction::I32Xor,
        (BinOp::Xor, Kind::I64) => Instruction::I64Xor,
        (BinOp::Shl, Kind::I32) => Instruction::I32Shl,
        (BinOp::Shl, Kind::I64) => Instruction::I64Shl,
        (BinOp::Shr, Kind::I32) => Instruction::I32ShrS,
        (BinOp::Shr, Kind::I64) => Instruction::I64ShrS,
        // Comparisons.
        (BinOp::Eq, Kind::I32) => Instruction::I32Eq,
        (BinOp::Eq, Kind::I64) => Instruction::I64Eq,
        (BinOp::Eq, Kind::F64) => Instruction::F64Eq,
        (BinOp::Ne, Kind::I32) => Instruction::I32Ne,
        (BinOp::Ne, Kind::I64) => Instruction::I64Ne,
        (BinOp::Ne, Kind::F64) => Instruction::F64Ne,
        (BinOp::Lt, Kind::F64) => Instruction::F64Lt,
        (BinOp::Lt, Kind::I32) => Instruction::I32LtS,
        (BinOp::Lt, Kind::I64) => Instruction::I64LtS,
        (BinOp::Le, Kind::F64) => Instruction::F64Le,
        (BinOp::Le, Kind::I32) => Instruction::I32LeS,
        (BinOp::Le, Kind::I64) => Instruction::I64LeS,
        (BinOp::Gt, Kind::F64) => Instruction::F64Gt,
        (BinOp::Gt, Kind::I32) => Instruction::I32GtS,
        (BinOp::Gt, Kind::I64) => Instruction::I64GtS,
        (BinOp::Ge, Kind::F64) => Instruction::F64Ge,
        (BinOp::Ge, Kind::I32) => Instruction::I32GeS,
        (BinOp::Ge, Kind::I64) => Instruction::I64GeS,
        // Unsigned forms (the helper layer's pointer/length math) — i32/i64 only.
        (BinOp::DivU, Kind::I32) => Instruction::I32DivU,
        (BinOp::DivU, Kind::I64) => Instruction::I64DivU,
        (BinOp::RemU, Kind::I32) => Instruction::I32RemU,
        (BinOp::RemU, Kind::I64) => Instruction::I64RemU,
        (BinOp::ShrU, Kind::I32) => Instruction::I32ShrU,
        (BinOp::ShrU, Kind::I64) => Instruction::I64ShrU,
        (BinOp::LtU, Kind::I32) => Instruction::I32LtU,
        (BinOp::LtU, Kind::I64) => Instruction::I64LtU,
        (BinOp::LeU, Kind::I32) => Instruction::I32LeU,
        (BinOp::LeU, Kind::I64) => Instruction::I64LeU,
        (BinOp::GtU, Kind::I32) => Instruction::I32GtU,
        (BinOp::GtU, Kind::I64) => Instruction::I64GtU,
        (BinOp::GeU, Kind::I32) => Instruction::I32GeU,
        (BinOp::GeU, Kind::I64) => Instruction::I64GeU,
        // The mnemonic for the missing arithmetic-on-f64 (And/Or/Xor/Shl/Shr/Rem
        // on f64, or any unsigned op on f64) is never produced — there are no such
        // wasm ops. The printer would emit e.g. `f64.and` (invalid); a bug if hit.
        (op, kind) => panic!("no wasm instruction for {op:?} on {kind:?}"),
    }
}

#[cfg(test)]
#[cfg(feature = "native")]
#[path = "wir_encode_tests.rs"]
mod tests;
