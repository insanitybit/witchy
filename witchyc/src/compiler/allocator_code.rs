//! Small structured byte emitter using the compiler's sole integer/index codec.
use super::emit::{unsigned, signed, indexed, get, trap_if};

pub(super) struct Code { bytes: Vec<u8> }
impl Code {
    pub fn new(locals: u32) -> Self {
        let mut bytes = vec![u8::from(locals != 0)];
        if locals != 0 { unsigned(&mut bytes, locals); bytes.push(0x7e); }
        Self { bytes }
    }
    pub fn finish(mut self) -> Vec<u8> { self.bytes.push(0x0b); self.bytes }
    pub fn op(&mut self, op: u8) { self.bytes.push(op); }
    pub fn get(&mut self, local: u32) { get(&mut self.bytes, local); }
    pub fn set(&mut self, local: u32) { indexed(&mut self.bytes, 0x21, local); }
    pub fn i64(&mut self, n: u64) { self.op(0x42); signed(&mut self.bytes, n as i64); }
    pub fn i32(&mut self, n: u32) { self.op(0x41); signed(&mut self.bytes, n as i64); }
    pub fn trap_if(&mut self) { trap_if(&mut self.bytes); }
    pub fn branch(&mut self, depth: u32) { indexed(&mut self.bytes, 0x0c, depth); }
    pub fn if_(&mut self, yes: impl FnOnce(&mut Self)) {
        self.bytes.extend([0x04, 0x40]); yes(self); self.op(0x0b);
    }
    pub fn if_else(&mut self, yes: impl FnOnce(&mut Self), no: impl FnOnce(&mut Self)) {
        self.bytes.extend([0x04, 0x40]); yes(self); self.op(0x05); no(self); self.op(0x0b);
    }
    /// Callback sees loop label0 / exit block1; each nested if adds one depth. The body branches itself.
    pub fn loop_(&mut self, body: impl FnOnce(&mut Self)) {
        self.bytes.extend([0x02, 0x40, 0x03, 0x40]); body(self); self.bytes.extend([0x0b, 0x0b]);
    }
    pub fn branch_if(&mut self, depth: u32) { indexed(&mut self.bytes, 0x0d, depth); }
    /// A fixed address in the allocator's memory.
    pub fn load_at(&mut self, address: u64, opcode: u8, align: u8) {
        self.i64(address); self.op(0xa7); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
    pub fn store_at(&mut self, address: u64, value: impl FnOnce(&mut Self), opcode: u8, align: u8) {
        self.i64(address); self.op(0xa7); value(self); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
    /// The address held in a local, as it is.
    pub fn load_from(&mut self, address: u32, opcode: u8, align: u8) {
        self.get(address); self.op(0xa7); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
    pub fn store_to(&mut self, address: u32, value: impl FnOnce(&mut Self), opcode: u8, align: u8) {
        self.get(address); self.op(0xa7); value(self); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
    pub fn address(&mut self, base: u32, offset: u64) {
        self.get(base); self.i64(offset); self.op(0x7c); self.op(0xa7);
    }
    /// Every helper access is to the allocator's own memory (index 1): alignment with the
    /// memory-index flag, the index, then a zero offset. Program memory (index 0) is never named here.
    pub fn load(&mut self, base: u32, offset: u64, opcode: u8, align: u8) {
        self.address(base, offset); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
    pub fn store(&mut self, base: u32, offset: u64, value: impl FnOnce(&mut Self), opcode: u8, align: u8) {
        self.address(base, offset); value(self); self.bytes.extend([opcode, align | 0x40, super::allocator::METADATA_MEMORY, 0]);
    }
}
