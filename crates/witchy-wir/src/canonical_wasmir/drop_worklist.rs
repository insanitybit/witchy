//! Constant-Wasm-stack destruction of owned graphs.
//!
//! A frame is (object, catalog row, next child), with -1 meaning first visit.
//! Parents stay on the worklist until their children have been released.
//! Only one child is pushed at a time, so temporary space is O(depth), not
//! O(collection length). No guest allocation or host callback may occur here.

use super::*;
use crate::layout::LayoutId;
use wasm_encoder::Instruction as I;
use super::ref_encoding::RefEncoding;

pub(super) const SCRATCH: usize = 12;
const OBJECT: usize = 0;
const COUNT: usize = 1;
const INDEX: usize = 2;
const FRAME: usize = 3;
const BASE: usize = 4;
const TOP: usize = 5;
const TAG: usize = 6;
const HIGH: usize = 7;
const ADDRESS: usize = 8;
const CHILD_TAG: usize = 9;
const END: usize = 10;
const PAGES: usize = 11;
const FRAME_BYTES: i32 = 12;

enum Children {
    Fields(Vec<(u32, LayoutId, bool)>),
    Elements(CollectionLayout, LayoutId),
    Dictionary(Vec<(u32, LayoutId)>),
}

fn dictionary_pointer(function: FunctionId, layouts: &LayoutInterner, slot: LayoutId) -> Result<Option<LayoutId>, EncodeError> {
    let concrete = layouts.resolve_nominal(slot).map_err(|_| EncodeError::UnsupportedLayout { function, operation: "DropRef dictionary slot" })?;
    match layouts.get(concrete).map(|descriptor| descriptor.kind()) {
        Some(LayoutKind::RuntimePointerValueSlot { pointee, semantics: PointerSlot::Owned }) => Ok(Some(*pointee)),
        Some(LayoutKind::Scalar(_)) => Ok(None),
        _ => Err(EncodeError::UnsupportedLayout { function, operation: "DropRef dictionary slot" }),
    }
}

fn rows(function: FunctionId, layouts: &LayoutInterner, root: LayoutId) -> Result<BTreeMap<LayoutId, Children>, EncodeError> {
    let mut result = BTreeMap::new();
    let mut pending = VecDeque::from([root]);
    while let Some(id) = pending.pop_front() {
        if result.contains_key(&id) { continue; }
        let concrete = layouts.resolve_nominal(id).map_err(|_| EncodeError::UnsupportedLayout {
            function, operation: "DropRef nominal layout",
        })?;
        let descriptor = layouts.get(concrete).ok_or(EncodeError::UnsupportedLayout {
            function, operation: "DropRef missing layout",
        })?;
        let children = match descriptor.kind() {
            LayoutKind::PackedList { .. } | LayoutKind::RuntimeBuffer { .. } => {
                let collection = collection_layout(function, layouts, id).map_err(EncodeError::Lowering)?;
                match collection.pointer_layout {
                    Some(child) => { pending.push_back(child); Children::Elements(collection, child) }
                    None => Children::Fields(Vec::new()),
                }
            }
            LayoutKind::PackedRecord { .. } | LayoutKind::Tuple { .. } => {
                let fields = fixed_pointer_fields(function, layouts, id).map_err(EncodeError::Lowering)?;
                pending.extend(fields.iter().map(|(_, child, _)| *child));
                Children::Fields(fields)
            }
            LayoutKind::CanonicalDictionary { key, value, .. } => {
                let mut fields = Vec::new();
                if let Some(child) = dictionary_pointer(function, layouts, *key)? { fields.push((16, child)); }
                if let Some(child) = dictionary_pointer(function, layouts, *value)? { fields.push((24, child)); }
                pending.extend(fields.iter().map(|(_, child)| *child));
                if fields.is_empty() { Children::Fields(Vec::new()) } else { Children::Dictionary(fields) }
            }
            LayoutKind::Scalar(_) => Children::Fields(Vec::new()),
            _ => return Err(EncodeError::UnsupportedLayout { function, operation: "DropRef unsupported owned layout" }),
        };
        result.insert(id, children);
    }
    if result.len() >= u32::MAX as usize {
        return Err(EncodeError::UnsupportedLayout { function, operation: "DropRef layout table overflow" });
    }
    Ok(result)
}

fn code(body: &mut WasmFunctionEncoder, instructions: impl IntoIterator<Item = I<'static>>) {
    for instruction in instructions { body.instruction(&instruction); }
}

fn load() -> I<'static> { I::I32Load(memarg(4)) }
fn store() -> I<'static> { I::I32Store(memarg(4)) }

/// A frame's size in reference units: 12 bytes under `Byte`, two 8-byte
/// units under `Scaled8`, so every frame address is itself a reference.
fn frame_units(refs: RefEncoding) -> i32 {
    match refs {
        RefEncoding::Byte => FRAME_BYTES,
        RefEncoding::Scaled8 => 2,
    }
}

/// The address `offset` bytes past the reference in `slot`.
fn at(body: &mut WasmFunctionEncoder, refs: RefEncoding, slot: u32, offset: i32) {
    body.instruction(&I::LocalGet(slot));
    if offset == 0 {
        refs.object(body);
    } else {
        refs.base(body);
        body.instruction(&I::I32Const(offset));
        refs.add(body);
        refs.finish(body);
    }
}

/// As `at`, adding the offset even when it is zero.
fn at_const(body: &mut WasmFunctionEncoder, refs: RefEncoding, slot: u32, offset: i32) {
    body.instruction(&I::LocalGet(slot));
    refs.base(body);
    body.instruction(&I::I32Const(offset));
    refs.add(body);
    refs.finish(body);
}

/// The address of the child slot in ADDRESS: `Byte` keeps the slot's
/// absolute address, `Scaled8` its offset inside the parent that `parent`
/// pushes.
fn child_slot(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32], parent: impl FnOnce(&mut WasmFunctionEncoder)) {
    match refs {
        RefEncoding::Byte => code(body, [I::LocalGet(s[ADDRESS]), I::I64ExtendI32U]),
        RefEncoding::Scaled8 => {
            parent(body);
            refs.base(body);
            body.instruction(&I::LocalGet(s[ADDRESS]));
            refs.add(body);
            refs.finish(body);
        }
    }
}

/// Reserve one frame before transferring ownership into it. Every instruction
/// that can trap runs here, while the parent still owns its child edge.
fn reserve(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    let frame = frame_units(refs);
    code(body, [
        I::LocalGet(s[TOP]), I::I32Const((u32::MAX - frame as u32) as i32), I::I32GtU,
        I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(s[TOP]), I::I32Const(frame), I::I32Add, I::LocalSet(s[END]),
    ]);
    at(body, refs, s[END], 0);
    code(body, [I::I64Const(65535), I::I64Add,
        I::I64Const(65536), I::I64DivU, I::I32WrapI64, I::LocalSet(s[PAGES]),
        I::LocalGet(s[PAGES]), I::I64ExtendI32U, I::MemorySize(0), I::I64GtU, I::If(BlockType::Empty),
        I::LocalGet(s[PAGES]), I::I64ExtendI32U, I::MemorySize(0), I::I64Sub, I::MemoryGrow(0),
        I::I64Const(-1), I::I64Eq, I::If(BlockType::Empty), I::Unreachable, I::End, I::End,
    ]);
}

/// Publish OBJECT/TAG/INDEX into space already reserved by `reserve`. This is
/// deliberately non-trapping so ownership transfer is failure-atomic.
fn commit_push(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    at(body, refs, s[TOP], 0);
    code(body, [I::LocalGet(s[OBJECT]), store()]);
    at(body, refs, s[TOP], 4);
    code(body, [I::LocalGet(s[TAG]), store()]);
    at(body, refs, s[TOP], 8);
    code(body, [I::LocalGet(s[INDEX]), store(),
        I::LocalGet(s[END]), I::LocalSet(s[TOP]),
        I::LocalGet(s[TOP]), I::LocalGet(s[HIGH]), I::I32GtU, I::If(BlockType::Empty),
        I::LocalGet(s[TOP]), I::LocalTee(s[HIGH]), I::GlobalSet(0), I::End,
    ]);
}

fn push(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    reserve(body, refs, s);
    commit_push(body, refs, s);
}

fn reclaim(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    at(body, refs, s[OBJECT], 0);
    code(body, [I::GlobalGet(1), store(), I::LocalGet(s[OBJECT]), I::GlobalSet(1)]);
}

fn pop(body: &mut WasmFunctionEncoder, s: &[u32]) {
    code(body, [I::LocalGet(s[FRAME]), I::LocalSet(s[TOP])]);
}

fn header(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    body.instruction(&I::LocalGet(s[OBJECT]));
    refs.base(body);
    body.instruction(&I::I32Const(8));
    refs.sub(body);
    refs.finish(body);
}

fn decrement(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    header(body, refs, s);
    code(body, [I::LocalGet(s[COUNT]), I::I32Const(1), I::I32Sub, store()]);
}

/// The frame's resume index: the parent continues at its next child.
fn advance(body: &mut WasmFunctionEncoder, refs: RefEncoding, s: &[u32]) {
    at(body, refs, s[FRAME], 8);
    code(body, [I::LocalGet(s[INDEX]), I::I32Const(1), I::I32Add, store()]);
}

pub(super) fn emit(body: &mut WasmFunctionEncoder, local: &LocalLayout, function: FunctionId,
    layouts: &LayoutInterner, root: LayoutId) -> Result<(), EncodeError>
{
    let table = rows(function, layouts, root)?;
    let ids: BTreeMap<_, _> = table.keys().enumerate().map(|(index, id)| (*id, index as i32)).collect();
    let s = &local.scratch;
    let refs = local.refs;
    if s.len() < SCRATCH { return Err(EncodeError::UnsupportedLayout { function, operation: "DropRef scratch" }); }
    memory_op_counters::emit_bump(body, memory_op_counters::Counter::Releases);
    // Static and shared roots need no temporary storage, including at the
    // memory limit. Only a last-reference graph needs the traversal stack.
    code(body, [I::LocalGet(s[OBJECT]), I::GlobalGet(2), I::I32LtU, I::If(BlockType::Empty), I::Else]);
    header(body, refs, s);
    code(body, [load(), I::LocalSet(s[COUNT]),
        I::LocalGet(s[COUNT]), I::I32Eqz, I::If(BlockType::Empty), I::Unreachable, I::End,
        I::LocalGet(s[COUNT]), I::I32Const(1), I::I32Eq, I::If(BlockType::Empty)]);
    if matches!(&table[&root], Children::Fields(fields) if fields.is_empty()) {
        reclaim(body, refs, s);
    } else {
        code(body, [I::GlobalGet(0), I::LocalTee(s[BASE]), I::LocalTee(s[TOP]), I::LocalSet(s[HIGH]),
            I::I32Const(ids[&root]), I::LocalSet(s[TAG]), I::I32Const(0), I::LocalSet(s[INDEX])]);
        push(body, refs, s);
        code(body, [I::Block(BlockType::Empty), I::Loop(BlockType::Empty),
            I::LocalGet(s[TOP]), I::LocalGet(s[BASE]), I::I32Eq, I::If(BlockType::Empty), I::Br(2), I::End,
            I::LocalGet(s[TOP]), I::I32Const(frame_units(refs)), I::I32Sub, I::LocalTee(s[FRAME])]);
        refs.object(body);
        code(body, [load(), I::LocalSet(s[OBJECT])]);
        at(body, refs, s[FRAME], 4);
        code(body, [load(), I::LocalSet(s[TAG])]);
        at(body, refs, s[FRAME], 8);
        code(body, [load(), I::LocalSet(s[INDEX]),
            I::LocalGet(s[INDEX]), I::I32Const(-1), I::I32Eq, I::If(BlockType::Empty),
            I::LocalGet(s[OBJECT]), I::GlobalGet(2), I::I32LtU, I::If(BlockType::Empty)]);
        pop(body, s);
        code(body, [I::Br(2), I::End]);
        header(body, refs, s);
        code(body, [load(), I::LocalSet(s[COUNT]),
            I::LocalGet(s[COUNT]), I::I32Eqz, I::If(BlockType::Empty), I::Unreachable, I::End,
            I::LocalGet(s[COUNT]), I::I32Const(1), I::I32GtU, I::If(BlockType::Empty)]);
        decrement(body, refs, s);
        pop(body, s);
        code(body, [I::Br(2), I::End]);
        at(body, refs, s[FRAME], 8);
        code(body, [I::I32Const(0), store(),
            I::I32Const(0), I::LocalSet(s[INDEX]), I::End]);
        // Under `Scaled8`, ADDRESS holds the child slot's offset inside the
        // parent rather than its address.
        let relative = |offset: I<'static>| -> Vec<I<'static>> {
            match refs {
                RefEncoding::Byte => vec![I::LocalGet(s[OBJECT]), offset, I::I32Add],
                RefEncoding::Scaled8 => vec![offset],
            }
        };
        for (id, children) in &table {
            code(body, [I::LocalGet(s[TAG]), I::I32Const(ids[id]), I::I32Eq, I::If(BlockType::Empty)]);
            match children {
                Children::Fields(fields) if fields.is_empty() => {
                    reclaim(body, refs, s); pop(body, s);
                    code(body, [I::Br(1), I::End]);
                    continue;
                }
                Children::Fields(fields) => {
                    code(body, [I::LocalGet(s[INDEX]), I::I32Const(fields.len() as i32), I::I32GeU, I::If(BlockType::Empty)]);
                    reclaim(body, refs, s); pop(body, s);
                    code(body, [I::Br(2), I::End, I::I32Const(-1), I::LocalSet(s[CHILD_TAG])]);
                    for (index, (offset, child, nullable)) in fields.iter().enumerate() {
                        code(body, [I::LocalGet(s[INDEX]), I::I32Const(index as i32), I::I32Eq, I::If(BlockType::Empty)]);
                        code(body, relative(I::I32Const(*offset as i32)));
                        code(body, [I::LocalSet(s[ADDRESS]),
                            I::I32Const(ids[child]), I::LocalSet(s[CHILD_TAG]),
                            I::I32Const(i32::from(*nullable)), I::LocalSet(s[PAGES]), I::End]);
                    }
                    code(body, [I::LocalGet(s[CHILD_TAG]), I::I32Const(-1), I::I32Eq,
                        I::If(BlockType::Empty), I::Unreachable, I::End]);
                }
                Children::Elements(collection, child) => {
                    at_const(body, refs, s[OBJECT], collection.length_offset as i32);
                    code(body, [load(), I::LocalSet(s[COUNT])]);
                    if let Some(capacity) = collection.capacity_offset {
                        body.instruction(&I::LocalGet(s[COUNT]));
                        at_const(body, refs, s[OBJECT], capacity as i32);
                        code(body, [load(), I::I32GtU, I::If(BlockType::Empty), I::Unreachable, I::End]);
                    }
                    code(body, [I::LocalGet(s[INDEX]), I::LocalGet(s[COUNT]), I::I32GeU, I::If(BlockType::Empty)]);
                    reclaim(body, refs, s); pop(body, s);
                    code(body, [I::Br(2), I::End]);
                    code(body, relative(I::I32Const(collection.data_offset as i32)));
                    code(body, [I::LocalGet(s[INDEX]), I::I32Const(collection.stride as i32), I::I32Mul, I::I32Add,
                        I::LocalSet(s[ADDRESS]), I::I32Const(ids[child]), I::LocalSet(s[CHILD_TAG]),
                        I::I32Const(i32::from(collection.pointer_slot == Some(PointerSlot::OwnedNullable))), I::LocalSet(s[PAGES])]);
                }
                Children::Dictionary(fields) => {
                    at(body, refs, s[OBJECT], 4);
                    code(body, [load(), I::LocalSet(s[COUNT]),
                        I::LocalGet(s[INDEX]), I::LocalGet(s[COUNT]), I::I32Const(fields.len() as i32), I::I32Mul,
                        I::I32GeU, I::If(BlockType::Empty)]);
                    reclaim(body, refs, s); pop(body, s);
                    code(body, [I::Br(2), I::End,
                        I::LocalGet(s[INDEX]), I::I32Const(fields.len() as i32), I::I32DivU,
                        I::I32Const(32), I::I32Mul]);
                    if refs == RefEncoding::Byte {
                        code(body, [I::LocalGet(s[OBJECT]), I::I32Add]);
                    }
                    code(body, [I::I32Const(20), I::I32Add, I::LocalSet(s[ADDRESS])]);
                    child_slot(body, refs, s, |body| { body.instruction(&I::LocalGet(s[OBJECT])); });
                    code(body, [load(), I::I32Const(1), I::I32Ne, I::If(BlockType::Empty)]);
                    advance(body, refs, s);
                    code(body, [I::Br(2), I::End]);
                    for (index, (offset, child)) in fields.iter().enumerate() {
                        code(body, [I::LocalGet(s[INDEX]), I::I32Const(fields.len() as i32), I::I32RemU,
                            I::I32Const(index as i32), I::I32Eq, I::If(BlockType::Empty),
                            I::LocalGet(s[ADDRESS]), I::I32Const(*offset as i32), I::I32Add,
                            I::LocalSet(s[ADDRESS]), I::I32Const(ids[child]), I::LocalSet(s[CHILD_TAG]), I::End]);
                    }
                    code(body, [I::I32Const(0), I::LocalSet(s[PAGES])]);
                }
            }
            // Transfer exactly one child to a new frame. The parent resumes
            // at its next slot only after this child has finished.
            child_slot(body, refs, s, |body| { body.instruction(&I::LocalGet(s[OBJECT])); });
            code(body, [load(), I::LocalSet(s[OBJECT]),
                I::LocalGet(s[OBJECT]), I::I32Eqz, I::LocalGet(s[PAGES]), I::I32And,
                I::If(BlockType::Empty)]);
            advance(body, refs, s);
            code(body, [I::Br(2), I::End,
                I::LocalGet(s[OBJECT]), I::I32Eqz, I::If(BlockType::Empty), I::Unreachable, I::End]);
            reserve(body, refs, s);
            advance(body, refs, s);
            // OBJECT now holds the child; the parent is still in its frame.
            child_slot(body, refs, s, |body| {
                at(body, refs, s[FRAME], 0);
                body.instruction(&load());
            });
            code(body, [I::I32Const(0), store(),
                I::LocalGet(s[CHILD_TAG]), I::LocalSet(s[TAG]), I::I32Const(-1), I::LocalSet(s[INDEX])]);
            commit_push(body, refs, s);
            code(body, [I::Br(1), I::End]);
        }
        code(body, [I::Unreachable, I::End, I::End]);
        // Restore the virgin-zero property of memory above the bump
        // frontier before a later ordinary allocation can reuse it.
        at(body, refs, s[BASE], 0);
        code(body, [I::I32Const(0), I::LocalGet(s[HIGH]), I::LocalGet(s[BASE]), I::I32Sub]);
        refs.object(body);
        code(body, [I::MemoryFill(0), I::LocalGet(s[BASE]), I::GlobalSet(0)]);
    }
    code(body, [I::Else]);
    decrement(body, refs, s);
    code(body, [I::End, I::End]);
    Ok(())
}

// Expected instruction mapping is authored separately from `emit`; it never
// invokes the encoder or decodes its output to invent an expected plan.
fn plan_at(p: &mut Vec<PlanToken>, refs: RefEncoding, slot: u32, offset: i32) {
    p.push(PlanToken::LocalGet(slot));
    if offset == 0 {
        refs.plan_object(p);
    } else {
        refs.plan_base(p);
        p.push(PlanToken::I32Const(offset));
        refs.plan_add(p);
        refs.plan_finish(p);
    }
}

fn plan_at_const(p: &mut Vec<PlanToken>, refs: RefEncoding, slot: u32, offset: i32) {
    p.push(PlanToken::LocalGet(slot));
    refs.plan_base(p);
    p.push(PlanToken::I32Const(offset));
    refs.plan_add(p);
    refs.plan_finish(p);
}

fn plan_child_slot(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32], parent: impl FnOnce(&mut Vec<PlanToken>)) {
    match refs {
        RefEncoding::Byte => p.extend([PlanToken::LocalGet(s[ADDRESS]), PlanToken::I64ExtendI32U]),
        RefEncoding::Scaled8 => {
            parent(p);
            refs.plan_base(p);
            p.push(PlanToken::LocalGet(s[ADDRESS]));
            refs.plan_add(p);
            refs.plan_finish(p);
        }
    }
}

fn plan_reserve(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    use PlanToken as P;
    let frame = match refs { RefEncoding::Byte => 12, RefEncoding::Scaled8 => 2 };
    p.extend([
        P::LocalGet(s[TOP]), P::I32Const((u32::MAX - frame as u32) as i32), P::I32GtU,
        P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(s[TOP]), P::I32Const(frame), P::I32Add, P::LocalSet(s[END]),
    ]);
    plan_at(p, refs, s[END], 0);
    p.extend([P::I64Const(65535), P::I64Add,
        P::I64Const(65536), P::I64DivU, P::I32WrapI64, P::LocalSet(s[PAGES]),
        P::LocalGet(s[PAGES]), P::I64ExtendI32U, P::MemorySize, P::I64GtU, P::If("Empty".into()),
        P::LocalGet(s[PAGES]), P::I64ExtendI32U, P::MemorySize, P::I64Sub, P::MemoryGrow,
        P::I64Const(-1), P::I64Eq, P::If("Empty".into()), P::Unreachable, P::End, P::End,
    ]);
}

fn plan_commit_push(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    use PlanToken as P;
    plan_at(p, refs, s[TOP], 0);
    p.extend([P::LocalGet(s[OBJECT]), pstore()]);
    plan_at(p, refs, s[TOP], 4);
    p.extend([P::LocalGet(s[TAG]), pstore()]);
    plan_at(p, refs, s[TOP], 8);
    p.extend([P::LocalGet(s[INDEX]), pstore(),
        P::LocalGet(s[END]), P::LocalSet(s[TOP]),
        P::LocalGet(s[TOP]), P::LocalGet(s[HIGH]), P::I32GtU, P::If("Empty".into()),
        P::LocalGet(s[TOP]), P::LocalTee(s[HIGH]), P::GlobalSet(0), P::End,
    ]);
}

fn plan_push(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    plan_reserve(p, refs, s);
    plan_commit_push(p, refs, s);
}

fn pload() -> PlanToken { PlanToken::I32Load { offset: 0, align: 2, memory: 0 } }
fn pstore() -> PlanToken { PlanToken::I32Store { offset: 0, align: 2, memory: 0 } }
fn plan_reclaim(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    plan_at(p, refs, s[OBJECT], 0);
    p.extend([PlanToken::GlobalGet(1), pstore(), PlanToken::LocalGet(s[OBJECT]), PlanToken::GlobalSet(1)]);
}
fn plan_pop(p: &mut Vec<PlanToken>, s: &[u32]) {
    p.extend([PlanToken::LocalGet(s[FRAME]), PlanToken::LocalSet(s[TOP])]);
}
fn plan_header(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    p.push(PlanToken::LocalGet(s[OBJECT]));
    refs.plan_base(p);
    p.push(PlanToken::I32Const(8));
    refs.plan_sub(p);
    refs.plan_finish(p);
}
fn plan_decrement(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    use PlanToken as P;
    plan_header(p, refs, s);
    p.extend([P::LocalGet(s[COUNT]), P::I32Const(1), P::I32Sub, pstore()]);
}
fn plan_advance(p: &mut Vec<PlanToken>, refs: RefEncoding, s: &[u32]) {
    use PlanToken as P;
    plan_at(p, refs, s[FRAME], 8);
    p.extend([P::LocalGet(s[INDEX]), P::I32Const(1), P::I32Add, pstore()]);
}

pub(super) fn plan(p: &mut Vec<PlanToken>, function: FunctionId, layouts: &LayoutInterner,
    local: &LocalLayout, root: LayoutId) -> Result<(), CanonicalAdmissionError>
{
    use PlanToken as P;
    let table = rows(function, layouts, root).map_err(|_| CanonicalAdmissionError::UnsupportedPlan)?;
    let ids: BTreeMap<_, _> = table.keys().enumerate().map(|(index, id)| (*id, index as i32)).collect();
    let s = &local.scratch;
    let refs = local.refs;
    let frame = match refs { RefEncoding::Byte => 12, RefEncoding::Scaled8 => 2 };
    if s.len() < SCRATCH { return Err(CanonicalAdmissionError::UnsupportedPlan); }
    memory_op_counters::plan_bump(p, memory_op_counters::Counter::Releases);
    p.extend([P::LocalGet(s[OBJECT]), P::GlobalGet(2), P::I32LtU, P::If("Empty".into()), P::Else]);
    plan_header(p, refs, s);
    p.extend([pload(), P::LocalSet(s[COUNT]),
        P::LocalGet(s[COUNT]), P::I32Eqz, P::If("Empty".into()), P::Unreachable, P::End,
        P::LocalGet(s[COUNT]), P::I32Const(1), P::I32Eq, P::If("Empty".into())]);
    if matches!(&table[&root], Children::Fields(fields) if fields.is_empty()) {
        plan_reclaim(p, refs, s);
    } else {
        p.extend([P::GlobalGet(0), P::LocalTee(s[BASE]), P::LocalTee(s[TOP]), P::LocalSet(s[HIGH]),
            P::I32Const(ids[&root]), P::LocalSet(s[TAG]), P::I32Const(0), P::LocalSet(s[INDEX])]);
        plan_push(p, refs, s);
        p.extend([P::Block("Empty".into()), P::Loop("Empty".into()),
            P::LocalGet(s[TOP]), P::LocalGet(s[BASE]), P::I32Eq, P::If("Empty".into()), P::Br(2), P::End,
            P::LocalGet(s[TOP]), P::I32Const(frame), P::I32Sub, P::LocalTee(s[FRAME])]);
        refs.plan_object(p);
        p.extend([pload(), P::LocalSet(s[OBJECT])]);
        plan_at(p, refs, s[FRAME], 4);
        p.extend([pload(), P::LocalSet(s[TAG])]);
        plan_at(p, refs, s[FRAME], 8);
        p.extend([pload(), P::LocalSet(s[INDEX]),
            P::LocalGet(s[INDEX]), P::I32Const(-1), P::I32Eq, P::If("Empty".into()),
            P::LocalGet(s[OBJECT]), P::GlobalGet(2), P::I32LtU, P::If("Empty".into())]);
        plan_pop(p, s);
        p.extend([P::Br(2), P::End]);
        plan_header(p, refs, s);
        p.extend([pload(), P::LocalSet(s[COUNT]),
            P::LocalGet(s[COUNT]), P::I32Eqz, P::If("Empty".into()), P::Unreachable, P::End,
            P::LocalGet(s[COUNT]), P::I32Const(1), P::I32GtU, P::If("Empty".into())]);
        plan_decrement(p, refs, s); plan_pop(p, s);
        p.extend([P::Br(2), P::End]);
        plan_at(p, refs, s[FRAME], 8);
        p.extend([P::I32Const(0), pstore(), P::I32Const(0), P::LocalSet(s[INDEX]), P::End]);
        let object = |p: &mut Vec<PlanToken>| if refs == RefEncoding::Byte {
            p.extend([P::LocalGet(s[OBJECT]), P::I32Add]);
        };
        for (id, children) in &table {
            p.extend([P::LocalGet(s[TAG]), P::I32Const(ids[id]), P::I32Eq, P::If("Empty".into())]);
            match children {
                Children::Fields(fields) if fields.is_empty() => {
                    plan_reclaim(p, refs, s); plan_pop(p, s);
                    p.extend([P::Br(1), P::End]);
                    continue;
                }
                Children::Fields(fields) => {
                    p.extend([P::LocalGet(s[INDEX]), P::I32Const(fields.len() as i32), P::I32GeU, P::If("Empty".into())]);
                    plan_reclaim(p, refs, s); plan_pop(p, s);
                    p.extend([P::Br(2), P::End, P::I32Const(-1), P::LocalSet(s[CHILD_TAG])]);
                    for (index, (offset, child, nullable)) in fields.iter().enumerate() {
                        p.extend([P::LocalGet(s[INDEX]), P::I32Const(index as i32), P::I32Eq, P::If("Empty".into())]);
                        // Byte: OBJECT + offset; Scaled8: the offset alone.
                        if refs == RefEncoding::Byte {
                            p.extend([P::LocalGet(s[OBJECT]), P::I32Const(*offset as i32), P::I32Add]);
                        } else {
                            p.push(P::I32Const(*offset as i32));
                        }
                        p.extend([P::LocalSet(s[ADDRESS]),
                            P::I32Const(ids[child]), P::LocalSet(s[CHILD_TAG]),
                            P::I32Const(i32::from(*nullable)), P::LocalSet(s[PAGES]), P::End]);
                    }
                    p.extend([P::LocalGet(s[CHILD_TAG]), P::I32Const(-1), P::I32Eq,
                        P::If("Empty".into()), P::Unreachable, P::End]);
                }
                Children::Elements(collection, child) => {
                    plan_at_const(p, refs, s[OBJECT], collection.length_offset as i32);
                    p.extend([pload(), P::LocalSet(s[COUNT])]);
                    if let Some(capacity) = collection.capacity_offset {
                        p.push(P::LocalGet(s[COUNT]));
                        plan_at_const(p, refs, s[OBJECT], capacity as i32);
                        p.extend([pload(), P::I32GtU, P::If("Empty".into()), P::Unreachable, P::End]);
                    }
                    p.extend([P::LocalGet(s[INDEX]), P::LocalGet(s[COUNT]), P::I32GeU, P::If("Empty".into())]);
                    plan_reclaim(p, refs, s); plan_pop(p, s);
                    p.extend([P::Br(2), P::End]);
                    if refs == RefEncoding::Byte {
                        p.extend([P::LocalGet(s[OBJECT]), P::I32Const(collection.data_offset as i32), P::I32Add]);
                    } else {
                        p.push(P::I32Const(collection.data_offset as i32));
                    }
                    p.extend([P::LocalGet(s[INDEX]), P::I32Const(collection.stride as i32), P::I32Mul, P::I32Add,
                        P::LocalSet(s[ADDRESS]), P::I32Const(ids[child]), P::LocalSet(s[CHILD_TAG]),
                        P::I32Const(i32::from(collection.pointer_slot == Some(PointerSlot::OwnedNullable))), P::LocalSet(s[PAGES])]);
                }
                Children::Dictionary(fields) => {
                    plan_at(p, refs, s[OBJECT], 4);
                    p.extend([pload(), P::LocalSet(s[COUNT]),
                        P::LocalGet(s[INDEX]), P::LocalGet(s[COUNT]), P::I32Const(fields.len() as i32), P::I32Mul,
                        P::I32GeU, P::If("Empty".into())]);
                    plan_reclaim(p, refs, s); plan_pop(p, s);
                    p.extend([P::Br(2), P::End,
                        P::LocalGet(s[INDEX]), P::I32Const(fields.len() as i32), P::I32DivU,
                        P::I32Const(32), P::I32Mul]);
                    object(p);
                    p.extend([P::I32Const(20), P::I32Add, P::LocalSet(s[ADDRESS])]);
                    plan_child_slot(p, refs, s, |p| p.push(P::LocalGet(s[OBJECT])));
                    p.extend([pload(), P::I32Const(1), P::I32Ne, P::If("Empty".into())]);
                    plan_advance(p, refs, s);
                    p.extend([P::Br(2), P::End]);
                    for (index, (offset, child)) in fields.iter().enumerate() {
                        p.extend([P::LocalGet(s[INDEX]), P::I32Const(fields.len() as i32), P::I32RemU,
                            P::I32Const(index as i32), P::I32Eq, P::If("Empty".into()),
                            P::LocalGet(s[ADDRESS]), P::I32Const(*offset as i32), P::I32Add,
                            P::LocalSet(s[ADDRESS]), P::I32Const(ids[child]), P::LocalSet(s[CHILD_TAG]), P::End]);
                    }
                    p.extend([P::I32Const(0), P::LocalSet(s[PAGES])]);
                }
            }
            plan_child_slot(p, refs, s, |p| p.push(P::LocalGet(s[OBJECT])));
            p.extend([pload(), P::LocalSet(s[OBJECT]),
                P::LocalGet(s[OBJECT]), P::I32Eqz, P::LocalGet(s[PAGES]), P::I32And,
                P::If("Empty".into())]);
            plan_advance(p, refs, s);
            p.extend([P::Br(2), P::End,
                P::LocalGet(s[OBJECT]), P::I32Eqz, P::If("Empty".into()), P::Unreachable, P::End]);
            plan_reserve(p, refs, s);
            plan_advance(p, refs, s);
            plan_child_slot(p, refs, s, |p| {
                plan_at(p, refs, s[FRAME], 0);
                p.push(pload());
            });
            p.extend([P::I32Const(0), pstore(),
                P::LocalGet(s[CHILD_TAG]), P::LocalSet(s[TAG]), P::I32Const(-1), P::LocalSet(s[INDEX])]);
            plan_commit_push(p, refs, s);
            p.extend([P::Br(1), P::End]);
        }
        p.extend([P::Unreachable, P::End, P::End]);
        plan_at(p, refs, s[BASE], 0);
        p.extend([P::I32Const(0), P::LocalGet(s[HIGH]), P::LocalGet(s[BASE]), P::I32Sub]);
        refs.plan_object(p);
        p.extend([P::MemoryFill(0), P::LocalGet(s[BASE]), P::GlobalSet(0)]);
    }
    p.push(P::Else);
    plan_decrement(p, refs, s);
    p.extend([P::End, P::End]);
    Ok(())
}

#[cfg(all(feature = "raw-encoder-test-api", feature = "native"))]
#[doc(hidden)]
pub mod self_checks {
    use super::*;

    // A mechanical-kernel fixture, not recursive production admission. The
    // latter remains sealed until nullable ownership and checked source
    // binding land. A recursive counted list terminates in an empty list.
    fn fixture(heap: &[u8]) -> (wasmtime::Store<()>, wasmtime::Instance, wasmtime::Memory) {
        fixture_with_slot(heap, PointerSlot::Owned)
    }

    fn fixture_with_slot(
        heap: &[u8],
        semantics: PointerSlot,
    ) -> (wasmtime::Store<()>, wasmtime::Instance, wasmtime::Memory) {
        let mut layouts = LayoutInterner::new();
        let node = layouts.intern_opaque_nominal([23; 32]).unwrap();
        let slot = layouts.intern_runtime_pointer_value_slot(node, semantics).unwrap();
        let body_layout = layouts.intern_packed_list(slot, RcHeader::Required).unwrap();
        layouts.bind_nominal(node, body_layout).unwrap();
        let frontier = 32 + heap.len() as u32;
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32], []);
        let mut functions = FunctionSection::new();
        functions.function(0);
        let mut memories = MemorySection::new();
        memories.memory(MemoryType { minimum: u64::from(frontier).div_ceil(65536), maximum: Some(64),
            memory64: true, shared: false, page_size_log2: None });
        let mut globals = GlobalSection::new();
        for value in [frontier as i32, 0, 32] {
            globals.global(GlobalType { val_type: ValType::I32, mutable: true, shared: false }, &ConstExpr::i32_const(value));
        }
        let mut exports = ExportSection::new();
        exports.export("drop", ExportKind::Func, 0);
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("frontier", ExportKind::Global, 0);
        exports.export("free", ExportKind::Global, 1);
        let local = LocalLayout { refs: ref_encoding::RefEncoding::Byte, memory_helpers: None,
            owner_scratch: None,
            owner_bridges: Vec::new(),
            indices: BTreeMap::new(),
            extra_types: Vec::new(),
            scratch: (1..=SCRATCH as u32).collect(),
            owner_context: Default::default(), wide: Vec::new(),
        };
        let mut body = WasmFunctionEncoder::new([(SCRATCH as u32, ValType::I32)]);
        code(&mut body, [I::LocalGet(0), I::LocalSet(local.scratch[OBJECT])]);
        emit(&mut body, &local, FunctionId::new(0), &layouts, node).unwrap();
        body.instruction(&I::End);
        let mut bodies = CodeSection::new();
        bodies.function(&body);
        let mut module = WasmModule::new();
        module.section(&types).section(&functions).section(&memories).section(&globals).section(&exports).section(&bodies);
        let bytes = module.finish();
        wasmparser::Validator::new().validate_all(&bytes).expect("structurally valid destructor");
        let mut config = wasmtime::Config::new();
        config.max_wasm_stack(64 * 1024).wasm_memory64(true).memory_may_move(false);
        let engine = wasmtime::Engine::new(&config).unwrap();
        let module = wasmtime::Module::new(&engine, bytes).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        memory.write(&mut store, 32, heap).unwrap();
        (store, instance, memory)
    }

    fn put(bytes: &mut [u8], at: usize, value: u32) { bytes[at..at + 4].copy_from_slice(&value.to_le_bytes()); }
    fn word(bytes: &[u8], at: usize) -> u32 { u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) }
    fn global(instance: &wasmtime::Instance, store: &mut wasmtime::Store<()>, name: &str) -> u32 {
        instance.get_global(&mut *store, name).unwrap().get(store).i32().unwrap() as u32
    }

    pub fn drop_worklist_releases_25000_nodes_without_call_stack_growth() {
        let count = 25_000;
        let mut heap = vec![0; count * 24];
        for index in 0..count {
            let at = index * 24;
            put(&mut heap, at, 1);
            put(&mut heap, at + 4, 16);
            put(&mut heap, at + 8, u32::from(index + 1 < count));
            put(&mut heap, at + 12, 1);
            if index + 1 < count { put(&mut heap, at + 16, (32 + at + 24 + 8) as u32); }
        }
        let (mut store, instance, memory) = fixture(&heap);
        let pages = memory.size(&store);
        instance.get_typed_func::<i32, ()>(&mut store, "drop").unwrap().call(&mut store, 40).unwrap();
        let frontier = 32 + heap.len();
        assert_eq!(global(&instance, &mut store, "frontier"), frontier as u32);
        assert!(memory.size(&store) > pages, "deep worklist must exercise memory growth");
        let mut free = global(&instance, &mut store, "free");
        let data = memory.data(&store);
        for index in 0..count {
            assert_eq!(free, (40 + index * 24) as u32, "child-before-parent free order");
            free = word(data, free as usize);
        }
        assert_eq!(free, 0);
        assert!(data[frontier..frontier + count * 12].iter().all(|byte| *byte == 0), "temporary frames must be cleared");
    }



}
