use super::*;

impl Emitter<'_> {
    pub(super) fn expr(&mut self, out: &mut Vec<u8>, expr: &Expr) {
        match &expr.kind {
            Value::StaticStr(text) => {
                out.push(0x41);
                signed(out, i64::from(self.literals.address(text)));
                out.push(0x42);
                signed(out, text.len() as i64);
            }
            Value::Unit => {}
            Value::U64(n) => {
                out.push(0x42);
                signed(out, *n as i64);
            }
            Value::Bool(b) => out.extend([0x41, u8::from(*b)]),
            Value::EntryAllocator => call(out, self.indices.authorize.expect("typed allocator entry import")),
            Value::TryAlloc { allocator, capacity, type_id, prefix_only } => {
                self.expr(out, allocator); Self::memory(out, Ty::Allocator, false);
                let kind = if *prefix_only { crate::compiler::allocator::PREFIX_TYPE } else { 0 };
                out.push(0x42); signed(out, (u64::from(*type_id) | kind) as i64);
                self.expr(out, capacity); call(out, self.indices.try_alloc);
            }
            Value::AllocationKey(value, type_id) => {
                self.expr(out, value);
                Self::memory(out, Ty::Buffer(type_id - 1), false);
            }
            Value::Capacity(value, type_id) => {
                self.expr(out, value); Self::memory(out, Ty::Buffer(type_id - 1), false);
                out.push(0x42); signed(out, *type_id as i64); call(out, self.indices.capacity);
            }
            Value::TryRelease(value, type_id) => {
                self.expr(out, value); out.push(0x42); signed(out, *type_id as i64);
                call(out, self.indices.try_release);
                // The allocator ABI returns (tag, untouched owner or zero).
                if self.layouts.sum(expr.ty).is_some_and(|sum| sum.zero_arm.is_some()) {
                    let word = self.temporary(Ty::U64);
                    indexed(out, 0x21, word); out.push(0x1a); get(out, word);
                }
            }
            Value::Storage { op, receiver, index, value, buffer } =>
                self.storage(out, expr.ty, *op, receiver, index, value.as_deref(), *buffer),
            Value::EntryConsole => out.extend([0x41, 1]), // private main entry, never source construction
            Value::EntryInput => out.extend([0x42, 3]), // likewise, as an i64; the host checks for this value
            // `main`'s directory: the host hands over its handle, and refuses a right it did not grant.
            Value::EntryHost(host, code) => {
                out.push(0x42); signed(out, *code as i64);
                call(out, self.indices.host[host.family().ordinal()].expect("typed host authorizer import"));
            }
            Value::Move { slot, place } => {
                let _ = slot; // ownership transfer is explicit in typed IR, not an owner copy
                self.read_place(out, place);
            }
            Value::Extract { wrapper, place } => {
                let _ = wrapper; // checked whole-wrapper consumption, not general partial move
                self.read_place(out, place);
            }
            Value::Discard(value) => {
                self.expr(out, value);
                for _ in self.layouts.flat(value.ty) { out.push(0x1a); }
            }
            Value::ReadPlace(place) => self.read_place(out, place),
            Value::Address(place) => self.pointer(out, place),
            Value::Stored(place, value) => {
                // The temporary's value first, into its own frame cells; then its address.
                self.expr(out, value);
                self.store_slots(out, place.root, value.ty);
                self.pointer(out, place);
            }
            Value::Sum { arm, payload } => {
                let sum = self.layouts.sum(expr.ty).expect("checked Result constructor").clone();
                if let Some(zero) = sum.zero_arm {
                    self.expr(out, payload);
                    if *arm == zero { out.extend([0x42, 0]); }
                    return;
                }
                Self::i32_const(out, *arm as u32);
                for tag in 0..sum.arms.len() {
                    if tag == *arm as usize { self.expr(out, payload); }
                    else {
                        for carrier in self.layouts.flat(sum.arms[tag]) { Self::zero_carrier(out, carrier); }
                    }
                }
            }
            Value::Retag(value, map) => {
                // The wider sum has a lane for every arm. The source's lanes go to the lanes of the same tags,
                // active or not; the arms the source does not have are zero, as an inactive arm is.
                let from = self.layouts.sum(value.ty).expect("checked anonymous sum").clone();
                let to = self.layouts.sum(expr.ty).expect("checked anonymous sum").clone();
                let source = self.capture(out, value);
                let tag = self.temporary(Ty::Tag);
                for (old, new) in map.iter().enumerate() {
                    get(out, source); Self::variant_tag(out, from.zero_arm); Self::i32_const(out, old as u32);
                    out.extend([0x46, 0x04, 0x40]);
                    Self::i32_const(out, *new as u32); indexed(out, 0x21, tag);
                    out.push(0x0b);
                }
                if let Some(zero) = to.zero_arm {
                    get(out, tag); Self::i32_const(out, u32::from(zero)); out.extend([0x46, 0x04, 0x7e, 0x42, 0, 0x05]);
                    if let Some(old) = map.iter().position(|new| *new != zero) { get(out, source + from.starts[old]); } else { out.extend([0x42, 0]); }
                    out.push(0x0b);
                    return;
                }
                get(out, tag);
                for (at, arm) in to.arms.iter().enumerate() {
                    match map.iter().position(|new| *new as usize == at) {
                        Some(old) => for slot in 0..self.layouts.flat(*arm).len() as u32 { get(out, source + from.starts[old] + slot); },
                        None => for carrier in self.layouts.flat(*arm) { Self::zero_carrier(out, carrier); },
                    }
                }
            }
            Value::Aggregate(values) => {
                for value in values {
                    self.expr(out, value);
                }
            }
            Value::Repeat(value, count) => {
                let start = self.capture(out, value);
                for _ in 0..*count {
                    for slot in 0..self.layouts.flat(value.ty).len() as u32 {
                        get(out, start + slot);
                    }
                }
            }
            Value::Project(value, offset) => {
                let start = self.capture(out, value);
                for slot in 0..self.layouts.flat(expr.ty).len() as u32 {
                    get(out, start + offset + slot);
                }
            }
            Value::Index(value, index, length) => {
                let start = self.capture(out, value);
                let index_local = self.checked_index(out, index, *length);
                self.select(
                    out,
                    start,
                    index_local,
                    *length,
                    self.layouts.flat(expr.ty).len() as u32,
                );
            }
            Value::SliceIndex(slice, index, stride, tested) => {
                let start = self.capture(out, slice);
                let index = self.capture(out, index);
                // `index < len`, else trap; then the element's cells at first + index * stride.
                if *tested && !self.proved_slices { get(out, index); get(out, start + 1); out.push(0x5a); trap_for(out, 6); }
                get(out, start); get(out, index); out.push(0xa7);
                Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                let address = self.temporary(Ty::Borrow { mutable: false, pointee: crate::compiler::borrow::Pointee::Unsigned(crate::compiler::scalar::Scalar::U8) });
                indexed(out, 0x21, address);
                self.read_location(out, &Location::Memory(address), expr.ty);
            }
            Value::SliceAt(slice, index, stride, reach) => {
                let start = self.capture(out, slice);
                let index = self.capture(out, index);
                if *reach == crate::compiler::logical_body::Reach::Walk {
                    // Below the length by construction (`Reach::Walk`): the address alone.
                    get(out, start); get(out, index); out.push(0xa7);
                    Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                } else if *reach == crate::compiler::logical_body::Reach::Result {
                    // `Result<&T, U64>` is `[tag, address, code]`: Ok (tag 0) when `index < len`,
                    // with the address and a zero code; else Err (tag 1), no address, the index.
                    get(out, index); get(out, start + 1); out.push(0x5a);
                    get(out, start); get(out, index); out.push(0xa7);
                    Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                    Self::i32_const(out, 0);
                    get(out, index); get(out, start + 1); out.push(0x54); out.push(0x1b);
                    out.extend([0x42, 0]); get(out, index);
                    get(out, index); get(out, start + 1); out.push(0x54); out.push(0x1b);
                } else {
                    // `index < len`, else trap; then first + index * stride.
                    get(out, index); get(out, start + 1); out.push(0x5a); trap_for(out, 6);
                    get(out, start); get(out, index); out.push(0xa7);
                    Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                }
            }
            Value::SliceCopy(target, source, stride) => {
                let to = self.capture(out, target);
                let from = self.capture(out, source);
                // Equal lengths, else trap: every read is then inside the source and every write
                // inside the target, as the per-element tests would have found.
                get(out, to + 1); get(out, from + 1); out.push(0x52); trap_for(out, 8);
                // Whole words while eight bytes remain, then bytes. A slice's length times its
                // stride is the size of memory it covers, so it does not wrap.
                let bytes = self.temporary(Ty::U64);
                get(out, to + 1); out.push(0x42); signed(out, *stride as i64); out.push(0x7e); indexed(out, 0x21, bytes);
                let at = self.temporary(Ty::U64);
                out.extend([0x42, 0]); indexed(out, 0x21, at);
                for (width, load, store) in [(8i64, 0x29u8, 0x37u8), (1, 0x31, 0x3c)] {
                    // while at + width <= bytes: to[at] = from[at]; at += width
                    out.extend([0x02, 0x40, 0x03, 0x40]);
                    get(out, at); out.push(0x42); signed(out, width); out.push(0x7c); get(out, bytes); out.push(0x56); // > bytes
                    out.extend([0x0d, 1]);
                    get(out, to); get(out, at); out.extend([0xa7, 0x6a]);
                    get(out, from); get(out, at); out.extend([0xa7, 0x6a]);
                    out.extend([load, 0, 0]);
                    out.extend([store, 0, 0]);
                    get(out, at); out.push(0x42); signed(out, width); out.push(0x7c); indexed(out, 0x21, at);
                    out.extend([0x0c, 0, 0x0b, 0x0b]);
                }
            }
            Value::SliceReadLe64(slice, index) => {
                let start = self.capture(out, slice);
                let index = self.capture(out, index);
                // The function's proof showed the eight bytes inside the slice: no test.
                if !self.proved_slices {
                    get(out, index); get(out, start + 1); out.push(0x56); trap_for(out, 6);
                    get(out, start + 1); get(out, index); out.push(0x7d);
                    out.extend([0x42, 8, 0x54]); trap_for(out, 6);
                }
                get(out, start); get(out, index); out.extend([0xa7, 0x6a]);
                out.extend([0x29, 0, 0]);
            }
            Value::SlicePart(slice, from, to, stride, utf8) => {
                let start = self.capture(out, slice);
                let from = self.capture(out, from);
                let to = self.capture(out, to);
                // `from <= to <= len`, else trap.
                get(out, from); get(out, to); out.push(0x56); trap_for(out, 8);
                get(out, to); get(out, start + 1); out.push(0x56); trap_for(out, 8);
                if *utf8 {
                    for endpoint in [from, to] {
                        // Length is a valid boundary without a load. Otherwise, a continuation
                        // byte (10xxxxxx) is the only invalid boundary in well-formed UTF-8.
                        get(out, endpoint); get(out, start + 1); out.extend([0x52, 0x04, 0x40]);
                        get(out, start); get(out, endpoint); out.extend([0xa7, 0x6a, 0x31, 0, 0]);
                        out.push(0x42); signed(out, 192); out.push(0x83);
                        out.push(0x42); signed(out, 128); out.push(0x51);
                        trap_for(out, 10);
                        out.push(0x0b);
                    }
                }
                get(out, start); get(out, from); out.push(0xa7);
                Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                get(out, to); get(out, from); out.push(0x7d);
            }
            // As `SlicePart` without the range test where the function's proof showed the range,
            // and on text a count of zero, not a trap, where an end is inside a scalar.
            Value::SliceWithin(slice, from, to, stride, utf8) => {
                let start = self.capture(out, slice);
                let from = self.capture(out, from);
                let to = self.capture(out, to);
                if !self.proved_slices {
                    get(out, from); get(out, to); out.push(0x56); trap_for(out, 8);
                    get(out, to); get(out, start + 1); out.push(0x56); trap_for(out, 8);
                }
                get(out, start); get(out, from); out.push(0xa7);
                Self::i32_const(out, *stride); out.extend([0x6c, 0x6a]);
                get(out, to); get(out, from); out.push(0x7d);
                if *utf8 {
                    for endpoint in [from, to] {
                        // One where the end is the text's or starts a scalar, else zero; times the count.
                        get(out, endpoint); get(out, start + 1); out.extend([0x52, 0x04, 0x7e]);
                        get(out, start); get(out, endpoint); out.extend([0xa7, 0x6a, 0x31, 0, 0]);
                        out.push(0x42); signed(out, 192); out.push(0x83);
                        out.push(0x42); signed(out, 128); out.extend([0x52, 0xad]);
                        out.extend([0x05, 0x42, 0x01, 0x0b]);
                        out.push(0x7e);
                    }
                }
            }
            Value::Widen(value) => self.expr(out, value),
            Value::Signed(op, operands) => {
                for operand in operands { self.expr(out, operand); }
                self.signed(out, *op);
            }
            // The engine's IEEE operations; only `ToI64` (i64.trunc_f64_s) traps, on NaN and out of range.
            // `F64.bits` of a NaN is the one canonical NaN's bits: a NaN's payload is the only
            // thing about it that may differ between machines, and this is the only place it
            // could be seen, so the engine need not canonicalize every operation (5x on n-body).
            Value::Float(crate::compiler::ir::Float::Bits, operands) => {
                for operand in operands { self.expr(out, operand); }
                let value = self.temporary(Ty::F64);
                indexed(out, 0x21, value);
                out.push(0x42); signed(out, 0x7ff8_0000_0000_0000u64 as i64);
                get(out, value); out.push(0xbd);
                get(out, value); get(out, value); out.push(0x62); // f64.ne: true only for NaN
                out.push(0x1b);
            }
            Value::BoxOp(op, id, args) => self.box_operation(out, *op, *id, args, expr.ty),
            Value::Bits(crate::compiler::ir::Bits::Packed(packed), operands) => self.packed(out, *packed, operands),
            Value::Bits(crate::compiler::ir::Bits::MulHigh, operands) => self.mul_high(out, operands),
            Value::Bits(op, operands) => {
                use crate::compiler::ir::Bits as B;
                let Ty::Unsigned(scalar) = expr.ty else { unreachable!("bit operations are typed unsigned") };
                let maximum = scalar.layout().maximum();
                let width = 64 - maximum.leading_zeros() as i64;
                for operand in operands { self.expr(out, operand); }
                match op {
                    B::Packed(_) | B::MulHigh => unreachable!(),
                    B::TrailingZeros => out.push(0x7a), B::CountOnes => out.push(0x7b),
                    B::WrapAdd => out.push(0x7c), B::WrapSub => out.push(0x7d), B::WrapMul => out.push(0x7e),
                    B::And => out.push(0x83),
                    B::Or => out.push(0x84),
                    B::Xor => out.push(0x85),
                    // Every bit of the type flipped: xor with its largest value.
                    B::Not => { out.push(0x42); signed(out, maximum as i64); out.push(0x85); }
                    B::Shl | B::Shr => {
                        // A shift by the width or more traps (Wasm would take the amount modulo 64).
                        let amount = self.temporary(Ty::U64);
                        indexed(out, 0x22, amount);
                        out.push(0x42); signed(out, width);
                        out.push(0x5a);
                        trap_for(out, 7);
                        get(out, amount);
                        if *op == B::Shl {
                            out.push(0x86);
                            // A narrow type keeps only its own bits.
                            if maximum != u64::MAX { out.push(0x42); signed(out, maximum as i64); out.push(0x83); }
                        } else { out.push(0x88); }
                    }
                }
            }
            // A literal's bits are known: `f64.const` rather than an i64 reinterpreted.
            Value::Float(crate::compiler::ir::Float::FromBits, operands) if matches!(operands[0].kind, Value::U64(_)) => {
                let Value::U64(bits) = operands[0].kind else { unreachable!() };
                out.push(0x44); out.extend(bits.to_le_bytes());
            }
            Value::Float(op, operands) => {
                use crate::compiler::ir::Float as F;
                for operand in operands { self.expr(out, operand); }
                out.push(match op {
                    F::Add => 0xa0, F::Sub => 0xa1, F::Mul => 0xa2, F::Div => 0xa3, F::Neg => 0x9a,
                    F::Eq => 0x61, F::Ne => 0x62, F::Lt => 0x63, F::Gt => 0x64, F::Le => 0x65, F::Ge => 0x66,
                    F::FromBits => 0xbf, F::Bits => 0xbd, F::FromU64 => 0xba, F::FromI64 => 0xb9, F::ToI64 => 0xb0,
                    F::Sqrt => 0x9f, F::Abs => 0x99, F::Floor => 0x9c,
                });
            }
            Value::Not(value) => {
                self.expr(out, value);
                out.push(0x45);
            }
            Value::Call(index, args) => {
                for arg in args {
                    self.expr(out, arg);
                }
                call(out, index + self.indices.source);
                if self.resumable && self.tasks.pausing[*index as usize] { self.unwinding(out); }
            }
            Value::Pause(token) => {
                // Going back in, the pause is where the task resumes: it returns. Otherwise it
                // records what the task waits for and starts the unwinding. Only a task pauses.
                // Going back in, it returns the word the step handed the task (in the token's
                // place); pausing, the value is never read.
                indexed(out, 0x23, STATE); Self::i32_const(out, REWIND); out.extend([0x46, 0x04, 0x7e]);
                Self::i32_const(out, NORMAL); indexed(out, 0x24, STATE);
                indexed(out, 0x23, TASK); Self::i32_const(out, TOKEN); out.push(0x6a); out.extend([0x29, 3, 0]);
                out.push(0x05);
                indexed(out, 0x23, TASK); out.push(0x45); trap_if(out);
                indexed(out, 0x23, TASK); Self::i32_const(out, TOKEN); out.push(0x6a); self.expr(out, token); out.extend([0x37, 3, 0]);
                Self::i32_const(out, UNWIND); indexed(out, 0x24, STATE);
                out.extend([0x42, 0]);
                out.push(0x0b);
                if self.resumable { self.unwinding(out); }
            }
            Value::TaskSpawn { allocator, bytes, entry, buffer } => self.spawn(out, expr.ty, allocator, bytes, entry, *buffer),
            Value::TaskStep(task, word) => self.step(out, expr.ty, task, word),
            // The word in the running task's token place; outside a task, a trap.
            // Outside a task, 0: nothing was handed in.
            Value::TaskWord => {
                indexed(out, 0x23, TASK); out.push(0x45);
                out.extend([0x04, 0x7e]); out.extend([0x42, 0]);
                out.push(0x05); indexed(out, 0x23, TASK); Self::i32_const(out, TOKEN); out.push(0x6a); out.extend([0x29, 3, 0]);
                out.push(0x0b);
            }
            // Whether a task is running: its place is not 0.
            Value::InTask => { indexed(out, 0x23, TASK); out.push(0x45); out.push(0x45); }
            Value::TaskStatus(task) => {
                self.expr(out, task); Self::memory(out, Ty::Host(crate::compiler::host::Host::Task), false);
                let token = self.temporary(Ty::U64); indexed(out, 0x21, token);
                let base = self.task_base(out, token);
                Self::heap_address(out, base, STATUS as u64); out.extend([0x28, 2, 0, 0xad]);
            }
            Value::TaskFinish(task, buffer) => {
                // Only a finished task's memory goes back: a paused one's frames are in it.
                self.expr(out, task);
                let token = self.temporary(Ty::U64); indexed(out, 0x21, token);
                let base = self.task_base(out, token);
                Self::heap_address(out, base, STATUS as u64); out.extend([0x28, 2, 0]);
                Self::i32_const(out, 2); out.push(0x47); trap_if(out);
                get(out, token); out.push(0x42); signed(out, *buffer as i64); call(out, self.indices.try_release);
                out.push(0x1a); trap_if(out);
            }
            Value::Read(receiver, run) => {
                self.read_place(out, receiver); // the grant, read through the checked shared permission
                call(out, if *run { self.indices.read_run.expect("typed input import") } else { self.indices.read.expect("typed input import") });
            }
            // A socket's or listener's handle number is the value it holds: no host call. (The
            // host checks the number wherever it is used, as it checks every handle.)
            Value::Host(crate::compiler::host::HostOp::NetHandle, Some(receiver), _) => self.read_place(out, receiver),
            Value::Host(op, receiver, operands) => {
                // (handle, number) -> number. The handle is read through a checked loan, or moved.
                if let Some(receiver) = receiver { self.read_place(out, receiver); }
                for operand in operands {
                    self.expr(out, operand);
                    // A byte slice crosses as one number: `address | length << 32`.
                    if matches!(operand.ty, Ty::Borrow { pointee: crate::compiler::borrow::Pointee::Slice(_), .. }) {
                        let flat = self.layouts.flat(operand.ty);
                        let held = self.save_stack(out, operand.ty);
                        self.read_local(out, held, flat[0]);
                        out.push(0xad); // i64.extend_i32_u
                        self.read_local(out, held + 1, flat[1]);
                        out.extend([0x42, 32, 0x86, 0x84]); // i64.const 32, i64.shl, i64.or
                    }
                }
                call(out, self.indices.host[op.family().ordinal()].expect("typed host import") + op.offset());
                if expr.ty == Ty::Unit { out.push(0x1a); }
            }
            Value::Print(receiver, value, word) => {
                self.read_place(out, receiver); // checked shared permission, not a source owner copy
                self.expr(out, value);
                let byte = value.ty == Ty::Unsigned(crate::compiler::scalar::Scalar::U8);
                let import = if *word { self.indices.print_word } else if byte { self.indices.print_byte } else { self.indices.print };
                call(out, import.expect("typed output import"));
            }
            Value::Exchange(left, right) => {
                // Both places are found before either is read, and the left value is held in locals
                // while the right one is written over it.
                let (first, second) = (self.locate(out, left), self.locate(out, right));
                self.read_location(out, &first, left.ty);
                let held = self.save_stack(out, left.ty);
                self.read_location(out, &second, left.ty);
                self.write_location(out, &first, left.ty);
                for (slot, ty) in self.layouts.flat(left.ty).into_iter().enumerate() { self.read_local(out, held + slot as u32, ty); }
                self.write_location(out, &second, left.ty);
            }
            Value::Binary(op @ (Binary::And | Binary::Or), left, right) => {
                self.expr(out, left);
                out.extend([0x04, 0x7f]);
                if matches!(op, Binary::And) {
                    self.expr(out, right);
                } else {
                    out.extend([0x41, 1]);
                }
                out.push(0x05);
                if matches!(op, Binary::Or) {
                    self.expr(out, right);
                } else {
                    out.extend([0x41, 0]);
                }
                out.push(0x0b);
            }
            // `x * c` with a constant `c`: one comparison, `x > MAX / c`, where the general check tests
            // both operands against 2^32 and divides back. The same trap (3) and the same product.
            Value::Binary(Binary::Mul, left, right) if !self.proved
                && (matches!(right.kind, Value::U64(c) if c > 1) || matches!(left.kind, Value::U64(c) if c > 1)) => {
                let (other, c) = match (&left.kind, &right.kind) { (_, Value::U64(c)) if *c > 1 => (left, *c), (Value::U64(c), _) => (right, *c), _ => unreachable!() };
                self.expr(out, other);
                let a = self.scratch();
                indexed(out, 0x22, a);
                out.push(0x42); signed(out, (u64::MAX / c) as i64);
                out.push(0x56); // i64.gt_u
                trap_for(out, 3);
                get(out, a);
                out.push(0x42); signed(out, c as i64);
                out.push(0x7e);
                if let Ty::Unsigned(scalar) = expr.ty { self.narrow(out, scalar); }
            }
            Value::Binary(op, left, right) => {
                self.expr(out, left);
                self.expr(out, right);
                match op {
                    // Shown in range by the function's proof: the plain operation.
                    Binary::Add | Binary::Sub | Binary::Mul if self.proved => out.push(match op { Binary::Add => 0x7c, Binary::Sub => 0x7d, _ => 0x7e }),
                    Binary::Add | Binary::Sub | Binary::Mul => self.checked(out, *op),
                    Binary::Div => out.push(0x80),
                    Binary::Rem => out.push(0x82),
                    Binary::Eq => out.push(if matches!(left.ty, Ty::Unsigned(_)) {
                        0x51
                    } else {
                        0x46
                    }),
                    Binary::Ne => out.push(if matches!(left.ty, Ty::Unsigned(_)) {
                        0x52
                    } else {
                        0x47
                    }),
                    Binary::Lt => out.push(0x54),
                    Binary::Gt => out.push(0x56),
                    Binary::Le => out.push(0x58),
                    Binary::Ge => out.push(0x5a),
                    Binary::And | Binary::Or => unreachable!(),
                }
                if let Ty::Unsigned(scalar) = expr.ty && !(self.proved && matches!(op, Binary::Add | Binary::Sub | Binary::Mul)) {
                    self.narrow(out, scalar);
                }
            }
        }
    }
}
