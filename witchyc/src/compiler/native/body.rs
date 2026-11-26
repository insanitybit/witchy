#[cfg(witchy_native_aws_lc)]
mod agreement;
mod calls;
mod cells;
mod const_div;
mod expression;
mod memory;
mod numbers;
mod packed;
mod shared;
mod slices;
mod storage;
mod boxes;
mod sums;
mod tasks;

use std::collections::{BTreeMap, BTreeSet};

use super::{Function, Result, error, layout::Layout};
use crate::compiler::{
    logical_body::{Binary, Expr, Function as LogicalFunction, Stmt, Ty, Value as Logical},
    logical_place::{Place, Projection},
};
use cranelift_codegen::ir::{
    Block, InstBuilder, MemFlagsData, StackSlot, StackSlotData, StackSlotKind, TrapCode, Type,
    Value,
    condcodes::IntCC,
    types::{F64, F64X2, I64},
};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::{DataId, FuncId, Module};
use cranelift_object::ObjectModule;

const TRAP: TrapCode = TrapCode::unwrap_user(1);
pub(super) const LANE_BYTES: u32 = 8;
pub(super) const MAX_FRAME_BYTES: u32 = 64 * 1024;

#[derive(Clone, Copy)]
enum Local {
    Unit,
    Stack(StackSlot, Ty),
    Scalar(Variable, Type),
    FloatPair(Variable),
    Words([Variable; 2]),
}

#[derive(Clone, Copy)]
enum Location {
    Address(Value, Ty),
    Scalar(Variable, Type),
    FloatPair(Variable, Option<u8>),
    Words([Variable; 2], Option<usize>),
}

struct Body<'a, 'b> {
    fb: FunctionBuilder<'a>,
    module: &'b mut ObjectModule,
    literals: &'b mut BTreeMap<std::sync::Arc<str>, DataId>,
    functions: &'b [Function],
    abi: &'b Function,
    print: FuncId,
    layout: &'b Layout<'b>,
    locals: Vec<Local>,
    return_slot: Option<Value>,
    entry_grants: Option<Value>,
    frame_bytes: u32,
    requires_aws_lc: bool,
    steps: &'b BTreeSet<u32>,
    shared_params: BTreeSet<u32>,
    reads: BTreeMap<shared::Field, Value>,
    heap: cranelift_codegen::ir::GlobalValue,
    offsets: BTreeMap<(Block, Value, Value, u32), Value>,
    quotients: BTreeMap<(Block, Value, u64), Value>,
}

pub(super) struct Lowered {
    pub frame_bytes: u32,
    pub requires_aws_lc: bool,
    /// Parameters the loan rule describes, for `alias.rs`.
    pub roots: Vec<(Value, super::alias::Root)>,
}

pub(super) fn lower(
    mut fb: FunctionBuilder<'_>,
    module: &mut ObjectModule,
    literals: &mut BTreeMap<std::sync::Arc<str>, DataId>,
    functions: &[Function],
    print: FuncId,
    layout: &Layout<'_>,
    function: &LogicalFunction,
    abi: &Function,
    entry_function: bool,
) -> Result<Lowered> {
    let entry = fb.create_block();
    fb.append_block_params_for_function_params(entry);
    fb.switch_to_block(entry);
    let mut args = fb.block_params(entry).to_vec().into_iter();
    // The runtime's heap is every function's first external name, so it is the same name at the
    // same index in each: a callee written in by the CLIF inliner, which copies a symbol's name
    // as it is, still names it (native/inline.rs).
    let heap_data = module.declare_data("witchy_heap", cranelift_module::Linkage::Import, true, false).map_err(error)?;
    let heap = module.declare_data_in_func(heap_data, fb.func);
    let return_slot = if abi.indirect_result() {
        args.next()
    } else {
        None
    };
    let mut body = Body {
        fb,
        module,
        literals,
        functions,
        abi,
        print,
        layout,
        locals: Vec::with_capacity(function.locals.len()),
        return_slot,
        entry_grants: None,
        frame_bytes: 0,
        requires_aws_lc: false,
        steps: &function.steps,
        shared_params: function
            .locals
            .iter()
            .take(function.params.len())
            .filter(|l| {
                matches!(l.ty, Ty::Borrow { mutable: false, pointee } if layout.is_plain_data(pointee.ty()))
                    && !l.mutable
                    && !function.addressed.contains(&l.id)
            })
            .map(|l| l.id)
            .collect(),
        reads: BTreeMap::new(),
        heap,
        offsets: BTreeMap::new(),
        quotients: BTreeMap::new(),
    };
    for local in &function.locals {
        let count = layout.lanes(local.ty)?;
        let storage = if count == 0 {
            Local::Unit
        } else if count == 1
            && matches!(
                local.ty,
                Ty::Unsigned(_) | Ty::Bool | Ty::I64 | Ty::F64 | Ty::Borrow { .. }
            )
            && !function.addressed.contains(&local.id)
        {
            // Keep the same storage budget regardless of register allocation.
            body.charge_frame(count)?;
            let ty = if local.ty == Ty::F64 { F64 } else { I64 };
            Local::Scalar(body.fb.declare_var(ty), ty)
        } else if !function.addressed.contains(&local.id) && layout.float_pair(local.ty)? {
            body.charge_frame(count)?;
            Local::FloatPair(body.fb.declare_var(F64X2))
        } else if !function.addressed.contains(&local.id)
            && (matches!(local.ty, Ty::Borrow { pointee: crate::compiler::logical_body::Pointee::Slice(_), .. })
                || layout.scalar_pair(local.ty)?)
        {
            body.charge_frame(count)?;
            Local::Words([body.fb.declare_var(I64), body.fb.declare_var(I64)])
        } else {
            Local::Stack(body.slot(local.ty)?, local.ty)
        };
        body.locals.push(storage);
    }
    let mut roots = Vec::new();
    for (i, ty) in function.params.iter().enumerate() {
        if layout.float_pair(*ty)? {
            let vector = args.next().ok_or_else(|| error("missing float pair argument"))?;
            if let Local::FloatPair(var) = body.locals[i] {
                body.fb.def_var(var, vector);
            } else {
                let words = body.float_pair_words(vector);
                body.set(i as u32, &words)?;
            }
            continue;
        }
        let values = args
            .by_ref()
            .take(layout.lanes(*ty)? as usize)
            .collect::<Vec<_>>();
        if let ([value], Some(root)) = (values.as_slice(), alias_root(layout, *ty)?) {
            roots.push((*value, root));
        }
        body.set(i as u32, &values)?;
    }
    // The synthetic logical EntryConsole assignment is emitted below using this value.
    if entry_function {
        body.entry_grants = args.next();
    }
    let config = body.module.isa().frontend_config();
    if body.block(&function.body, None)? {
        if function.result != Ty::Unit {
            return Err(error("function falls through without a value"));
        }
        body.fb.ins().return_(&[]);
    }
    body.fb.seal_all_blocks();
    body.fb.finalize(config);
    Ok(Lowered { frame_bytes: body.frame_bytes, requires_aws_lc: body.requires_aws_lc, roots })
}

/// A parameter `alias.rs` may trust: a borrow of a record or fixed array whose fields are plain
/// data or dense buffers of plain data, with the byte offsets of those buffers' owner fields.
fn alias_root(layout: &Layout, ty: Ty) -> Result<Option<super::alias::Root>> {
    let Ty::Borrow { mutable, pointee: crate::compiler::logical_body::Pointee::Aggregate(id) } = ty else {
        return Ok(None);
    };
    let record = Ty::Aggregate(id);
    let mut owners = std::collections::BTreeSet::new();
    let count = layout.aggregate(record)?.fields.len() as u32;
    for ordinal in 0..count {
        let field = layout.field(record, ordinal)?;
        match field.ty {
            Ty::Buffer(buffer) => {
                let Some(described) = layout.types.buffers().get(buffer as usize) else { return Ok(None) };
                if !described.dense || !layout.is_plain_data(described.element) {
                    return Ok(None);
                }
                owners.insert(i64::from(field.byte_offset));
            }
            other if layout.is_plain_data(other) => {}
            _ => return Ok(None),
        }
    }
    Ok(Some(super::alias::Root { mutable, owners }))
}

impl Body<'_, '_> {
    fn switch_to(&mut self, block: Block) {
        self.reads.clear();
        self.offsets.clear();
        self.quotients.clear();
        self.fb.switch_to_block(block);
    }

    fn charge_frame(&mut self, lanes: u32) -> Result<u32> {
        let bytes = lanes * LANE_BYTES;
        self.frame_bytes += bytes;
        if self.frame_bytes > MAX_FRAME_BYTES {
            return Err(error("function exceeds the 64 KiB native frame limit"));
        }
        Ok(bytes)
    }

    fn slot(&mut self, ty: Ty) -> Result<StackSlot> {
        self.charge_frame(self.layout.lanes(ty)?)?;
        let storage = self.layout.storage(ty)?;
        Ok(self.fb.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            storage.size.max(1),
            storage.align.trailing_zeros() as u8,
        )))
    }

    fn scratch(&mut self, ty: Ty) -> Result<Value> {
        let slot = self.slot(ty)?;
        Ok(self.fb.ins().stack_addr(I64, slot, 0))
    }

    fn entry_grant(&mut self, bit: i64) -> Result<Value> {
        let mask = self.entry_grants.ok_or_else(|| error("host grant outside native entry"))?;
        Ok(self.fb.ins().band_imm_s(mask, bit))
    }

    fn local_address(&mut self, id: u32) -> Result<Value> {
        let slot = match self.locals[id as usize] {
            Local::Stack(slot, _) => slot,
            Local::Unit => return Err(error("cannot address a unit local")),
            Local::Scalar(..) | Local::FloatPair(_) | Local::Words(..) => {
                return Err(error("local address was not recorded by the checker"));
            }
        };
        Ok(self.fb.ins().stack_addr(I64, slot, 0))
    }

    fn load_word(&mut self, addr: Value) -> Value {
        self.fb.ins().load(I64, MemFlagsData::new(), addr, 0)
    }

    fn read_location(&mut self, location: Location) -> Result<Vec<Value>> {
        match location {
            Location::Address(addr, ty) => self.load_expression(addr, ty),
            Location::Scalar(var, _) => Ok(vec![self.fb.use_var(var)]),
            Location::Words(words, field) => Ok(match field {
                Some(field) => vec![self.fb.use_var(words[field])],
                None => words.map(|word| self.fb.use_var(word)).to_vec(),
            }),
            Location::FloatPair(var, lane) => {
                let vector = self.fb.use_var(var);
                Ok(match lane {
                    Some(lane) => vec![self.fb.ins().extractlane(vector, lane)],
                    None => self.float_pair_words(vector),
                })
            }
        }
    }

    fn write_location(&mut self, location: Location, values: &[Value]) -> Result<()> {
        match location {
            Location::Address(addr, ty) => self.store_typed(addr, ty, values),
            Location::Words(words, field) => {
                let words: &[Variable] = match field {
                    Some(field) => &words[field..=field],
                    None => &words,
                };
                for (&word, &value) in words.iter().zip(values) {
                    let bits = self.bitcast_to(value, I64);
                    self.fb.def_var(word, bits);
                }
                Ok(())
            }
            Location::FloatPair(var, lane) => {
                let value = match lane {
                    Some(lane) => {
                        let vector = self.fb.use_var(var);
                        let field = self.bitcast_to(values[0], F64);
                        self.fb.ins().insertlane(vector, field, lane)
                    }
                    None => self.pack_float_values(values),
                };
                self.fb.def_var(var, value);
                Ok(())
            }
            Location::Scalar(var, ty) => {
                let value = self.bitcast_to(values[0], ty);
                self.fb.def_var(var, value);
                Ok(())
            }
        }
    }

    fn set(&mut self, local: u32, values: &[Value]) -> Result<()> {
        self.forget_local(local);
        let location = match self.locals[local as usize] {
            Local::Unit => return Ok(()),
            Local::Stack(_, ty) => Location::Address(self.local_address(local)?, ty),
            Local::Scalar(var, ty) => Location::Scalar(var, ty),
            Local::FloatPair(var) => Location::FloatPair(var, None),
            Local::Words(words) => Location::Words(words, None),
        };
        self.write_location(location, values)
    }

    fn locate(&mut self, place: &Place) -> Result<Location> {
        if !place.pointer
            && place.projections.is_empty()
            && let Local::Scalar(var, ty) = self.locals[place.root as usize]
        {
            return Ok(Location::Scalar(var, ty));
        }
        if !place.pointer && let Local::Words(words) = self.locals[place.root as usize] {
            let field = match place.projections.as_slice() {
                [] => None,
                [Projection::Field { ordinal }] => Some(*ordinal as usize),
                _ => return Err(error("invalid projection from a native word pair")),
            };
            return Ok(Location::Words(words, field));
        }
        if !place.pointer && let Local::FloatPair(var) = self.locals[place.root as usize] {
            let lane = match place.projections.as_slice() {
                [] => None,
                [Projection::Field { ordinal }] => Some(*ordinal as u8),
                _ => return Err(error("invalid projection from a native float pair")),
            };
            return Ok(Location::FloatPair(var, lane));
        }
        Ok(Location::Address(self.place(place)?, place.ty))
    }

    fn place(&mut self, place: &Place) -> Result<Value> {
        if matches!(place.root_ty, Ty::Slice(_)) {
            return Err(error(
                "slice storage must be accessed with indexing or len()",
            ));
        }
        let mut addr = match (place.pointer, self.locals[place.root as usize]) {
            (true, Local::Scalar(var, _)) => self.fb.use_var(var),
            (true, _) => {
                let slot = self.local_address(place.root)?;
                self.load_word(slot)
            }
            (false, _) => self.local_address(place.root)?,
        };
        let mut ty = place.root_ty;
        for projection in &place.projections {
            match projection {
                Projection::Field { ordinal } => {
                    let field = self.layout.field(ty, *ordinal)?;
                    addr = self.fb.ins().iadd_imm_s(addr, i64::from(field.byte_offset));
                    ty = field.ty;
                }
                Projection::Index { value, length } => {
                    let index = self.one(value)?;
                    let len = (!self.abi.proved_array_bounds)
                        .then(|| self.fb.ins().iconst(I64, i64::from(*length)));
                    ty = self.layout.array(ty, *length)?;
                    addr = self.index_address(addr, index, len, ty)?;
                }
            }
        }
        Ok(addr)
    }

    fn index_address(
        &mut self,
        addr: Value,
        index: Value,
        checked_length: Option<Value>,
        ty: Ty,
    ) -> Result<Value> {
        if let Some(len) = checked_length {
            let past = self
                .fb
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
            self.fb.ins().trapnz(past, TRAP);
        }
        let stride = self.layout.storage(ty)?.size;
        Ok(self.offset(addr, index, stride))
    }

    // Scalar SSA uses the native register class. Calls and flattened payloads
    // carry uninterpreted 64-bit lanes.
    fn bitcast_to(&mut self, value: Value, ty: Type) -> Value {
        if self.fb.func.dfg.value_type(value) == ty {
            value
        } else {
            self.fb.ins().bitcast(ty, MemFlagsData::new(), value)
        }
    }

    fn abi_lanes(&mut self, expr: &Expr) -> Result<Vec<Value>> {
        let mut lanes = self.expr(expr)?;
        if expr.ty == Ty::F64 {
            lanes[0] = self.bitcast_to(lanes[0], I64);
        }
        Ok(lanes)
    }

    fn one(&mut self, expr: &Expr) -> Result<Value> {
        match self.expr(expr)?.as_slice() {
            [value] if expr.ty == Ty::F64 => Ok(self.bitcast_to(*value, F64)),
            [value] => Ok(*value),
            _ => Err(error("expected one native value lane")),
        }
    }

    fn block(&mut self, statements: &[Stmt], loop_blocks: Option<(Block, Block)>) -> Result<bool> {
        for statement in statements {
            match statement {
                Stmt::Set(local, value) => {
                    if let Local::FloatPair(var) = self.locals[*local as usize] {
                        let vector = self.float_pair_expr(value)?;
                        self.fb.def_var(var, vector);
                        continue;
                    }
                    if matches!(self.locals[*local as usize], Local::Stack(..))
                        && !self.layout.float_pair(value.ty)?
                        && matches!(value.ty, Ty::Aggregate(_))
                        && self.layout.lanes(value.ty)? > super::DIRECT_RESULT_WORDS
                        && let Logical::Call(call) = &value.kind
                        && self.has_plain_arguments(call)
                    {
                        // All inputs are flattened values. The internal callee can reach
                        // the destination only via its final return stores.
                        let dest = self.local_address(*local)?;
                        self.emit_call(call, Some(dest))?;
                        continue;
                    }
                    let values = self.expr(value)?;
                    self.set(*local, &values)?;
                }
                Stmt::StorePlace(place, _, value) | Stmt::Replace(_, place, value) => {
                    let op = match statement {
                        Stmt::StorePlace(_, op, _) => *op,
                        _ => None,
                    };
                    let location = self.locate(place)?;
                    if op.is_none() && let Location::FloatPair(var, None) = location {
                        let vector = self.float_pair_expr(value)?;
                        self.fb.def_var(var, vector);
                        continue;
                    }
                    let values = if let Some(op) = op {
                        let old = self.read_location(location)?[0];
                        let step = matches!(op, Binary::Add) && place.projections.is_empty() && !place.pointer
                            && self.steps.contains(&place.root) && place.ty == Ty::U64
                            && matches!(value.kind, Logical::U64(1));
                        let right = self.one(value)?;
                        vec![self.binary_typed(op, old, right, place.ty, step)]
                    } else {
                        self.expr(value)?
                    };
                    if place.pointer {
                        self.reads.clear();
                    } else {
                        self.forget_local(place.root);
                    }
                    self.write_location(location, &values)?;
                }
                Stmt::Eval(value) => {
                    self.expr(value)?;
                }
                Stmt::Expire(ids) => {
                    for id in ids {
                        self.forget_local(*id);
                    }
                }
                Stmt::Return(value, _, _) => {
                    self.emit_return(value)?;
                    return Ok(false);
                }
                Stmt::If(condition, yes, no) => {
                    let (then, otherwise, join) = (
                        self.fb.create_block(),
                        self.fb.create_block(),
                        self.fb.create_block(),
                    );
                    self.branch(condition, then, otherwise)?;
                    self.switch_to(then);
                    let yes = self.block(yes, loop_blocks)?;
                    if yes {
                        self.fb.ins().jump(join, &[]);
                    }
                    self.switch_to(otherwise);
                    let no = self.block(no, loop_blocks)?;
                    if no {
                        self.fb.ins().jump(join, &[]);
                    }
                    if !(yes || no) {
                        return Ok(false);
                    }
                    self.switch_to(join);
                }
                Stmt::While(condition, statements, _) => {
                    if self.shared_loop(condition, statements)? {
                        continue;
                    }
                    let (head, run, exit) = (
                        self.fb.create_block(),
                        self.fb.create_block(),
                        self.fb.create_block(),
                    );
                    self.fb.ins().jump(head, &[]);
                    self.switch_to(head);
                    self.branch(condition, run, exit)?;
                    self.switch_to(run);
                    if self.block(statements, Some((head, exit)))? {
                        self.fb.ins().jump(head, &[]);
                    }
                    self.switch_to(exit);
                }
                Stmt::Break | Stmt::Continue => {
                    let (head, exit) =
                        loop_blocks.ok_or_else(|| error("loop control outside a loop"))?;
                    let destination = if matches!(statement, Stmt::Break) {
                        exit
                    } else {
                        head
                    };
                    self.fb.ins().jump(destination, &[]);
                    return Ok(false);
                }
                Stmt::Trap(_) => {
                    self.fb.ins().trap(TRAP);
                    return Ok(false);
                }
                Stmt::Match { value, arms } => {
                    if !self.match_sum(value, arms, loop_blocks)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    fn binary_typed(&mut self, op: Binary, left: Value, right: Value, ty: Ty, counted_step: bool) -> Value {
        if (counted_step || self.abi.proved_arithmetic) && matches!(ty, Ty::Unsigned(_)) {
            match op {
                Binary::Add => return self.fb.ins().iadd(left, right),
                Binary::Sub => return self.fb.ins().isub(left, right),
                Binary::Mul => return self.fb.ins().imul(left, right),
                _ => {}
            }
        }
        let value = self.binary(op, left, right);
        self.narrow(value, ty)
    }

    fn binary(&mut self, op: Binary, left: Value, right: Value) -> Value {
        use Binary::*;
        let comparison = match op {
            Add | Sub | Mul => {
                let (value, overflow) = match op {
                    Add => self.fb.ins().uadd_overflow(left, right),
                    Sub => self.fb.ins().usub_overflow(left, right),
                    _ => self.fb.ins().umul_overflow(left, right),
                };
                self.fb.ins().trapnz(overflow, TRAP);
                return value;
            }
            Div => return self.fb.ins().udiv(left, right),
            Rem => return self.fb.ins().urem(left, right),
            Eq => IntCC::Equal,
            Ne => IntCC::NotEqual,
            Lt => IntCC::UnsignedLessThan,
            Le => IntCC::UnsignedLessThanOrEqual,
            Gt => IntCC::UnsignedGreaterThan,
            Ge => IntCC::UnsignedGreaterThanOrEqual,
            And | Or => unreachable!("short-circuit operators handled in expression lowering"),
        };
        let value = self.fb.ins().icmp(comparison, left, right);
        self.fb.ins().uextend(I64, value)
    }
}
