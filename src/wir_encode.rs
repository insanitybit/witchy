//! WIR → wasm **binary** encoder (Milestone M3). Turns a [`crate::wir::WirModule`]
//! directly into a WebAssembly binary via the `wasm-encoder` crate, bypassing the
//! WAT text path (`to_wat` + `wat::parse_str`). The output is behaviorally
//! identical to that path — `print_node`/`print_expr` in `src/wir.rs` are the
//! authoritative semantics this mirrors instruction-for-instruction.
//!
//! Index spaces: imported functions come first (index = position in
//! `module.imports`), then defined functions (index = `imports.len()` + position
//! in `module.funcs`). Locals are params-then-body in declaration order.

// M3: the encoder is built and validated by its own tests but not yet wired into
// `compile_module`'s pipeline (that sink-flip is the next step), so `encode` and
// its helpers read as dead to the non-test build. Mirrors `wir.rs`'s allow.
#![allow(dead_code)]

use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction, MemArg, MemorySection, MemoryType, Module, RefType, TableSection, TableType,
    TypeSection, ValType,
};

use crate::wir::{BinOp, GlobalInit, Kind, UnOp, WirExpr, WirModule, WirNode, WirSeq};

/// Map a WIR `Kind` to a wasm-encoder `ValType`.
fn val_type(kind: Kind) -> ValType {
    match kind {
        Kind::I32 => ValType::I32,
        Kind::I64 => ValType::I64,
        Kind::F64 => ValType::F64,
    }
}

/// Natural alignment exponent (log2 of byte size) for a `<kind>.load`.
fn load_align(kind: Kind) -> u32 {
    match kind {
        Kind::I32 => 2, // 4 bytes
        Kind::I64 => 3, // 8 bytes
        Kind::F64 => 3, // 8 bytes
    }
}

/// Encode a [`WirModule`] into a wasm binary.
pub fn encode(module: &WirModule) -> Vec<u8> {
    // --- Type section: collect unique (params, results) signatures ---------
    // Imports carry their param/result `Kind`s directly. Funcs derive params
    // from `params[*].ty.kind()` and results from `ret[*].kind()`. `Kind` is not
    // `Hash`, so dedup with a small linear scan over the collected signatures
    // (the signature set is tiny — a handful per module).
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

    // Closure `$clos{N}` signatures (one i32 env-pointer param, N i64 slot args,
    // one i64 slot result). `$clos0..=MAX_CLOS` are interned FIRST, so they hold
    // type indices `0..=MAX_CLOS`: spliced prelude raw bodies were assembled
    // against that exact layout and bake their `call_indirect (type $closN)`
    // operands as those indices. Reserving them up front (even when no visible
    // node references them) keeps a spliced `$dict_update`'s indirect call valid.
    let mut clos_type_idx: HashMap<usize, u32> = HashMap::new();
    for n in 0..=crate::wir::MAX_CLOS {
        let mut params = vec![Kind::I32];
        params.extend(std::iter::repeat_n(Kind::I64, n));
        let idx = intern(params, vec![Kind::I64]);
        clos_type_idx.insert(n, idx);
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

    // Any wider closure arity actually referenced by a visible `CallIndirect`
    // (beyond the reserved prelude band) interns after the import/func types.
    let mut clos_arities: Vec<usize> = Vec::new();
    for f in &module.funcs {
        collect_clos_arities(&f.body, &mut clos_arities);
    }
    for &n in &clos_arities {
        if !clos_type_idx.contains_key(&n) {
            let mut params = vec![Kind::I32];
            params.extend(std::iter::repeat_n(Kind::I64, n));
            let idx = intern(params, vec![Kind::I64]);
            clos_type_idx.insert(n, idx);
        }
    }

    let mut type_section = TypeSection::new();
    for (params, results) in &sigs {
        type_section.ty().function(
            params.iter().map(|k| val_type(*k)),
            results.iter().map(|k| val_type(*k)),
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
        import_section.import("witchy", &imp.name, EntityType::Function(ty_idx));
    }

    // --- Function section (declares each defined func's type) ---------------
    let mut function_section = FunctionSection::new();
    for &ty_idx in &func_type_idx {
        function_section.function(ty_idx);
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
            GlobalType { val_type: val_type(g.kind), mutable: g.mutable, shared: false },
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
            body_local_types.push(val_type(l.ty.kind()));
        }

        let mut function = Function::new_with_locals_types(body_local_types);
        let mut ctx = EncodeCtx {
            local_index: &local_index,
            func_index: &func_index,
            import_index: &import_index,
            global_index: &global_index,
            clos_type_idx: &clos_type_idx,
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
    wasm.finish()
}

/// Collect the distinct closure arities referenced by `CallIndirect` nodes in a
/// node sequence (for `$clos{N}` type-section synthesis). Walks both nodes and
/// their nested expressions.
fn collect_clos_arities(seq: &WirSeq, out: &mut Vec<usize>) {
    fn push(out: &mut Vec<usize>, n: usize) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    fn walk_expr(e: &WirExpr, out: &mut Vec<usize>) {
        match e {
            WirExpr::CallIndirect { type_arity, args, index } => {
                push(out, *type_arity);
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
            WirExpr::Load { ptr, .. } => walk_expr(ptr, out),
            WirExpr::MemoryGrow(pages) => walk_expr(pages, out),
            WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            WirExpr::Control(node) => walk_node(node, out),
            WirExpr::Seq(nodes) => collect_clos_arities(nodes, out),
            // Leaves: no nested closure-arity references.
            WirExpr::ConstI64(_)
            | WirExpr::ConstF64(_)
            | WirExpr::ConstI32(_)
            | WirExpr::StrPtr(_)
            | WirExpr::MemorySize
            | WirExpr::GetLocal(_)
            | WirExpr::GetGlobal(_) => {}
        }
    }
    fn walk_node(node: &WirNode, out: &mut Vec<usize>) {
        match node {
            WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
                walk_expr(value, out)
            }
            WirNode::Store { ptr, value, .. } => {
                walk_expr(ptr, out);
                walk_expr(value, out);
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
                collect_clos_arities(then_, out);
                collect_clos_arities(els, out);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                collect_clos_arities(body, out)
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
    /// Closure arity -> `$clos{N}` type index (for `call_indirect`).
    clos_type_idx: &'a HashMap<usize, u32>,
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
                };
                func.instruction(&instr);
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
            WirNode::If {
                cond,
                then_,
                els,
                result,
            } => {
                self.encode_expr(func, cond);
                let bt = match result {
                    Some(t) => BlockType::Result(val_type(t.kind())),
                    None => BlockType::Empty,
                };
                func.instruction(&Instruction::If(bt));
                // `if`/`else` are not labeled targets; no frame pushed (br depth
                // counts Block/Loop only, mirroring the named-label model where
                // `if` carries no `$label`).
                self.encode_seq(func, then_);
                func.instruction(&Instruction::Else);
                self.encode_seq(func, els);
                func.instruction(&Instruction::End);
            }
            WirNode::Block {
                label,
                result,
                body,
            } => {
                let bt = match result {
                    Some(t) => BlockType::Result(val_type(t.kind())),
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
            WirExpr::CallIndirect { type_arity, args, index } => {
                // Args (env ptr then slot args) first, then the code index, then
                // `call_indirect (type $clos{N})` on table 0 — operand order and
                // type/table indices match codegen.
                for a in args {
                    self.encode_expr(func, a);
                }
                self.encode_expr(func, index);
                let type_index = *self.clos_type_idx.get(type_arity).unwrap_or_else(|| {
                    panic!("call_indirect references unsynthesized $clos{type_arity}")
                });
                func.instruction(&Instruction::CallIndirect { type_index, table_index: 0 });
            }
            WirExpr::Control(node) => self.encode_node(func, node),
            WirExpr::Seq(nodes) => self.encode_seq(func, nodes),
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
    }
}

/// from-slot conversion instruction (None if the slot already is the value).
/// Mirrors `wir::from_slot_op`.
fn from_slot_instr(kind: Kind) -> Option<Instruction<'static>> {
    match kind {
        Kind::I64 => None,
        Kind::I32 => Some(Instruction::I32WrapI64),
        Kind::F64 => Some(Instruction::F64ReinterpretI64),
    }
}

/// `<kind>.const 0`.
fn const_zero(kind: Kind) -> Instruction<'static> {
    match kind {
        Kind::I32 => Instruction::I32Const(0),
        Kind::I64 => Instruction::I64Const(0),
        Kind::F64 => Instruction::F64Const(0.0.into()),
    }
}

/// `<kind>.const -1`.
fn const_neg_one(kind: Kind) -> Instruction<'static> {
    match kind {
        Kind::I32 => Instruction::I32Const(-1),
        Kind::I64 => Instruction::I64Const(-1),
        Kind::F64 => Instruction::F64Const((-1.0).into()),
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
mod tests {
    use super::*;
    use crate::wir::{
        to_wat, BinOp, DataSegment, GlobalInit, Kind, UnOp, WirExpr, WirFunc, WirGlobal, WirImport,
        WirLocal, WirModule, WirNode, WirTable, WirTy,
    };
    use std::sync::{Arc, Mutex};

    fn local(name: &str, ty: WirTy) -> WirLocal {
        WirLocal { name: name.into(), ty }
    }

    /// Instantiate a wasm binary and run its `run` export, capturing `print_int`
    /// and `print` output as ordered lines. (Copied from `wir.rs`'s test setup.)
    fn run_binary(binary: &[u8]) -> Vec<String> {
        let engine = wasmtime::Engine::default();
        let m = wasmtime::Module::new(&engine, binary)
            .unwrap_or_else(|e| panic!("encoded module invalid: {e}"));
        let out: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = wasmtime::Linker::new(&engine);
        let o = out.clone();
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                o.lock().unwrap().push(n.to_string());
            })
            .unwrap();
        let o = out.clone();
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: wasmtime::Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").and_then(|e| e.into_memory()).unwrap();
                    let data = mem.data(&caller);
                    let s = String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize])
                        .into_owned();
                    o.lock().unwrap().push(s);
                },
            )
            .unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let inst = linker.instantiate(&mut store, &m).expect("instantiate");
        let run = inst.get_typed_func::<(), ()>(&mut store, "run").expect("run export");
        run.call(&mut store, ()).expect("run");
        let v = out.lock().unwrap().clone();
        v
    }

    /// Run a module via the binary encoder.
    fn run_encoded(module: &WirModule) -> Vec<String> {
        run_binary(&encode(module))
    }

    /// Run a module via the legacy WAT path.
    fn run_wat(module: &WirModule) -> Vec<String> {
        let wat = to_wat(module);
        let binary = wat::parse_str(&wat)
            .unwrap_or_else(|e| panic!("WIR→WAT did not assemble: {e}\n---\n{wat}"));
        run_binary(&binary)
    }

    /// Assert the encoder output runs identically to the expected lines AND to the
    /// WAT path's output (the binary-vs-WAT agreement gate).
    fn assert_agrees(module: &WirModule, expected: &[&str]) {
        let exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        let enc = run_encoded(module);
        let wat = run_wat(module);
        assert_eq!(enc, exp, "binary output mismatch");
        assert_eq!(enc, wat, "binary vs WAT output mismatch");
    }

    /// Module with one Int-returning func + a `run` that prints its result.
    /// (Mirrors `wir.rs`'s `int_demo`.)
    fn int_demo(f: WirFunc, call: WirExpr) -> WirModule {
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![call],
            })],
            raw_body: None,
        };
        WirModule {
            imports: vec![
                WirImport { name: "print_int".into(), params: vec![Kind::I64], results: vec![] },
                WirImport {
                    name: "print".into(),
                    params: vec![Kind::I32, Kind::I32],
                    results: vec![],
                },
            ],
            funcs: vec![f, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        }
    }

    /// The #35 memory primitives — Store / Load(offset) / MemoryCopy / MemoryFill /
    /// MemorySize / MemoryGrow and an unsigned compare — encode AND print-via-WAT
    /// identically and run correctly. These are the nodes the allocation helpers
    /// ($ensure/$concat/$mkN) lower to.
    #[test]
    fn memory_ops_roundtrip() {
        use WirExpr::*;
        let i32c = |n: i32| ConstI32(n);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                // mem[16] = 99 (i64), then copy those 8 bytes to mem[32].
                WirNode::Store { ptr: i32c(16), value: ConstI64(99), kind: Kind::I64, offset: 0 },
                WirNode::MemoryCopy { dest: i32c(32), src: i32c(16), len: i32c(8) },
                // print the copied value (99).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Load { ptr: Box::new(i32c(32)), kind: Kind::I64, offset: 0 }],
                }),
                // memory.size >u 0  →  1.
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Binary {
                            op: BinOp::GtU,
                            kind: Kind::I32,
                            lhs: Box::new(MemorySize),
                            rhs: Box::new(i32c(0)),
                        }),
                    }],
                }),
                // memory.grow(1) returns the previous size in pages (1).
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(MemoryGrow(Box::new(i32c(1)))),
                    }],
                }),
                // memory.fill mem[64..72] = 0, then read one byte back (0).
                WirNode::MemoryFill { dest: i32c(64), value: i32c(0), len: i32c(8) },
                WirNode::Do(CallHost {
                    import: "print_int".into(),
                    args: vec![Convert {
                        from: Kind::I32,
                        to: Kind::I64,
                        arg: Box::new(Load { ptr: Box::new(i32c(64)), kind: Kind::I32, offset: 0 }),
                    }],
                }),
            ],
            raw_body: None,
        };
        let module = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&module, &["99", "1", "1", "0"]);
    }

    #[test]
    fn arithmetic_roundtrips() {
        // fn add() -> Int: (2 + 3) * 4 == 20
        let add = WirFunc {
            name: "add".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Mul,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(2)),
                    rhs: Box::new(WirExpr::ConstI64(3)),
                }),
                rhs: Box::new(WirExpr::ConstI64(4)),
            }))],
            raw_body: None,
        };
        let m = int_demo(add, WirExpr::Call { func: "add".into(), args: vec![] });
        assert_agrees(&m, &["20"]);
    }

    #[test]
    fn if_with_result_roundtrips() {
        // fn pick(b: Bool) -> Int: if b: 10 else: 20
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn loop_spine_roundtrips() {
        // fn sum_to(n) -> Int: sum of 0..n (Block/Loop/Br spine).
        let i_lt_n = WirExpr::Binary {
            op: BinOp::Lt,
            kind: Kind::I64,
            lhs: Box::new(WirExpr::GetLocal("i".into())),
            rhs: Box::new(WirExpr::GetLocal("n".into())),
        };
        let not_i_lt_n = WirExpr::Binary {
            op: BinOp::Eq,
            kind: Kind::I32,
            lhs: Box::new(i_lt_n),
            rhs: Box::new(WirExpr::ConstI32(0)),
        };
        let sum_to = WirFunc {
            name: "sum_to".into(),
            params: vec![local("n", WirTy::Int)],
            ret: vec![WirTy::Int],
            locals: vec![local("total", WirTy::Int), local("i", WirTy::Int)],
            body: vec![
                WirNode::SetLocal { local: "total".into(), value: WirExpr::ConstI64(0) },
                WirNode::SetLocal { local: "i".into(), value: WirExpr::ConstI64(0) },
                WirNode::Block {
                    label: "exit".into(),
                    result: None,
                    body: vec![WirNode::Loop {
                        label: "head".into(),
                        body: vec![
                            WirNode::Br { target: "exit".into(), cond: Some(not_i_lt_n) },
                            WirNode::SetLocal {
                                local: "total".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("total".into())),
                                    rhs: Box::new(WirExpr::GetLocal("i".into())),
                                },
                            },
                            WirNode::SetLocal {
                                local: "i".into(),
                                value: WirExpr::Binary {
                                    op: BinOp::Add,
                                    kind: Kind::I64,
                                    lhs: Box::new(WirExpr::GetLocal("i".into())),
                                    rhs: Box::new(WirExpr::ConstI64(1)),
                                },
                            },
                            WirNode::Br { target: "head".into(), cond: None },
                        ],
                    }],
                },
                WirNode::Return(Some(WirExpr::GetLocal("total".into()))),
            ],
            raw_body: None,
        };
        // sum 0..5 = 10
        let m = int_demo(
            sum_to,
            WirExpr::Call { func: "sum_to".into(), args: vec![WirExpr::ConstI64(5)] },
        );
        assert_agrees(&m, &["10"]);
    }

    #[test]
    fn slot_conversions_roundtrip() {
        // FromSlot(ToSlot(x, k), k) == x.
        for (kind, value, expect) in [(Kind::I64, WirExpr::ConstI64(42), "42")] {
            let f = WirFunc {
                name: "rt".into(),
                params: vec![],
                ret: vec![WirTy::Int],
                locals: vec![],
                body: vec![WirNode::Return(Some(WirExpr::FromSlot(
                    Box::new(WirExpr::ToSlot(Box::new(value), kind)),
                    kind,
                )))],
                raw_body: None,
            };
            let m = int_demo(f, WirExpr::Call { func: "rt".into(), args: vec![] });
            assert_agrees(&m, &[expect]);
        }
    }

    #[test]
    fn unary_ops_roundtrip() {
        // fn neg() -> Int: -(5) == -5
        let neg = WirFunc {
            name: "neg".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::Neg,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(5)),
            }))],
            raw_body: None,
        };
        let m = int_demo(neg, WirExpr::Call { func: "neg".into(), args: vec![] });
        assert_agrees(&m, &["-5"]);

        // fn bnot() -> Int: ~0 == -1
        let bnot = WirFunc {
            name: "bnot".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Unary {
                op: UnOp::BitNot,
                kind: Kind::I64,
                arg: Box::new(WirExpr::ConstI64(0)),
            }))],
            raw_body: None,
        };
        let m = int_demo(bnot, WirExpr::Call { func: "bnot".into(), args: vec![] });
        assert_agrees(&m, &["-1"]);
    }

    #[test]
    fn control_value_if_roundtrips() {
        // value-`if` in expression position (the `&&`/`||`/if-expr shape).
        let pick = WirFunc {
            name: "pick".into(),
            params: vec![local("b", WirTy::Bool)],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Control(Box::new(WirNode::If {
                cond: WirExpr::GetLocal("b".into()),
                then_: vec![WirNode::Push(WirExpr::ConstI64(10))],
                els: vec![WirNode::Push(WirExpr::ConstI64(20))],
                result: Some(WirTy::Int),
            }))))],
            raw_body: None,
        };
        let m_true = int_demo(
            pick.clone(),
            WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(1)] },
        );
        assert_agrees(&m_true, &["10"]);
        let m_false =
            int_demo(pick, WirExpr::Call { func: "pick".into(), args: vec![WirExpr::ConstI32(0)] });
        assert_agrees(&m_false, &["20"]);
    }

    #[test]
    fn string_print_roundtrips() {
        // print(console, "hi"): data [2,0,0,0,'h','i'] at offset 8.
        let mut bytes = (2u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(b"hi");
        let print_call = WirExpr::CallHost {
            import: "print".into(),
            args: vec![
                WirExpr::Binary {
                    op: BinOp::Add,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::StrPtr(8)),
                    rhs: Box::new(WirExpr::ConstI32(4)),
                },
                WirExpr::Load { ptr: Box::new(WirExpr::StrPtr(8)), kind: Kind::I32, offset: 0 },
            ],
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(print_call)],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print".into(),
                params: vec![Kind::I32, Kind::I32],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![DataSegment { offset: 8, bytes }],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["hi"]);
    }

    // --- M3-obstacle additions: globals, table+call_indirect, multi-value,
    //     raw-body splice ---------------------------------------------------

    #[test]
    fn mutable_global_set_get_roundtrips() {
        // A mutable i64 global `$counter` initialized to 5. `run`:
        //   counter = counter + 37   (global.get; +; global.set)
        //   print_int(counter)       (global.get)  -> 42
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![
                WirNode::SetGlobal {
                    global: "counter".into(),
                    value: WirExpr::Binary {
                        op: BinOp::Add,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetGlobal("counter".into())),
                        rhs: Box::new(WirExpr::ConstI64(37)),
                    },
                },
                WirNode::Do(WirExpr::CallHost {
                    import: "print_int".into(),
                    args: vec![WirExpr::GetGlobal("counter".into())],
                }),
            ],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![run],
            memory_pages: 1,
            data: vec![],
            globals: vec![WirGlobal {
                name: "counter".into(),
                kind: Kind::I64,
                mutable: true,
                init: GlobalInit::I64(5),
                export: None,
            }],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["42"]);
    }

    #[test]
    fn table_call_indirect_roundtrips() {
        // A lifted lambda `$__lam0` with the `$clos1` signature
        // `(param i32 env) (param i64 arg) (result i64)`: returns arg + 100.
        // A closure record `[code_index=0]` lives at offset 0 in memory (data
        // segment of 4 zero bytes). `run` reads its code index and `call_indirect
        // (type $clos1)` with the record as env and a literal slot arg of 5 -> 105.
        let lam0 = WirFunc {
            name: "__lam0".into(),
            params: vec![local("env", WirTy::Capability), local("arg", WirTy::Slot)],
            ret: vec![WirTy::Slot],
            locals: vec![],
            body: vec![WirNode::Return(Some(WirExpr::Binary {
                op: BinOp::Add,
                kind: Kind::I64,
                lhs: Box::new(WirExpr::GetLocal("arg".into())),
                rhs: Box::new(WirExpr::ConstI64(100)),
            }))],
            raw_body: None,
        };
        // The closure record at offset 0 holds the code index (0) as its i32
        // header. `call_indirect` args = [env ptr, slot arg]; index = i32.load env.
        let call = WirExpr::CallIndirect {
            type_arity: 1,
            args: vec![WirExpr::ConstI32(0), WirExpr::ToSlot(Box::new(WirExpr::ConstI64(5)), Kind::I64)],
            index: Box::new(WirExpr::Load {
                ptr: Box::new(WirExpr::ConstI32(0)),
                kind: Kind::I32,
                offset: 0,
            }),
        };
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![WirNode::Do(WirExpr::CallHost {
                import: "print_int".into(),
                args: vec![call],
            })],
            raw_body: None,
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![lam0, run],
            memory_pages: 1,
            // 4 zero bytes at offset 0: the closure record's code index (slot 0).
            data: vec![DataSegment { offset: 0, bytes: vec![0, 0, 0, 0] }],
            globals: vec![],
            table: Some(WirTable { funcs: vec!["__lam0".into()] }),
            exports: vec![("run".into(), "run".into())],
        };
        assert_agrees(&m, &["105"]);
    }

    #[test]
    fn multi_value_result_roundtrips() {
        // fn pair() -> (Int, Int): leaves 30 then 12 on the stack — a 2-result
        // func exercising a `(result i64 i64)` signature in the type section.
        // `run` calls $pair (two i64s on the stack), `i64.add`s them, and prints
        // the sum -> 42. The node-tree `WirExpr` model can't reference a call's
        // multiple stack results directly, so `run` is spliced as a raw body
        // (which also covers the splice + multi-value paths together).
        let pair = WirFunc {
            name: "pair".into(),
            params: vec![],
            ret: vec![WirTy::Int, WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::ConstI64(30)),
                WirNode::Push(WirExpr::ConstI64(12)),
            ],
            raw_body: None,
        };
        // Index space: import print_int = 0; defined funcs pair = 1, run = 2.
        let mut run_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        run_fn.instruction(&Instruction::Call(1)); // call $pair -> [30, 12]
        run_fn.instruction(&Instruction::I64Add); // -> 42
        run_fn.instruction(&Instruction::Call(0)); // call $print_int
        run_fn.instruction(&Instruction::End);
        let run = WirFunc {
            name: "run".into(),
            params: vec![],
            ret: vec![],
            locals: vec![],
            body: vec![],
            raw_body: Some(run_fn.into_raw_body()),
        };
        let m = WirModule {
            imports: vec![WirImport {
                name: "print_int".into(),
                params: vec![Kind::I64],
                results: vec![],
            }],
            funcs: vec![pair, run],
            memory_pages: 1,
            data: vec![],
            globals: vec![],
            table: None,
            exports: vec![("run".into(), "run".into())],
        };
        assert_eq!(run_encoded(&m), vec!["42".to_string()]);
    }

    #[test]
    fn raw_body_splice_roundtrips() {
        // A func `magic` whose body is supplied as pre-encoded wasm bytes (the
        // splice path): `i64.const 99; end`. `run` calls it and prints -> 99.
        // The raw body is `Function::into_raw_body()` output (locals + instrs +
        // End, NO length prefix) — exactly what `CodeSection::raw` consumes.
        let mut magic_fn = Function::new_with_locals_types(Vec::<ValType>::new());
        magic_fn.instruction(&Instruction::I64Const(99));
        magic_fn.instruction(&Instruction::End);
        let magic = WirFunc {
            name: "magic".into(),
            params: vec![],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![], // ignored: raw_body wins
            raw_body: Some(magic_fn.into_raw_body()),
        };
        let m = int_demo(magic, WirExpr::Call { func: "magic".into(), args: vec![] });
        // No WAT path for a raw-body func, so assert on the encoder output only.
        assert_eq!(run_encoded(&m), vec!["99".to_string()]);
    }
}
