use super::*;

impl Emitter<'_> {
    pub(super) fn block(&mut self, out: &mut Vec<u8>, body: &[Stmt]) {
        for stmt in body {
            if !self.resumable { self.statement(out, stmt); continue; }
            // Going back into a paused task runs only the statements that hold the site it
            // paused in (`resume`); while running, every statement runs.
            let count = crate::compiler::pause::count(stmt, &self.tasks.pausing);
            indexed(out, 0x23, STATE); out.push(0x45);
            if count > 0 {
                get(out, self.resume); Self::i32_const(out, self.site); out.push(0x6b);
                Self::i32_const(out, count); out.extend([0x49, 0x72]);
            }
            out.extend([0x04, 0x40]);
            self.labels += 1;
            let first = self.site;
            self.statement(out, stmt);
            debug_assert_eq!(self.site, first + count, "every site is counted where it is emitted");
            self.labels -= 1;
            out.push(0x0b);
        }
    }
    fn statement(&mut self, out: &mut Vec<u8>, stmt: &Stmt) {
        {
            match stmt {
                Stmt::Set(local, value) => {
                    if self.repeat_in_frame(out, *local, value) { return; }
                    if self.copy_in_frame(out, *local, value) { return; }
                    self.expr(out, value);
                    self.store_slots(out, *local, value.ty);
                }
                Stmt::StorePlace(place, operation, value) => {
                    // Resolve every target index once, before old-value/RHS reads.
                    let location = self.locate(out, place);
                    if operation.is_some() { self.read_location(out, &location, place.ty); }
                    self.expr(out, value);
                    if let Some(op) = operation {
                        // Shown in range by the function's proof: the plain operation.
                        let step = matches!(op, Binary::Add) && place.projections.is_empty() && !place.pointer && self.steps.contains(&place.root)
                            && place.ty == Ty::Unsigned(crate::compiler::scalar::Scalar::U64) && matches!(value.kind, Value::U64(1));
                        if step || (self.proved && matches!(op, Binary::Add | Binary::Sub | Binary::Mul) && matches!(place.ty, Ty::Unsigned(_))) {
                            out.push(match op { Binary::Add => 0x7c, Binary::Sub => 0x7d, _ => 0x7e });
                        } else {
                            self.checked(out, *op);
                            if let Ty::Unsigned(scalar) = place.ty { self.narrow(out, scalar); }
                        }
                    }
                    self.write_location(out, &location, place.ty);
                }
                Stmt::Replace(slot, place, value) => {
                    let location = self.locate(out, place); // local target has no evaluation effects
                    self.expr(out, value); // RHS move/call BEFORE replacing the old affine carrier
                    let _ = slot; // logical availability transition checked before emission
                    self.write_location(out, &location, place.ty);
                }
                Stmt::Expire(slots) => { let _ = slots; } // discardable authority, no host close/unwind
                Stmt::Eval(value) => {
                    self.expr(out, value);
                    for _ in self.layouts.flat(value.ty) {
                        out.push(0x1a);
                    }
                }
                Stmt::Return(value, expired) => {
                    self.expr(out, value);
                    let _ = expired; // logical affine expiry AFTER evaluating return; no callbacks
                    self.restore(out);
                    out.push(0x0f);
                }
                Stmt::If(condition, yes, no) => {
                    self.expr(out, condition);
                    out.extend([0x04, 0x40]);
                    self.labels += 1;
                    self.hit(out,"then");
                    self.block(out, yes);
                    out.push(0x05);
                    self.hit(out,"else");
                    self.block(out, no);
                    self.labels -= 1;
                    out.push(0x0b);
                }
                // The loop label is the nearest one outside this statement's own; the exit block is one further.
                Stmt::Continue => { out.push(0x0c); unsigned(out, self.labels); }
                Stmt::Break => { out.push(0x0c); unsigned(out, self.labels + 1); }
                Stmt::Trap => out.push(0x00),
                Stmt::Match { value: Expr { kind: Value::BoxOp(op, id, args), .. }, arms }
                    if matches!(op, crate::compiler::operations::BoxOp::Ref | crate::compiler::operations::BoxOp::Mut | crate::compiler::operations::BoxOp::Take) => {
                    use crate::compiler::operations::BoxOp;
                    // Both variants are generated here from the pointer itself. The
                    // scrutinee has no stored tag that could be invalid.
                    let address = self.box_word(out, *op, &args[0]);
                    if *op != BoxOp::Take && self.tracked.is_none() && self.returns_nullable_borrow(arms) {
                        // In the null arm the inactive pointer carrier is already zero.
                        get(out, address); out.push(0x50);
                        get(out, address); out.push(0xa7);
                        self.restore(out);
                        out.push(0x0f);
                        return;
                    }
                    get(out, address); out.extend([0x50, 0x04, 0x40]);
                    self.labels += 1;
                    self.hit(out, "arm");
                    self.block(out, &arms[1].body); // Err(())
                    out.push(0x05);
                    self.hit(out, "arm");
                    let occupied = &arms[0];
                    if *op == BoxOp::Take {
                        let element = self.layouts.buffers()[*id as usize].element;
                        if let Some(binding) = occupied.binding {
                            Self::load_cells(out, &self.layouts.cells(element).expect("box cells"), &self.layouts.guards(element), address, None);
                            self.store_slots(out, binding, element);
                        }
                        self.release_box(out, address, *id);
                    } else if let Some(binding) = occupied.binding {
                        get(out, address); out.push(0xa7);
                        self.store_slots(out, binding, occupied.payload);
                    }
                    self.block(out, &occupied.body);
                    self.labels -= 1;
                    out.push(0x0b);
                }
                Stmt::Match { value, arms } => {
                    let sum = self.layouts.sum(value.ty).expect("checked Result scrutinee").clone();
                    let source = self.capture(out, value); // ALL physical carriers, once
                    // if tag == 0 { arm 0 } else if tag == 1 { arm 1 } ... else unreachable
                    for (tag, arm) in arms.iter().enumerate() {
                        get(out, source);
                        Self::variant_tag(out, sum.zero_arm);
                        Self::i32_const(out, tag as u32);
                        out.extend([0x46, 0x04, 0x40]);
                        self.labels += 1;
                        self.hit(out,"arm");
                        self.active_arm(out, arm, source + sum.starts[tag]);
                        out.push(0x05);
                    }
                    out.push(0x00); // an invalid tag traps; no arm is a default
                    self.labels -= arms.len() as u32;
                    out.extend(std::iter::repeat(0x0b).take(arms.len()));
                }
                Stmt::MatchRef { value, offsets, arms, tag: kept } => {
                    let address = self.capture(out, value);
                    let Ty::Borrow { pointee: crate::compiler::borrow::Pointee::Sum(id), .. } = value.ty else { unreachable!("typed match") };
                    let zero = self.layouts.sum(Ty::Sum(id)).expect("borrowed sum").zero_arm;
                    // Read once while running; going back in, the local holds what was read.
                    if let Some(kept) = kept {
                        indexed(out, 0x23, STATE); out.extend([0x45, 0x04, 0x40]);
                        Self::stored_tag(out, address, zero); indexed(out, 0x21, *kept);
                        out.push(0x0b);
                    }
                    for (tag, arm) in arms.iter().enumerate() {
                        match kept { Some(kept) => get(out, *kept), None => Self::stored_tag(out, address, zero) }
                        Self::i32_const(out, tag as u32);
                        out.extend([0x46, 0x04, 0x40]);
                        self.labels += 1;
                        self.hit(out,"arm");
                        if let Some(binding) = arm.binding {
                            // The binder is a reference to the variant's value where it is stored.
                            get(out, address); Self::i32_const(out, offsets[tag]); out.push(0x6a);
                            self.write_local(out, binding, arm.payload);
                        }
                        self.block(out, &arm.body);
                        out.push(0x05);
                    }
                    out.push(0x00); // an invalid tag traps; no arm is a default
                    self.labels -= arms.len() as u32;
                    out.extend(std::iter::repeat(0x0b).take(arms.len()));
                }
                Stmt::While(condition, body) => {
                    out.extend([0x02, 0x40, 0x03, 0x40]); // break block, continue loop
                    self.expr(out, condition);
                    out.extend([0x45, 0x0d, 1]);
                    let outer = std::mem::replace(&mut self.labels, 0);
                    self.depth += outer + 2;
                    self.hit(out,"loop");
                    self.block(out, body);
                    self.depth -= outer + 2;
                    self.labels = outer;
                    out.extend([0x0c, 0, 0x0b, 0x0b]);
                }
            }
        }
    }
    /// The two branches merely return the borrowed pointer in variant zero or
    /// the empty variant one. Only elide these exact, effect-free constructors.
    fn returns_nullable_borrow(&self, arms: &[crate::compiler::match_arm::Arm]) -> bool {
        let [live, empty] = arms else { return false };
        let ([Stmt::Return(some, _)], [Stmt::Return(none, _)]) = (live.body.as_slice(), empty.body.as_slice()) else { return false };
        let (Value::Sum { arm: 0, payload }, Value::Sum { arm: 1, payload: unit }) = (&some.kind, &none.kind) else { return false };
        let Some(sum) = self.layouts.sum(some.ty) else { return false };
        if some.ty != none.ty || sum.arms.as_slice() != [live.payload, Ty::Unit]
            || !matches!(live.payload, Ty::Borrow { .. }) || !matches!(unit.kind, Value::Unit) { return false; }
        let (Value::ReadPlace(place) | Value::Move { place, .. }) = &payload.kind else { return false };
        Some(place.root) == live.binding && !place.pointer && place.projections.is_empty()
    }

    /// Convert the representation on the stack to its logical variant index.
    pub(super) fn variant_tag(out: &mut Vec<u8>, zero_arm: Option<u8>) {
        if let Some(zero) = zero_arm {
            out.push(0x50); // i64.eqz
            if zero == 0 { out.push(0x45); }
        }
    }
    fn stored_tag(out: &mut Vec<u8>, address: u32, zero: Option<u8>) {
        get(out, address);
        if zero.is_some() { out.extend([0x29, 3, 0]); } else { out.extend([0x28, 2, 0]); }
        Self::variant_tag(out, zero);
    }

    fn active_arm(&mut self, out: &mut Vec<u8>, arm: &crate::compiler::match_arm::Arm, payload: u32) {
        if let Some(binding) = arm.binding {
            let flat = self.layouts.flat(arm.payload);
            for slot in 0..flat.len() as u32 { get(out, payload + slot); }
            // Only this arm's initialized payload is bound. Unit has zero slots.
            self.store_slots(out, binding, arm.payload);
        }
        self.block(out, &arm.body);
    }
    pub(super) fn zero_carrier(out: &mut Vec<u8>, ty: Ty) {
        // Physical ABI padding ONLY: no Expr/source value/owner for inactive arms.
        match ty {
            Ty::Bool | Ty::Console | Ty::Allocator | Ty::Tag | Ty::Borrow { .. } => out.extend([0x41, 0]),
            Ty::Unsigned(_) | Ty::I64 | Ty::Boxed | Ty::Buffer(_) | Ty::Input | Ty::Host(_) => out.extend([0x42, 0]),
            Ty::F64 => { out.push(0x44); out.extend(0f64.to_le_bytes()); }
            _ => unreachable!("Result payload carriers were flattened and checked"),
        }
    }
    pub(super) fn save_stack(&mut self, out: &mut Vec<u8>, ty: Ty) -> u32 {
        let start = self.locals.len() as u32;
        let flat = self.layouts.flat(ty);
        self.locals.extend(&flat);
        for slot in (0..flat.len() as u32).rev() {
            indexed(out, 0x21, start + slot);
        }
        start
    }
    pub(super) fn capture(&mut self, out: &mut Vec<u8>, value: &Expr) -> u32 {
        self.expr(out, value);
        self.save_stack(out, value.ty)
    }
    pub(super) fn checked_index(&mut self, out: &mut Vec<u8>, index: &Expr, length: u32) -> u32 {
        let local = self.capture(out, index);
        if self.proved_index { return local; }
        get(out, local);
        out.push(0x42);
        signed(out, length as i64);
        out.push(0x5a);
        trap_for(out, 6);
        local
    }
    pub(super) fn select(&self, out: &mut Vec<u8>, start: u32, index: u32, length: u32, slots: u32) {
        // A bounded local-only selection; no guest address, pointer or indirect call.
        for slot in 0..slots {
            get(out, start + slot);
            for element in 1..length {
                get(out, start + element * slots + slot);
                get(out, index);
                out.push(0x42);
                signed(out, element as i64);
                out.extend([0x51, 0x45, 0x1b]); // retain previous unless this index matches
            }
        }
    }
}
