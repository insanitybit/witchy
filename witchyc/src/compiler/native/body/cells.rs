//! Proved cell accesses written in place of a call to the runtime's `witchy_cell` or
//! `witchy_read_range` (runtime/native/heap.rs).
//!
//! An operation the source admits only under a kernel-checked proof (`StorageOp::proof_admitted`:
//! `memory.init_unchecked`, `take_unchecked`, `get_unchecked`, ...) is emitted only after
//! `verify_build` accepted every obligation of its function, and those obligations are exactly
//! what the runtime would test: the element type, the index below the capacity, and the cell
//! empty to initialize or full to read or take. So the inline form tests none of them. It keeps:
//!
//! - for a `Buffer`, the handle's slot and generation, as `live` does: a stale or forged handle
//!   traps, and a `Buffer` can issue keys, links and weak references that outlive it. A
//!   `PrefixBuffer` issues none and has one owner, so in a default build its value is its
//!   header's address, as a Rust `Vec` holds a pointer (`Body::direct`, storage.rs);
//! - a `PrefixBuffer`'s prefix rule (an initialization at the prefix, a take of the last cell):
//!   dense storage is the runtime's representation, which the proof model does not know of;
//! - the bookkeeping, as `Buffer::mark` and `Buffer::range` keep it: the bitmap bit, the count of
//!   occupied cells and the prefix, which release and unproved code rely on.
//!
//! `WITCHY_NATIVE_CHECKED=1` at compile time (checked mode, native/mod.rs), or
//! `WITCHY_NATIVE_RUNTIME_CELLS=1` for the cells alone, keeps every call, and a run with
//! `WITCHY_HEAP_AUDIT=1` recounts each buffer's bookkeeping when it is released; together they are
//! the check in scripts/native-checked.py.
use super::{Body, I64, IntCC, InstBuilder, MemFlagsData, Result, TRAP, Ty, Value};
use cranelift_codegen::ir::types::{I8, I32};

/// The runtime's `Buffer` header (`#[repr(C)]`): capacity, stride, bytes, occupied (u32),
/// prefix (u32), kind; the payload follows it, and a sparse buffer's bitmap follows the payload.
const CAPACITY: i32 = 0;
const STRIDE: i32 = 8;
const OCCUPIED: i32 = 24;
const PREFIX: i32 = 28;
const KIND: i32 = 32;
const HEADER: i64 = 40;
/// The kind bit of a buffer stored as a prefix alone (runtime `PREFIX`, compiler `PREFIX_TYPE`).
const DENSE: i64 = 1 << 29;
/// A slot of the runtime's table (`#[repr(C)] Slot`): the buffer at 0, its generation at 8.
const SLOT_BYTES: i64 = 16;
const SLOT_GENERATION: i32 = 8;
/// `Heap::slots`'s length in runtime/native/heap.rs.
const SLOTS: i64 = 1 << 18;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Change {
    Initialize,
    Take,
    /// A read as `Buffer::range` makes it: one that reaches the prefix moves the prefix on.
    Read,
    /// A read as `witchy_cell` makes it: nothing changes.
    Peek,
}

/// Whether proved cell accesses are written in (the default) or left as runtime calls.
pub(super) fn inline() -> bool {
    !super::super::checked() && std::env::var_os("WITCHY_NATIVE_RUNTIME_CELLS").is_none()
}

/// An observed build (`WITCHY_NATIVE_OBSERVED=1` when compiling): proved accesses to a
/// `PrefixBuffer` are written in as in a default build, and each first asks the runtime
/// (`witchy_observe`) to judge the access from its own state and say the address it must reach;
/// a disagreement traps. The buffer is named by slot and generation there, as in checked mode.
pub(super) fn observed() -> bool {
    std::env::var_os("WITCHY_NATIVE_OBSERVED").is_some()
}

/// Whether every buffer is named by a slot and generation, a `PrefixBuffer` too.
pub(super) fn handles() -> bool {
    super::super::checked() || observed()
}

impl Body<'_, '_> {
    /// The buffer header a handle names, as the runtime's `live` finds it.
    fn live_inline(&mut self, handle: Value) -> Result<Value> {
        let flags = MemFlagsData::new();
        let id = self.fb.ins().band_imm_u(handle, 0xffff_ffff);
        let index = self.fb.ins().iadd_imm_s(id, -1);
        let outside = self.fb.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, index, SLOTS);
        self.fb.ins().trapnz(outside, TRAP);
        let table = self.fb.ins().symbol_value(I64, self.heap);
        let offset = self.fb.ins().imul_imm_u(index, SLOT_BYTES);
        let slot = self.fb.ins().iadd(table, offset);
        let generation = self.fb.ins().uload32(flags, slot, SLOT_GENERATION);
        let wanted = self.fb.ins().ushr_imm_u(handle, 32);
        let stale = self.fb.ins().icmp(IntCC::NotEqual, generation, wanted);
        self.fb.ins().trapnz(stale, TRAP);
        let buffer = self.fb.ins().load(I64, flags, slot, 0);
        // A released slot keeps its generation and holds no buffer.
        self.fb.ins().trapz(buffer, TRAP);
        Ok(buffer)
    }

    /// The header of the buffer `handle` names. A `PrefixBuffer`'s lane is that address in a
    /// default build (`direct`); any other lane is a slot and generation, looked up as `live` does.
    fn header(&mut self, handle: Value, buffer: u32) -> Result<Value> {
        if self.direct(buffer)? { Ok(handle) } else { self.live_inline(handle) }
    }

    /// The address of cell `index` (or of the run `index..index + count`, for a read) after
    /// marking it as `change` does, for an operation whose proof showed the cell's state.
    ///
    /// The distance between cells is the size of `element` as the compiler lays it out, which
    /// is the stride the allocation was asked for (`allocation` and `boxed`, with the same
    /// `buffer_stride`): the header's copy of it is not read. The buffer holds this element
    /// type, and is dense or sparse, because the handle's type says so, as the proof took it.
    pub(super) fn proved_cells(&mut self, handle: Value, buffer_type: u32, element: Ty, index: Value, count: Value, change: Change) -> Result<Value> {
        // A size is 32 bits wide, so it is the same number signed.
        let stride = self.buffer_stride(element)? as i64;
        let flags = MemFlagsData::new();
        // Nothing read before may be taken from a cache across a store to the heap.
        self.reads.clear();
        let dense = self.dense(buffer_type)?;
        // An observed build asks the runtime what this access must come to before making it.
        let expected = if dense && observed() {
            let kind = self.constant(u64::from(buffer_type) + 1);
            let mode = self.constant(match change { Change::Initialize => 0, Change::Take => 1, Change::Read | Change::Peek => 2 });
            Some(self.runtime_value(super::storage::Runtime::Observe, &[handle, kind, index, count, mode])?)
        } else {
            None
        };
        let buffer = self.header(handle, buffer_type)?;
        let prefix = self.fb.ins().uload32(flags, buffer, PREFIX);
        let end = self.fb.ins().iadd(index, count);

        if dense {
            // Dense: the prefix is the state. An initialization must be at it and a take of its
            // last cell; a read changes nothing.
            match change {
                Change::Initialize => {
                    let elsewhere = self.fb.ins().icmp(IntCC::NotEqual, index, prefix);
                    self.fb.ins().trapnz(elsewhere, TRAP);
                    let next = self.fb.ins().ireduce(I32, end);
                    self.fb.ins().store(flags, next, buffer, PREFIX);
                }
                Change::Take => {
                    let elsewhere = self.fb.ins().icmp(IntCC::NotEqual, end, prefix);
                    self.fb.ins().trapnz(elsewhere, TRAP);
                    let next = self.fb.ins().ireduce(I32, index);
                    self.fb.ins().store(flags, next, buffer, PREFIX);
                }
                Change::Read | Change::Peek => {
                    // A read must lie below the prefix, as `permits` asks. The proof models cells
                    // one by one and knows nothing of a dense buffer's prefix.
                    let some = self.fb.ins().icmp_imm_u(IntCC::NotEqual, count, 0);
                    let past = self.fb.ins().icmp(IntCC::UnsignedGreaterThan, end, prefix);
                    let wrong = self.fb.ins().band(some, past);
                    self.fb.ins().trapnz(wrong, TRAP);
                }
            }
        } else {
            // Sparse: one bit a cell after the payload, the count of full cells, and a prefix
            // that is a lower bound of the leading full cells.
            match change {
                Change::Peek => {}
                Change::Read => {
                    // As `Buffer::range`: a read that reaches the prefix shows the cells it read full.
                    let from_below = self.fb.ins().icmp(IntCC::UnsignedLessThanOrEqual, index, prefix);
                    let past = self.fb.ins().icmp(IntCC::UnsignedGreaterThan, end, prefix);
                    let both = self.fb.ins().band(from_below, past);
                    let next = self.fb.ins().select(both, end, prefix);
                    let next = self.fb.ins().ireduce(I32, next);
                    self.fb.ins().store(flags, next, buffer, PREFIX);
                }
                Change::Initialize | Change::Take => {
                    let capacity = self.fb.ins().load(I64, flags, buffer, CAPACITY);
                    let payload_bytes = self.fb.ins().imul_imm_u(capacity, stride);
                    let bitmap = self.fb.ins().iadd_imm_s(buffer, HEADER);
                    let bitmap = self.fb.ins().iadd(bitmap, payload_bytes);
                    let byte_index = self.fb.ins().ushr_imm_u(index, 3);
                    let byte_address = self.fb.ins().iadd(bitmap, byte_index);
                    let byte = self.fb.ins().uload8(I64, flags, byte_address, 0);
                    let bit = self.fb.ins().band_imm_u(index, 7);
                    let one = self.fb.ins().iconst(I64, 1);
                    let mask = self.fb.ins().ishl(one, bit);
                    let occupied = self.fb.ins().uload32(flags, buffer, OCCUPIED);
                    let (byte, occupied, next) = if change == Change::Initialize {
                        let byte = self.fb.ins().bor(byte, mask);
                        let occupied = self.fb.ins().iadd_imm_s(occupied, 1);
                        // As `heap_range::mark`: the prefix moves only when this cell is at it.
                        let at = self.fb.ins().icmp(IntCC::Equal, index, prefix);
                        (byte, occupied, self.fb.ins().select(at, end, prefix))
                    } else {
                        let keep = self.fb.ins().bnot(mask);
                        let byte = self.fb.ins().band(byte, keep);
                        let occupied = self.fb.ins().iadd_imm_s(occupied, -1);
                        (byte, occupied, self.fb.ins().umin(prefix, index))
                    };
                    let byte = self.fb.ins().ireduce(I8, byte);
                    self.fb.ins().store(flags, byte, byte_address, 0);
                    let occupied = self.fb.ins().ireduce(I32, occupied);
                    self.fb.ins().store(flags, occupied, buffer, OCCUPIED);
                    let next = self.fb.ins().ireduce(I32, next);
                    self.fb.ins().store(flags, next, buffer, PREFIX);
                }
            }
        }
        let offset = self.fb.ins().imul_imm_u(index, stride);
        let payload = self.fb.ins().iadd_imm_s(buffer, HEADER);
        let address = self.fb.ins().iadd(payload, offset);
        if let Some(expected) = expected {
            let other = self.fb.ins().icmp(IntCC::NotEqual, address, expected);
            self.fb.ins().trapnz(other, TRAP);
        }
        Ok(address)
    }

    /// A checked cell access, as `witchy_cell` makes it, without the call: the element type
    /// (a mismatch traps), then status 1 for an index past the capacity and 2 for a cell in the
    /// wrong state, else 0 with the cell marked. The address is the cell's when the status is 0.
    pub(super) fn checked_cell(&mut self, handle: Value, type_id: Value, index: Value, change: Change) -> Result<(Value, Value)> {
        let flags = MemFlagsData::new();
        self.reads.clear();
        let buffer = self.live_inline(handle)?;
        let kind = self.fb.ins().load(I64, flags, buffer, KIND);
        let declared = self.fb.ins().band_imm_u(kind, !DENSE);
        let other = self.fb.ins().icmp(IntCC::NotEqual, declared, type_id);
        self.fb.ins().trapnz(other, TRAP);
        let capacity = self.fb.ins().load(I64, flags, buffer, CAPACITY);
        let stride = self.fb.ins().load(I64, flags, buffer, STRIDE);

        let inside = self.fb.create_block();
        let allowed = self.fb.create_block();
        let done = self.fb.create_block();
        self.fb.append_block_param(done, I64);
        let past = self.fb.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, capacity);
        let one = self.fb.ins().iconst(I64, 1);
        self.fb.ins().brif(past, done, &[one.into()], inside, &[]);

        // `Buffer::permits` for one cell.
        self.fb.switch_to_block(inside);
        self.fb.seal_block(inside);
        let prefix = self.fb.ins().uload32(flags, buffer, PREFIX);
        let dense = self.fb.ins().band_imm_u(kind, DENSE);
        let next = self.fb.ins().iadd_imm_s(index, 1);
        let dense_ok = match change {
            Change::Initialize => self.fb.ins().icmp(IntCC::Equal, index, prefix),
            Change::Take => self.fb.ins().icmp(IntCC::Equal, next, prefix),
            Change::Read | Change::Peek => self.fb.ins().icmp(IntCC::UnsignedLessThanOrEqual, next, prefix),
        };
        // The bitmap is read only for a sparse buffer, which has one; a dense buffer's byte
        // would lie past its allocation.
        let sparse_block = self.fb.create_block();
        let judged = self.fb.create_block();
        self.fb.append_block_param(judged, I64);
        let dense_ok = self.fb.ins().uextend(I64, dense_ok);
        self.fb.ins().brif(dense, judged, &[dense_ok.into()], sparse_block, &[]);
        self.fb.switch_to_block(sparse_block);
        self.fb.seal_block(sparse_block);
        let payload_bytes = self.fb.ins().imul(capacity, stride);
        let bitmap = self.fb.ins().iadd_imm_s(buffer, HEADER);
        let bitmap = self.fb.ins().iadd(bitmap, payload_bytes);
        let byte_index = self.fb.ins().ushr_imm_u(index, 3);
        let byte_address = self.fb.ins().iadd(bitmap, byte_index);
        let byte = self.fb.ins().uload8(I64, flags, byte_address, 0);
        let bit = self.fb.ins().band_imm_u(index, 7);
        let shifted = self.fb.ins().ushr(byte, bit);
        let full = self.fb.ins().band_imm_u(shifted, 1);
        let sparse_ok = match change {
            Change::Initialize => self.fb.ins().icmp_imm_u(IntCC::Equal, full, 0),
            _ => {
                let below = self.fb.ins().icmp(IntCC::UnsignedLessThan, index, prefix);
                let set = self.fb.ins().icmp_imm_u(IntCC::NotEqual, full, 0);
                self.fb.ins().bor(below, set)
            }
        };
        let sparse_ok = self.fb.ins().uextend(I64, sparse_ok);
        self.fb.ins().jump(judged, &[sparse_ok.into()]);
        self.fb.switch_to_block(judged);
        self.fb.seal_block(judged);
        let ok = self.fb.block_params(judged)[0];
        let two = self.fb.ins().iconst(I64, 2);
        self.fb.ins().brif(ok, allowed, &[], done, &[two.into()]);

        // Allowed: marked as `Buffer::mark` marks it (a read changes nothing).
        self.fb.switch_to_block(allowed);
        self.fb.seal_block(allowed);
        if change != Change::Peek && change != Change::Read {
            self.mark_one(buffer, kind, index, change)?;
        }
        let zero = self.fb.ins().iconst(I64, 0);
        self.fb.ins().jump(done, &[zero.into()]);

        self.fb.switch_to_block(done);
        self.fb.seal_block(done);
        let status = self.fb.block_params(done)[0];
        let offset = self.fb.ins().imul(index, stride);
        let payload = self.fb.ins().iadd_imm_s(buffer, HEADER);
        Ok((status, self.fb.ins().iadd(payload, offset)))
    }

    /// Mark one allowed cell initialized or taken, as `heap_range::mark` does.
    fn mark_one(&mut self, buffer: Value, kind: Value, index: Value, change: Change) -> Result<()> {
        let flags = MemFlagsData::new();
        let dense = self.fb.ins().band_imm_u(kind, DENSE);
        let prefix = self.fb.ins().uload32(flags, buffer, PREFIX);
        let end = self.fb.ins().iadd_imm_s(index, 1);
        let dense_block = self.fb.create_block();
        let sparse_block = self.fb.create_block();
        let done = self.fb.create_block();
        self.fb.ins().brif(dense, dense_block, &[], sparse_block, &[]);
        self.fb.switch_to_block(dense_block);
        self.fb.seal_block(dense_block);
        let next = if change == Change::Initialize { end } else { index };
        let next = self.fb.ins().ireduce(I32, next);
        self.fb.ins().store(flags, next, buffer, PREFIX);
        self.fb.ins().jump(done, &[]);
        self.fb.switch_to_block(sparse_block);
        self.fb.seal_block(sparse_block);
        let capacity = self.fb.ins().load(I64, flags, buffer, CAPACITY);
        let stride = self.fb.ins().load(I64, flags, buffer, STRIDE);
        let payload_bytes = self.fb.ins().imul(capacity, stride);
        let bitmap = self.fb.ins().iadd_imm_s(buffer, HEADER);
        let bitmap = self.fb.ins().iadd(bitmap, payload_bytes);
        let byte_index = self.fb.ins().ushr_imm_u(index, 3);
        let byte_address = self.fb.ins().iadd(bitmap, byte_index);
        let byte = self.fb.ins().uload8(I64, flags, byte_address, 0);
        let bit = self.fb.ins().band_imm_u(index, 7);
        let one = self.fb.ins().iconst(I64, 1);
        let mask = self.fb.ins().ishl(one, bit);
        let occupied = self.fb.ins().uload32(flags, buffer, OCCUPIED);
        let (byte, occupied, next) = if change == Change::Initialize {
            let byte = self.fb.ins().bor(byte, mask);
            let occupied = self.fb.ins().iadd_imm_s(occupied, 1);
            let at = self.fb.ins().icmp(IntCC::Equal, index, prefix);
            (byte, occupied, self.fb.ins().select(at, end, prefix))
        } else {
            let keep = self.fb.ins().bnot(mask);
            let byte = self.fb.ins().band(byte, keep);
            let occupied = self.fb.ins().iadd_imm_s(occupied, -1);
            (byte, occupied, self.fb.ins().umin(prefix, index))
        };
        let byte = self.fb.ins().ireduce(I8, byte);
        self.fb.ins().store(flags, byte, byte_address, 0);
        let occupied = self.fb.ins().ireduce(I32, occupied);
        self.fb.ins().store(flags, occupied, buffer, OCCUPIED);
        let next = self.fb.ins().ireduce(I32, next);
        self.fb.ins().store(flags, next, buffer, PREFIX);
        self.fb.ins().jump(done, &[]);
        self.fb.switch_to_block(done);
        self.fb.seal_block(done);
        Ok(())
    }

    /// A proved initialization of `count` cells from `source`, or a proved take of the first
    /// `count`, written in for a dense buffer (the prefix rule kept as a trap, as `range` keeps
    /// it); a sparse buffer calls the runtime, whose bitmap walk this does not repeat.
    pub(super) fn proved_dense_range(&mut self, handle: Value, type_id: Value, buffer_type: u32, index: Value, count: Value, source: Option<Value>) -> Result<()> {
        use super::storage::Runtime;
        let flags = MemFlagsData::new();
        self.reads.clear();
        if !self.dense(buffer_type)? {
            match source {
                Some(source) => { self.runtime(Runtime::InitSlice, &[handle, type_id, index, source, count])?; }
                None => { self.runtime(Runtime::TakePrefix, &[handle, type_id, count])?; }
            }
            return Ok(());
        }
        let expected = if observed() {
            let kind = self.constant(u64::from(buffer_type) + 1);
            let (from, mode) = if source.is_some() { (index, self.constant(0)) } else { (self.constant(0), self.constant(1)) };
            Some(self.runtime_value(Runtime::Observe, &[handle, kind, from, count, mode])?)
        } else {
            None
        };
        let buffer = self.header(handle, buffer_type)?;
        let prefix = self.fb.ins().uload32(flags, buffer, PREFIX);
        let some = self.fb.ins().icmp_imm_u(IntCC::NotEqual, count, 0);
        match source {
            Some(source) => {
                // Initialize `index..index + count`: at the prefix, which moves to the end.
                let elsewhere = self.fb.ins().icmp(IntCC::NotEqual, index, prefix);
                let wrong = self.fb.ins().band(some, elsewhere);
                self.fb.ins().trapnz(wrong, TRAP);
                let stride = self.fb.ins().load(I64, flags, buffer, STRIDE);
                let offset = self.fb.ins().imul(index, stride);
                let payload = self.fb.ins().iadd_imm_s(buffer, HEADER);
                let target = self.fb.ins().iadd(payload, offset);
                if let Some(expected) = expected {
                    let other = self.fb.ins().icmp(IntCC::NotEqual, target, expected);
                    self.fb.ins().trapnz(other, TRAP);
                }
                let bytes = self.fb.ins().imul(count, stride);
                self.runtime(Runtime::CopySlice, &[target, source, bytes])?;
                let end = self.fb.ins().iadd(index, count);
                let next = self.fb.ins().select(some, end, prefix);
                let next = self.fb.ins().ireduce(I32, next);
                self.fb.ins().store(flags, next, buffer, PREFIX);
            }
            None => {
                // Take the first `count`: all there is, after which none is left.
                let elsewhere = self.fb.ins().icmp(IntCC::NotEqual, count, prefix);
                let wrong = self.fb.ins().band(some, elsewhere);
                self.fb.ins().trapnz(wrong, TRAP);
                let zero = self.fb.ins().iconst(I64, 0);
                let next = self.fb.ins().select(some, zero, prefix);
                let next = self.fb.ins().ireduce(I32, next);
                self.fb.ins().store(flags, next, buffer, PREFIX);
            }
        }
        Ok(())
    }
}
