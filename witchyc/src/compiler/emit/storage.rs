use super::*;

impl Emitter<'_> {
    /// The allocator's `cell` helper is the only way to a cell: it validates the token and type,
    /// applies `mode` (see `allocator::cell`) and keeps the occupancy bitmap and count, which live
    /// in the allocator's own memory. Returns the locals holding (error, cell address).
    pub(super) fn cell(&mut self, out: &mut Vec<u8>, token: u32, index: u32, buffer: u32, mode: u8) -> (u32, u32) {
        get(out, token); out.push(0x42); signed(out, buffer as i64 + 1); get(out, index);
        out.extend([0x42, mode]);
        // No range: the range modes alone read `cells`.
        out.extend([0x42, 0]);
        call(out, self.indices.slot_prepare);
        let error = self.temporary(Ty::U64);
        let address = self.temporary(Ty::U64);
        indexed(out, 0x21, address); indexed(out, 0x21, error);
        (error, address)
    }
    /// `count * stride` payload bytes from `from` to `to` in program memory, eight at a time and
    /// then byte by byte. The runs are in distinct allocations, so they do not overlap.
    fn copy_bytes(&mut self, out: &mut Vec<u8>, to: u32, from: u32, count: u32, buffer: u32) {
        let stride = self.layouts.buffers()[buffer as usize].stride;
        let bytes = self.temporary(Ty::U64);
        get(out, count); out.push(0x42); signed(out, stride as i64); out.push(0x7e); indexed(out, 0x21, bytes);
        let at = self.temporary(Ty::U64);
        out.extend([0x42, 0]); indexed(out, 0x21, at);
        for (width, load, store, align) in [(8i64, 0x29u8, 0x37u8, 3u8), (1, 0x31, 0x3c, 0)] {
            // while at + width <= bytes: to[at] = from[at]; at += width
            out.extend([0x02, 0x40, 0x03, 0x40]);
            get(out, at); out.push(0x42); signed(out, width); out.push(0x7c); get(out, bytes); out.push(0x56); // > bytes
            out.extend([0x0d, 1]);
            get(out, to); get(out, at); out.extend([0x7c, 0xa7]);
            get(out, from); get(out, at); out.extend([0x7c, 0xa7]);
            out.extend([load, align, 0]);
            out.extend([store, align, 0]);
            get(out, at); out.push(0x42); signed(out, width); out.push(0x7c); indexed(out, 0x21, at);
            out.extend([0x0c, 0, 0x0b, 0x0b]);
        }
    }
    /// The range modes of the `cell` helper (6: mark `count` empty cells full, 7: full cells
    /// empty), from cell `index`. Returns the local holding the first cell's address.
    fn cell_range(&mut self, out: &mut Vec<u8>, token: u32, index: u32, count: u32, buffer: u32, mode: u8) -> u32 {
        get(out, token); out.push(0x42); signed(out, buffer as i64 + 1); get(out, index);
        out.extend([0x42, mode]);
        get(out, count);
        call(out, self.indices.range_prepare);
        let address = self.temporary(Ty::U64);
        indexed(out, 0x21, address); out.push(0x1a); // the error word is always 0: drop it
        address
    }
    /// The address of a cell that a proof has shown to be in range and full, for an operation
    /// that leaves it full: the token carries where the allocation starts, so no helper is
    /// called and nothing in the allocator's memory is read. Returns the local holding it.
    pub(super) fn allocation_base(out: &mut Vec<u8>, token: u32) {
        let heap = crate::compiler::allocator::heap();
        get(out, token); out.extend([0x42, 1, 0x7d]);
        out.push(0x42); signed(out, crate::compiler::allocator::first_quantum_mask(heap) as i64); out.push(0x83);
        out.extend([0x42, 5, 0x86]);
        out.push(0x42); signed(out, crate::compiler::allocator::HEAP_LOW as i64); out.push(0x7c);
    }
    pub(super) fn proved_cell(&mut self, out: &mut Vec<u8>, token: u32, index: u32, buffer: u32) -> u32 {
        let stride = self.layouts.buffers()[buffer as usize].stride;
        Self::allocation_base(out, token);
        get(out, index); out.push(0x42); signed(out, stride as i64); out.extend([0x7e, 0x7c]);
        let address = self.temporary(Ty::U64);
        indexed(out, 0x21, address);
        address
    }
    /// Guard results are private i64 locals. No typed T is loaded or stored on the normal Err path.
    /// The value of a checked `init` or `swap` is evaluated only when the guard succeeds, as before.
    /// For `init` that is after the helper has marked the cell full; the receiver's unique loan
    /// spans the value expression, so nothing the value runs can reach the cell before it is written.
    pub(super) fn storage(&mut self, out: &mut Vec<u8>, result: Ty, op: StorageOp, receiver: &Expr,
        index: &Expr, value: Option<&Expr>, buffer: u32) {
        let element = self.layouts.buffers()[buffer as usize].element;
        self.expr(out, receiver);
        if matches!(op, StorageOp::RefLink | StorageOp::MutLink) {
            // Evaluate the permission expression in source order. The proof identifies
            // the supplied link with that owner's token and admits its first cell.
            out.push(0x1a);
            let link = self.capture(out, index);
            Self::allocation_base(out, link);
            out.push(0xa7); // program-memory address of cell zero
            return;
        }
        Self::memory(out, Ty::Buffer(buffer), false); // permission load, never owner Copy
        let token = self.temporary(Ty::U64); indexed(out, 0x21, token);
        let index = self.capture(out, index); // exactly once, after receiver and before state guard
        // A value that is handed back on failure, or stored on a proof's word, is evaluated up front.
        let early = matches!(op, StorageOp::TryInit | StorageOp::InitUnchecked | StorageOp::ReplaceUnchecked | StorageOp::FillUnchecked);
        let mut incoming = if early { value.map(|value| self.capture(out, value)) } else { None };
        let cells = self.layouts.cells(element).expect("checked element cells");
        let guards = self.layouts.guards(element);
        // Proof-admitted operations: bounds and cell state MUST come from the separate current
        // kernel VC. The helper still validates token and type; it makes no index or state test.
        match op {
            StorageOp::InitUnchecked => {
                let (_, address) = self.cell(out, token, index, buffer, 3);
                let incoming = incoming.expect("typed unchecked init payload");
                Self::store_cells(out, &cells, &guards, address, incoming);
                return;
            }
            StorageOp::ReplaceUnchecked => {
                let address = self.proved_cell(out, token, index, buffer);
                Self::load_cells(out, &cells, &guards, address, None);
                Self::store_cells(out, &cells, &guards, address, incoming.expect("typed replacement payload"));
                return;
            }
            StorageOp::TakeUnchecked | StorageOp::GetUnchecked => {
                let address = if op == StorageOp::TakeUnchecked { self.cell(out, token, index, buffer, 4).1 } else { self.proved_cell(out, token, index, buffer) };
                Self::load_cells(out, &cells, &guards, address, None);
                return;
            }
            StorageOp::MovePrefixUnchecked => {
                // `index` is the count; the target is evaluated after it. Both are places, read
                // without running anything, so the order cannot be seen.
                let target = value.expect("a range move's target");
                self.expr(out, target);
                Self::memory(out, Ty::Buffer(buffer), false);
                let target_token = self.temporary(Ty::U64); indexed(out, 0x21, target_token);
                let zero = self.temporary(Ty::U64); out.extend([0x42, 0]); indexed(out, 0x21, zero);
                // The helper checks each whole range before it marks anything: the target's cells
                // empty, then the source's full; a contradiction traps.
                let to = self.cell_range(out, target_token, zero, index, buffer, 6);
                let from = self.cell_range(out, token, zero, index, buffer, 7);
                self.copy_bytes(out, to, from, index, buffer);
                return;
            }
            StorageOp::TakePrefixUnchecked => {
                // Copy elements: one helper call checks every cell full and marks the run empty;
                // no payload moves.
                let zero = self.temporary(Ty::U64); out.extend([0x42, 0]); indexed(out, 0x21, zero);
                let _ = self.cell_range(out, token, zero, index, buffer, 7);
                return;
            }
            StorageOp::ReadWordUnchecked => {
                let count = self.capture(out, value.expect("a word read's count"));
                // The first cell's address from the token, as a proved read finds it: no helper.
                let at = self.proved_cell(out, token, index, buffer);
                get(out, count); out.extend([0x42, 8, 0x51]); // i64.eq
                out.extend([0x04, 0x7e]); // if (result i64)
                get(out, at); out.push(0xa7); out.extend([0x29, 0, 0]); // i64.load, byte aligned
                out.push(0x05);
                let word = self.temporary(Ty::U64);
                let k = self.temporary(Ty::U64);
                out.extend([0x42, 0]); indexed(out, 0x21, word);
                out.extend([0x42, 0]); indexed(out, 0x21, k);
                out.extend([0x02, 0x40, 0x03, 0x40]);
                get(out, k); get(out, count); out.extend([0x5a, 0x0d, 1]); // k >= count: out
                get(out, word);
                get(out, at); get(out, k); out.extend([0x7c, 0xa7]); out.extend([0x31, 0, 0]); // i64.load8_u
                get(out, k); out.extend([0x42, 3, 0x86, 0x86]); // << (k * 8)
                out.push(0x84); indexed(out, 0x21, word); // i64.or
                get(out, k); out.extend([0x42, 1, 0x7c]); indexed(out, 0x21, k);
                out.extend([0x0c, 0, 0x0b, 0x0b]);
                get(out, word);
                out.push(0x0b);
                return;
            }
            StorageOp::InitWordUnchecked => {
                let Some(Expr { kind: Value::Aggregate(parts), .. }) = value else { unreachable!("checked: (word, count)") };
                let [word, count] = parts.as_slice() else { unreachable!("checked: (word, count)") };
                let word = self.capture(out, word);
                let count = self.capture(out, count);
                // One helper call checks the run empty and marks it full; then the bytes, in
                // program memory: the whole word at once when it is all of them.
                let at = self.cell_range(out, token, index, count, buffer, 6);
                get(out, count); out.extend([0x42, 8, 0x51]); // i64.eq
                out.extend([0x04, 0x40]);
                get(out, at); out.push(0xa7); get(out, word); out.extend([0x37, 0, 0]); // i64.store, byte aligned
                out.push(0x05);
                let k = self.temporary(Ty::U64);
                out.extend([0x42, 0]); indexed(out, 0x21, k);
                out.extend([0x02, 0x40, 0x03, 0x40]);
                get(out, k); get(out, count); out.extend([0x5a, 0x0d, 1]); // k >= count: out
                get(out, at); get(out, k); out.extend([0x7c, 0xa7]);
                get(out, word); get(out, k); out.extend([0x42, 3, 0x86, 0x88]); // word >> (k * 8)
                out.extend([0x3c, 0, 0]); // i64.store8
                get(out, k); out.extend([0x42, 1, 0x7c]); indexed(out, 0x21, k);
                out.extend([0x0c, 0, 0x0b, 0x0b]);
                out.push(0x0b);
                return;
            }
            StorageOp::FillUnchecked => {
                // One helper call checks every cell empty and marks the run full; the item is then
                // stored in each, one stride apart, in program memory.
                let incoming = incoming.expect("a fill's item");
                let zero = self.temporary(Ty::U64); out.extend([0x42, 0]); indexed(out, 0x21, zero);
                let at = self.cell_range(out, token, zero, index, buffer, 6);
                let stride = self.layouts.buffers()[buffer as usize].stride;
                let left = self.temporary(Ty::U64); get(out, index); indexed(out, 0x21, left);
                // One-byte cells: eight at a time while eight remain, the byte in every place of
                // one word (a store a byte was 16 instructions a byte, 2026-11-22). The same
                // cells get the same bytes as the loop below, which writes those left over; the
                // helper above has already admitted the whole run.
                if let ([cell], 1) = (&cells[..], stride)
                    && cell.ty == Ty::Unsigned(Scalar::U8) && cell.byte_offset == 0 && guards[cell.slot as usize].is_empty() {
                    let word = self.temporary(Ty::U64);
                    get(out, incoming + cell.slot); out.push(0x42); signed(out, 255); out.push(0x83);
                    out.push(0x42); signed(out, 0x0101_0101_0101_0101); out.push(0x7e); indexed(out, 0x21, word);
                    out.extend([0x02, 0x40, 0x03, 0x40]);
                    get(out, left); out.extend([0x42, 8, 0x54, 0x0d, 1]); // left < 8: out
                    get(out, at); out.push(0xa7); get(out, word); out.extend([0x37, 0, 0]); // i64.store, byte aligned
                    get(out, at); out.extend([0x42, 8, 0x7c]); indexed(out, 0x21, at);
                    get(out, left); out.extend([0x42, 8, 0x7d]); indexed(out, 0x21, left);
                    out.extend([0x0c, 0, 0x0b, 0x0b]);
                }
                // while left != 0: store at `at`; at += stride; left -= 1
                out.extend([0x02, 0x40, 0x03, 0x40]);
                get(out, left); out.extend([0x50, 0x0d, 1]); // i64.eqz; br_if out
                Self::store_cells(out, &cells, &guards, at, incoming);
                get(out, at); out.push(0x42); signed(out, stride as i64); out.push(0x7c); indexed(out, 0x21, at);
                get(out, left); out.extend([0x42, 1, 0x7d]); indexed(out, 0x21, left);
                out.extend([0x0c, 0, 0x0b, 0x0b]);
                return;
            }
            StorageOp::InitSliceUnchecked => {
                let source = self.capture(out, value.expect("shared source slice"));
                let count = source + 1;
                let target = self.cell_range(out, token, index, count, buffer, 6);
                let address = self.temporary(Ty::U64);
                get(out, source); out.push(0xad); indexed(out, 0x21, address);
                self.copy_bytes(out, target, address, count, buffer);
                return;
            }
            StorageOp::CopyIntoUnchecked => {
                let Some(Expr { kind: Value::Aggregate(parts), .. }) = value else { unreachable!("checked: (target, at)") };
                let [target, at] = parts.as_slice() else { unreachable!("checked: (target, at)") };
                self.expr(out, target);
                Self::memory(out, Ty::Buffer(buffer), false);
                let target_token = self.temporary(Ty::U64); indexed(out, 0x21, target_token);
                let at = self.capture(out, at);
                // The target's run is checked empty and marked full once; the source's first cell
                // is found from its token, as a proved read is, and nothing about it changes.
                let to = self.cell_range(out, target_token, at, index, buffer, 6);
                let zero = self.temporary(Ty::U64); out.extend([0x42, 0]); indexed(out, 0x21, zero);
                let from = self.proved_cell(out, token, zero, buffer);
                self.copy_bytes(out, to, from, index, buffer);
                return;
            }
            StorageOp::SliceUnchecked => {
                // The first cell's address, then the count; the proof admitted every cell in between.
                let to = self.capture(out, value.expect("a slice's end"));
                let address = self.proved_cell(out, token, index, buffer);
                Self::heap_address(out, address, 0);
                get(out, to); get(out, index); out.push(0x7d);
                return;
            }
            StorageOp::RefUnchecked | StorageOp::MutUnchecked => {
                // The reference is the cell's address; the admitted bound and occupancy make it a value.
                let address = self.proved_cell(out, token, index, buffer);
                Self::heap_address(out, address, 0);
                return;
            }
            _ => {}
        }
        let mode = match op { StorageOp::Init | StorageOp::TryInit => 0, StorageOp::Take => 1, _ => 2 };
        let (guard, address) = self.cell(out, token, index, buffer, mode);
        if op == StorageOp::Initialized {
            // Out of range and empty are ordinary false, success is true.
            // No T load/store or bitmap/count change is performed by a probe.
            get(out, guard); out.push(0x50); // i64.eqz returns canonical i32 Bool
            return;
        }
        let carriers = self.layouts.flat(result);
        // `try_init`'s `Result<(), T>` has no tag word when `T` is an owner that is never zero
        // (`Buffer<_>`, layout.rs `zero_arm`): the one carrier is zero for `Ok` and the item handed
        // back for `Err`. Every other result here has an error number in its `Err`, so a tag.
        // (Review 2026-11-21: the tag was written into that carrier, an i32 into an i64, and
        // the module was refused by the engine.) Anything else is a compiler defect: stop.
        let niche = self.layouts.sum(result).expect("a checked storage operation's Result").zero_arm;
        assert!(niche.is_none() || (op == StorageOp::TryInit && niche == Some(0) && carriers.len() == 1),
            "a storage operation's result without a tag is `try_init`'s alone");
        let output = self.locals.len() as u32;
        self.locals.extend(&carriers);
        // Inactive physical carriers only, not initialized source elements or owner values.
        for (slot, ty) in carriers.iter().enumerate() {
            Self::zero_carrier(out, *ty); indexed(out, 0x21, output + slot as u32);
        }
        get(out, guard); out.push(0x50); out.extend([0x04, 0x40]);
        // Success only: the complete incoming value is captured before any payload store.
        if !early { incoming = value.map(|value| self.capture(out, value)); }
        if op == StorageOp::Ref {
            // The reference is the checked cell's address.
            Self::heap_address(out, address, 0); indexed(out, 0x21, output + 1);
        }
        if matches!(op, StorageOp::Read | StorageOp::Take | StorageOp::Swap) {
            Self::load_cells(out, &cells, &guards, address, Some(output + 1));
        }
        if let Some(incoming) = incoming {
            Self::store_cells(out, &cells, &guards, address, incoming);
        }
        out.push(0x05); // normal Err skips ALL typed leaf loads/stores
        if niche.is_some() {
            // The cell kept its state; the owner handed back is the whole result, and is not zero.
            get(out, incoming.expect("try_init's item")); indexed(out, 0x21, output);
        } else {
            self.set_tag(out, output, 1);
            if let (StorageOp::TryInit, Some(handed)) = (op, incoming) {
                // The cell kept its state; the value goes back whole in the Err arm.
                for slot in 0..carriers.len() as u32 - 1 { get(out, handed + slot); indexed(out, 0x21, output + 1 + slot); }
            } else { get(out, guard); indexed(out, 0x21, output + carriers.len() as u32 - 1); }
        }
        out.push(0x0b);
        for slot in 0..carriers.len() as u32 { get(out, output + slot); }
    }
}
