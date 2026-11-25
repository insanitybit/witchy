use super::*;

impl Emitter<'_> {
    pub(super) fn temporary(&mut self, ty: Ty) -> u32 {
        let index = self.locals.len() as u32;
        self.locals.push(ty);
        index
    }
    pub(super) fn i32_const(out: &mut Vec<u8>, value: u32) {
        out.push(0x41);
        signed(out, value as i64);
    }
    pub(super) fn prologue(&mut self, out: &mut Vec<u8>, parameters: u32) {
        if self.frame.bytes == 0 { return; }
        // Internal i32 temporaries share Bool's register carrier, never its
        // source value semantics (as does the output handle signature).
        let old = self.temporary(Ty::Bool);
        let base = self.temporary(Ty::Bool);
        self.old_sp = Some(old);
        self.base = Some(base);
        indexed(out, 0x23, 0);
        indexed(out, 0x21, old);
        get(out, old);
        self.bound(out, true);
        out.push(0x49); // old < low (unsigned)
        trap_if(out);
        get(out, old);
        self.bound(out, false);
        out.push(0x4b); // old > high (unsigned)
        trap_if(out);
        get(out, old);
        Self::i32_const(out, frame::ALIGNMENT - 1);
        out.push(0x71); // misaligned old
        trap_if(out);
        get(out, old);
        self.bound(out, true);
        out.push(0x6b); // subtraction now proven nonwrapping
        Self::i32_const(out, self.frame.bytes);
        out.push(0x49); // available < complete aligned frame
        trap_if(out);
        // No store or SP publication happens before every guard above.
        get(out, old);
        Self::i32_const(out, self.frame.bytes);
        out.push(0x6b);
        indexed(out, 0x22, base);
        indexed(out, 0x24, 0);
        // Parameters that live in the frame are copied there, a whole value at a time.
        for (root, ty) in self.frame.types.clone() {
            if root >= parameters { continue; }
            for slot in 0..self.layouts.flat(ty).len() as u32 { get(out, root + slot); }
            self.store_slots(out, root, ty);
        }
    }
    /// Where frames may be: fixed, or, in a program with tasks, the running task's stack.
    pub(super) fn bound(&self, out: &mut Vec<u8>, low: bool) {
        if self.tasks.used { indexed(out, 0x23, if low { LOW } else { HIGH }); }
        else { Self::i32_const(out, if low { frame::low() } else { frame::HIGH }); }
    }
    pub(super) fn restore(&self, out: &mut Vec<u8>) {
        if let Some(old) = self.old_sp {
            get(out, old);
            indexed(out, 0x24, 0);
        }
    }
    pub(super) fn address(&self, out: &mut Vec<u8>, local: u32) {
        get(out, self.base.expect("addressed scalar has a frame"));
        Self::i32_const(out, self.frame.slots[&local]);
        out.push(0x6a);
    }
    pub(super) fn memory(out: &mut Vec<u8>, ty: Ty, store: bool) {
        let (load, write, align) = match ty {
            Ty::Unsigned(Scalar::U8) => (0x31, 0x3c, 0),
            Ty::Unsigned(Scalar::U16) => (0x33, 0x3d, 1),
            Ty::Boxed | Ty::Unsigned(Scalar::U32) => (0x35, 0x3e, 2),
            Ty::Unsigned(Scalar::U64) => (0x29, 0x37, 3),
            Ty::Bool => (0x2d, 0x3a, 0),
            // A reference field of a view stored where a shared reference to the view points.
            Ty::Console | Ty::Allocator | Ty::Tag | Ty::Borrow { .. } => (0x28, 0x36, 2),
            Ty::Buffer(_) | Ty::Input | Ty::Host(_) | Ty::I64 => (0x29, 0x37, 3),
            Ty::F64 => (0x2b, 0x39, 3),
            _ => unreachable!("only scalar storage is addressable"),
        };
        if store && ty == Ty::Bool { out.extend([0x45, 0x45]); }
        out.extend([if store { write } else { load }, align, 0]);
        if !store && ty == Ty::Bool { out.extend([0x45, 0x45]); }
    }
    pub(super) fn read_local(&self, out: &mut Vec<u8>, local: u32, ty: Ty) {
        if self.frame.slots.contains_key(&local) {
            self.address(out, local);
            Self::memory(out, ty, false);
        } else { get(out, local); }
    }
    // Consume one scalar already on the operand stack; preserve evaluation order.
    pub(super) fn write_local(&mut self, out: &mut Vec<u8>, local: u32, ty: Ty) {
        if self.frame.slots.contains_key(&local) {
            let temporary = self.temporary(ty);
            indexed(out, 0x21, temporary);
            self.address(out, local);
            get(out, temporary);
            Self::memory(out, ty, true);
        } else { indexed(out, 0x21, local); }
    }
    fn flat_index(&mut self, out: &mut Vec<u8>, place: &Place) -> u32 {
        out.push(0x42);
        signed(out, 0);
        for projection in &place.projections {
            match projection {
                Projection::Field { slot, .. } => {
                    out.push(0x42); signed(out, *slot as i64); out.push(0x7c);
                }
                Projection::Index { value, length, slots, .. } => {
                    let index = self.checked_index(out, value, *length);
                    get(out, index);
                    out.push(0x42); signed(out, *slots as i64);
                    out.extend([0x7e, 0x7c]); // proven-small flat-cell arithmetic
                }
            }
        }
        self.save_stack(out, Ty::U64)
    }
    pub(super) fn pointer(&mut self, out: &mut Vec<u8>, place: &Place) {
        if place.pointer { get(out, place.root); }
        else {
            get(out, self.base.expect("addressed root has a frame"));
            // An offset of zero adds nothing (`i32.const 0; i32.add` was written for it).
            let root = self.frame.roots[&place.root];
            if root != 0 { Self::i32_const(out, root); out.push(0x6a); }
        }
        for projection in &place.projections {
            match projection {
                Projection::Field { byte_offset, .. } => {
                    if *byte_offset != 0 { Self::i32_const(out, *byte_offset); out.push(0x6a); }
                }
                Projection::Index { value, length, stride, .. } => {
                    let index = self.checked_index(out, value, *length);
                    get(out, index);
                    out.push(0xa7); // only AFTER the U64 range check
                    Self::i32_const(out, *stride);
                    out.extend([0x6c, 0x6a]); // layout stride includes padding
                }
            }
        }
    }
    pub(super) fn locate(&mut self, out: &mut Vec<u8>, place: &Place) -> Location {
        if place.pointer && place.projections.is_empty() { return Location::Memory(place.root); }
        if !place.pointer {
            if let Some((slot, _)) = place.static_offsets() { return Location::Local(place.root + slot); }
            if !self.frame.roots.contains_key(&place.root) {
                return Location::Dynamic { root: place.root, root_ty: place.root_ty,
                    index: self.flat_index(out, place) };
            }
        }
        self.pointer(out, place);
        let pointer = self.temporary(Ty::Bool); // private i32 carrier
        indexed(out, 0x21, pointer);
        Location::Memory(pointer)
    }
    pub(super) fn read_place(&mut self, out: &mut Vec<u8>, place: &Place) {
        // In memory (a framed root, or through a reference) with indexes that change nothing: the
        // element is read at its address. The root is then read after the indexes are evaluated,
        // which is the same value, since evaluating them writes nothing.
        let in_memory = place.pointer || self.frame.roots.contains_key(&place.root);
        let quiet = place.projections.iter().all(|projection| match projection { Projection::Index { value, .. } => Self::quiet(value), _ => true });
        if place.static_offsets().is_none() && in_memory && quiet {
            let location = self.locate(out, place);
            self.read_location(out, &location, place.ty);
            return;
        }
        if place.static_offsets().is_some() {
            let location = self.locate(out, place);
            self.read_location(out, &location, place.ty);
        } else {
            // Preserve existing by-value rvalue evaluation: snapshot the root's
            // initialized cells BEFORE side effects in index expressions.
            if place.pointer { self.read_location(out, &Location::Memory(place.root), place.root_ty); }
            else { self.read_location(out, &Location::Local(place.root), place.root_ty); }
            let root = self.save_stack(out, place.root_ty);
            let index = self.flat_index(out, place);
            self.read_location(out, &Location::Dynamic { root, root_ty: place.root_ty, index }, place.ty);
        }
    }
    /// Whether evaluating `e` can change nothing and call nothing: numbers, reads of places
    /// with such indexes, and arithmetic on them (which may trap, and then nothing is read).
    fn quiet(e: &Expr) -> bool {
        match &e.kind {
            Value::Unit | Value::U64(_) | Value::Bool(_) => true,
            Value::ReadPlace(place) => place.projections.iter().all(|projection| match projection { Projection::Index { value, .. } => Self::quiet(value), _ => true }),
            Value::Binary(_, left, right) => Self::quiet(left) && Self::quiet(right),
            Value::Not(inner) | Value::Widen(inner) => Self::quiet(inner),
            _ => false,
        }
    }
    pub(super) fn read_location(&self, out: &mut Vec<u8>, location: &Location, ty: Ty) {
        match location {
            Location::Local(start) => {
                for (slot, ty) in self.layouts.flat(ty).into_iter().enumerate() {
                    self.read_local(out, start + slot as u32, ty);
                }
            }
            Location::Memory(pointer) => {
                let cells = self.layouts.cells(ty).expect("checked initialized-cell layout");
                let guards = self.layouts.guards(ty);
                let at: BTreeMap<u32, u64> = cells.iter().map(|cell| (cell.slot, cell.byte_offset)).collect();
                let pointer = *pointer;
                let read = |out: &mut Vec<u8>, offset: u64, ty: Ty| {
                    get(out, pointer);
                    if offset != 0 { Self::i32_const(out, offset as u32); out.push(0x6a); }
                    Self::memory(out, ty, false);
                };
                for cell in &cells {
                    read(out, cell.byte_offset, cell.ty);
                    // A variant's cell that is not live holds another variant's bytes: read as zero.
                    let guard = &guards[cell.slot as usize];
                    if !guard.is_empty() {
                        Self::zero_carrier(out, cell.ty);
                        Self::live(out, guard, &mut |out, tag| read(out, at[&tag], Ty::Tag));
                        out.push(0x1b);
                    }
                }
            }
            Location::Dynamic { root, root_ty, index } => {
                let root_cells = self.layouts.flat(*root_ty);
                for (slot, ty) in self.layouts.flat(ty).into_iter().enumerate() {
                    let mut candidates = root_cells.iter().enumerate().filter(|(_, candidate)| **candidate == ty);
                    let (first, _) = candidates.next().expect("projection has a matching initialized cell");
                    get(out, root + first as u32);
                    for (candidate, _) in candidates {
                        get(out, root + candidate as u32);
                        get(out, *index);
                        out.push(0x42); signed(out, slot as i64); out.push(0x7c);
                        out.push(0x42); signed(out, candidate as i64);
                        out.extend([0x51, 0x45, 0x1b]);
                    }
                }
            }
        }
    }
    /// The i32 truth that each (tag slot, tag) holds, with `tag` pushing a tag slot's value.
    pub(super) fn live(out: &mut Vec<u8>, guard: &[(u32, u8)], tag: &mut dyn FnMut(&mut Vec<u8>, u32)) {
        for (index, (slot, value)) in guard.iter().enumerate() {
            tag(out, *slot);
            Self::i32_const(out, u32::from(*value));
            out.push(0x46); // i32.eq
            if index > 0 { out.push(0x71); } // i32.and
        }
    }
    /// Write a value on the operand stack into the locals from `start`. Where some of them live
    /// in the frame, the variants of a stored sum share bytes, so a slot is written only while
    /// it is live; in registers every slot is written.
    pub(super) fn store_slots(&mut self, out: &mut Vec<u8>, start: u32, ty: Ty) {
        let flat = self.layouts.flat(ty);
        let guards = self.layouts.guards(ty);
        let framed = (0..flat.len() as u32).any(|slot| self.frame.slots.contains_key(&(start + slot)));
        if !framed || guards.iter().all(Vec::is_empty) {
            for (slot, ty) in flat.into_iter().enumerate().rev() { self.write_local(out, start + slot as u32, ty); }
            return;
        }
        let source = self.save_stack(out, ty);
        for (slot, ty) in flat.into_iter().enumerate() {
            let guard = &guards[slot];
            if !guard.is_empty() { Self::live(out, guard, &mut |out, tag| get(out, source + tag)); out.extend([0x04, 0x40]); }
            get(out, source + slot as u32);
            self.write_local(out, start + slot as u32, ty);
            if !guard.is_empty() { out.push(0x0b); }
        }
    }
    /// `[v; n]` of a scalar into an array that lives in the frame, for n of 8 or more: `v` once
    /// and a loop of stores, not n values on the stack and n stores (code size, which is
    /// compile time at every start: `[0; 40]` in the f64 printer was 40 unrolled stores). Only
    /// when the elements' frame offsets step evenly; anything else is written as before.
    pub(super) fn repeat_in_frame(&mut self, out: &mut Vec<u8>, local: u32, value: &Expr) -> bool {
        let Value::Repeat(inner, count) = &value.kind else { return false };
        let count = *count;
        let flat = self.layouts.flat(inner.ty);
        if count < 8 || flat.len() != 1 || !self.layouts.guards(inner.ty).iter().all(Vec::is_empty) { return false; }
        let element = flat[0];
        let Some(&first) = self.frame.slots.get(&local) else { return false };
        let Some(&second) = self.frame.slots.get(&(local + 1)) else { return false };
        let Some(stride) = second.checked_sub(first).filter(|stride| *stride > 0) else { return false };
        let evenly = (0..count).all(|k| k.checked_mul(stride).and_then(|step| first.checked_add(step)).is_some_and(|at| self.frame.slots.get(&(local + k)) == Some(&at)));
        let Some(end) = count.checked_mul(stride).filter(|_| evenly) else { return false };
        let source = self.capture(out, inner);
        let offset = self.temporary(Ty::Tag); // an i32: the byte offset from the first element
        Self::i32_const(out, 0); indexed(out, 0x21, offset);
        out.extend([0x03, 0x40]); // loop
        self.address(out, local); get(out, offset); out.push(0x6a); // i32.add
        get(out, source);
        Self::memory(out, element, true);
        get(out, offset); Self::i32_const(out, stride); out.push(0x6a); indexed(out, 0x22, offset); // local.tee
        Self::i32_const(out, end); out.push(0x49); // i32.lt_u
        out.extend([0x0d, 0x00]); // br_if 0: the next element
        out.push(0x0b); // end
        true
    }
    /// `b = a` for a whole array of one scalar type, both in the frame with even and equal
    /// strides, n of 8 or more: a loop of loads and stores, not n of each written out (a
    /// `[U64; 128]` copied was 3.8 KB of code; two of them were most of the f64 printer's 18.8 KB).
    /// Anything else is written as before.
    pub(super) fn copy_in_frame(&mut self, out: &mut Vec<u8>, local: u32, value: &Expr) -> bool {
        let Value::ReadPlace(place) = &value.kind else { return false };
        if place.pointer || !place.projections.is_empty() || place.root == local { return false; }
        let flat = self.layouts.flat(value.ty);
        let count = flat.len() as u32;
        if count < 8 || !flat.iter().all(|ty| *ty == flat[0]) || !self.layouts.guards(value.ty).iter().all(Vec::is_empty) { return false; }
        let element = flat[0];
        let even = |frame: &frame::Frame, start: u32| -> Option<(u32, u32)> {
            let first = *frame.slots.get(&start)?;
            let stride = frame.slots.get(&(start + 1))?.checked_sub(first).filter(|stride| *stride > 0)?;
            let evenly = (0..count).all(|k| k.checked_mul(stride).and_then(|step| first.checked_add(step)).is_some_and(|at| frame.slots.get(&(start + k)) == Some(&at)));
            if evenly { Some((first, stride)) } else { None }
        };
        let (Some((_, stride)), Some((_, from_stride))) = (even(&self.frame, local), even(&self.frame, place.root)) else { return false };
        if stride != from_stride { return false; }
        let Some(end) = count.checked_mul(stride) else { return false };
        let offset = self.temporary(Ty::Tag); // an i32: the byte offset from the first element
        Self::i32_const(out, 0); indexed(out, 0x21, offset);
        out.extend([0x03, 0x40]); // loop
        self.address(out, local); get(out, offset); out.push(0x6a); // destination
        self.address(out, place.root); get(out, offset); out.push(0x6a); // source
        Self::memory(out, element, false); // load
        Self::memory(out, element, true); // store
        get(out, offset); Self::i32_const(out, stride); out.push(0x6a); indexed(out, 0x22, offset); // local.tee
        Self::i32_const(out, end); out.push(0x49); // i32.lt_u
        out.extend([0x0d, 0x00]); // br_if 0: the next element
        out.push(0x0b); // end
        true
    }
    pub(super) fn write_location(&mut self, out: &mut Vec<u8>, location: &Location, ty: Ty) {
        if let Location::Local(start) = location {
            // The complete RHS is already on the operand stack before ANY write.
            self.store_slots(out, *start, ty);
            return;
        }
        let source = self.save_stack(out, ty); // materialize ALL overlapping RHS cells
        match location {
            Location::Memory(pointer) => {
                let guards = self.layouts.guards(ty);
                for cell in self.layouts.cells(ty).expect("checked initialized-cell layout") {
                    // Only the live variant's cells are written: the others share its bytes.
                    let guard = &guards[cell.slot as usize];
                    if !guard.is_empty() { Self::live(out, guard, &mut |out, tag| get(out, source + tag)); out.extend([0x04, 0x40]); }
                    get(out, *pointer);
                    if cell.byte_offset != 0 { Self::i32_const(out, cell.byte_offset as u32); out.push(0x6a); }
                    get(out, source + cell.slot);
                    Self::memory(out, cell.ty, true);
                    if !guard.is_empty() { out.push(0x0b); }
                }
            }
            Location::Dynamic { root, root_ty, index } => {
                for (slot, ty) in self.layouts.flat(ty).into_iter().enumerate() {
                    for (candidate, actual) in self.layouts.flat(*root_ty).into_iter().enumerate() {
                        if actual != ty { continue; }
                        get(out, *index);
                        out.push(0x42); signed(out, slot as i64); out.push(0x7c);
                        out.push(0x42); signed(out, candidate as i64);
                        out.extend([0x51, 0x04, 0x40]);
                        get(out, source + slot as u32);
                        indexed(out, 0x21, root + candidate as u32);
                        out.push(0x0b);
                    }
                }
            }
            Location::Local(_) => unreachable!(),
        }
    }
}
