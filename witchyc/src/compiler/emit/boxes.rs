use super::*;

impl Emitter<'_> {
    /// Store a value held in locals from `source` into a heap cell: only live variants' cells.
    pub(super) fn store_cells(out: &mut Vec<u8>, cells: &[crate::compiler::cell::Cell], guards: &[Vec<(u32, u8)>], address: u32, source: u32) {
        for cell in cells {
            let guard = &guards[cell.slot as usize];
            if !guard.is_empty() { Self::live(out, guard, &mut |out, tag| get(out, source + tag)); out.extend([0x04, 0x40]); }
            Self::heap_address(out, address, cell.byte_offset);
            get(out, source + cell.slot); Self::memory(out, cell.ty, true);
            if !guard.is_empty() { out.push(0x0b); }
        }
    }
    /// Load a heap cell's value, onto the stack or into locals from `into`. A variant's cell that is
    /// not live holds another variant's bytes, and reads as zero.
    pub(super) fn load_cells(out: &mut Vec<u8>, cells: &[crate::compiler::cell::Cell], guards: &[Vec<(u32, u8)>], address: u32, into: Option<u32>) {
        let at: BTreeMap<u32, u64> = cells.iter().map(|cell| (cell.slot, cell.byte_offset)).collect();
        for cell in cells {
            Self::heap_address(out, address, cell.byte_offset);
            Self::memory(out, cell.ty, false);
            let guard = &guards[cell.slot as usize];
            if !guard.is_empty() {
                Self::zero_carrier(out, cell.ty);
                Self::live(out, guard, &mut |out, tag| { Self::heap_address(out, address, at[&tag]); Self::memory(out, Ty::Tag, false); });
                out.push(0x1b);
            }
            if let Some(into) = into { indexed(out, 0x21, into + cell.slot); }
        }
    }
    pub(super) fn box_word(&mut self, out: &mut Vec<u8>, op: crate::compiler::operations::BoxOp, owner: &Expr) -> u32 {
        if matches!(op, crate::compiler::operations::BoxOp::Take | crate::compiler::operations::BoxOp::EndNull) {
            self.expr(out, owner);
        } else if let Value::Address(place) = &owner.kind {
            // Only the internal address word is read. The checked loan stays live
            // for exactly the same scope even if the place resides in a local.
            self.read_place(out, place);
        } else {
            self.expr(out, owner);
            Self::memory(out, Ty::Boxed, false);
        }
        let word = self.temporary(Ty::U64);
        indexed(out, 0x21, word);
        word
    }
    pub(super) fn release_box(&self, out: &mut Vec<u8>, address: u32, id: u32) {
        get(out, address); out.push(0x42);
        signed(out, (crate::compiler::allocator::BOX_TYPE | u64::from(id + 1)) as i64);
        call(out, self.indices.try_release);
        // Only Buffer cells contribute to the allocator's initialized-cell count.
        out.extend([0x1a, 0x1a]);
    }
    pub(super) fn box_operation(&mut self, out: &mut Vec<u8>, op: crate::compiler::operations::BoxOp, id: u32, args: &[Expr], result: Ty) {
        use crate::compiler::operations::BoxOp;
        let element = self.layouts.buffers()[id as usize].element;
        let type_id = crate::compiler::allocator::BOX_TYPE | u64::from(id + 1);
        if op == BoxOp::New {
            self.expr(out, &args[0]);
            Self::memory(out, Ty::Allocator, false);
            let given = self.capture(out, &args[1]);
            out.push(0x42); signed(out, type_id as i64);
            out.extend([0x42, 1]);
            call(out, self.indices.try_alloc);
            out.push(0x1a); // error code; the source error arm returns the whole input
            let address = self.temporary(Ty::U64);
            indexed(out, 0x21, address);
            let output = self.result_locals(out, result);
            self.tag_word(output);
            indexed(out, 0x22, output); // allocator's tag
            out.extend([0x45, 0x04, 0x40]);
            Self::store_cells(out, &self.layouts.cells(element).expect("box cells"), &self.layouts.guards(element), address, given);
            get(out, address); indexed(out, 0x21, output + 1);
            out.push(0x05);
            for slot in 0..self.layouts.flat(element).len() as u32 {
                get(out, given + slot); indexed(out, 0x21, output + 2 + slot);
            }
            out.push(0x0b);
            for slot in 0..self.layouts.flat(result).len() as u32 { get(out, output + slot); }
            return;
        }
        if op == BoxOp::FromBuffer {
            // This projection is inert, including for a capacity-zero buffer.
            // Storage access still needs a live owner and an initialized cell.
            self.expr(out, &args[0]);
            Self::memory(out, Ty::Buffer(id), false);
            let token = self.temporary(Ty::U64);
            indexed(out, 0x21, token);
            Self::allocation_base(out, token);
            return;
        }
        if op == BoxOp::WeakBits {
            self.expr(out, &args[0]);
            return;
        }
        if op == BoxOp::Enclosing {
            // The membership proof binds operand 2 to an initialized parent
            // focus. All other operands are checked, immutable projections.
            self.expr(out, &args[2]);
            out.push(0xa7);
            return;
        }
        if matches!(op, BoxOp::FollowLoan | BoxOp::FollowLoanMut) {
            // Admission recovered this witness from fixed field projections and
            // pure, initialized index-zero loans. Only the stored address is needed.
            self.expr(out, &args[1]);
            out.push(0xa7);
            return;
        }
        if matches!(op, BoxOp::Follow | BoxOp::FollowMut) {
            // Only the live loan carries authority. Its proven allocation identity
            // matches this weak value. Neither a generation nor a table is loaded.
            // Forming a fixed projection has no runtime effects. An indexed
            // place still evaluates its index (which may itself call or trap).
            if !matches!(&args[0].kind, Value::Address(p) if p.static_offsets().is_some()) {
                self.expr(out, &args[0]);
                out.push(0x1a);
            }
            self.expr(out, &args[1]); out.push(0xa7);
            return;
        }
        if op == BoxOp::EndNull {
            self.expr(out, &args[0]);
            out.push(0x1a);
            return;
        }
        let address = self.box_word(out, op, &args[0]);
        if matches!(op, BoxOp::Lend | BoxOp::LendMut) { get(out, address); out.push(0xa7); return; }
        if matches!(op, BoxOp::Address | BoxOp::Weaken) { get(out, address); return; }
        if op == BoxOp::Take && self.layouts.sum(result).is_some_and(|sum| sum.zero_arm.is_some()) {
            get(out, address); out.extend([0x50, 0x04, 0x7e, 0x42, 0, 0x05]);
            Self::load_cells(out, &self.layouts.cells(element).expect("owned payload"), &self.layouts.guards(element), address, None);
            self.release_box(out, address, id);
            out.push(0x0b);
            return;
        }
        let output = self.result_locals(out, result);
        get(out, address); out.extend([0x50, 0x04, 0x40]); // null is the ordinary Err(()) arm
        self.set_tag(out, output, 1);
        out.push(0x05);
        if op == BoxOp::Take {
            Self::load_cells(out, &self.layouts.cells(element).expect("box cells"), &self.layouts.guards(element), address, Some(output + 1));
            self.release_box(out, address, id);
        } else {
            get(out, address); out.push(0xa7); indexed(out, 0x21, output + 1);
        }
        out.push(0x0b);
        for slot in 0..self.layouts.flat(result).len() as u32 { get(out, output + slot); }
    }
    pub(super) fn heap_address(out: &mut Vec<u8>, address: u32, byte_offset: u64) {
        get(out, address);
        // checked slot success, or proof-admitted unchecked index + typed extent; no add for 0.
        if byte_offset != 0 { out.push(0x42); signed(out, byte_offset as i64); out.push(0x7c); }
        out.push(0xa7);
    }
}
