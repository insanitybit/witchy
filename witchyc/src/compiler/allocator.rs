//! Reviewed typed allocator TCB. Planning is read-only; commit cannot return Err.
//!
//! Program memory (index 0): frames below `STACK_HIGH`, cell payloads in the heap.
//! Allocator memory (index 1): entries, two search hints, the occupancy map (one byte per
//! quantum) and the cell bitmap (one bit per heap byte). Only the helpers here name memory 1.
use super::{allocator_code::Code, indices::Indices, ir::Program};
use std::cell::Cell;

/// Sizes a build may choose. Everything else in the layout follows from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heap {
    /// Pages of program memory; the heap is what lies above `HEAP_LOW`.
    pub pages: u32,
    /// Allocations that can be live at once. A power of two.
    pub slots: u64,
    /// Bytes of call frames.
    pub stack: u32,
}
impl Heap {
    pub const DEFAULT: Heap = Heap { pages: 3, slots: 64, stack: 16_384 };
    pub fn checked(self) -> Result<Self, String> {
        if !(3..=16_384).contains(&self.pages) { return Err("heap pages must be between 3 and 16384".into()); }
        if !self.slots.is_power_of_two() || !(64..=1_048_576).contains(&self.slots) {
            return Err("allocation slots must be a power of two between 64 and 1048576".into());
        }
        if self.stack % 8 != 0 || !(1024..=65_536).contains(&self.stack) {
            return Err("stack bytes must be a multiple of 8 between 1024 and 65536".into());
        }
        Ok(self)
    }
    pub fn stack_low(self) -> u32 { STACK_HIGH - self.stack }
    pub fn heap_high(self) -> u64 { self.pages as u64 * 65_536 }
    pub fn heap_bytes(self) -> u64 { self.heap_high() - HEAP_LOW }
    pub fn quanta(self) -> u64 { self.heap_bytes() / QUANTUM }
    fn slot_bits(self) -> u64 { self.slots.trailing_zeros() as u64 }
    /// Bits that hold the first quantum of an allocation: every quantum index fits.
    pub(super) fn quantum_bits(self) -> u64 { 64 - (self.quanta() - 1).leading_zeros() as u64 }
    /// A token is generation, slot and first quantum packed into 63 bits, plus one. The
    /// generation gets what the other two leave, at most 32 bits, as the entry stores it.
    fn generation_bits(self) -> u64 { (63 - self.slot_bits() - self.quantum_bits()).min(32) }
    /// A slot whose generation reaches this is never used again.
    pub fn generation_max(self) -> u64 { (1u64 << self.generation_bits()) - 1 }
    /// Head of the released-slot list, lowest quantum that may be free, count of slots ever used.
    pub fn hints(self) -> u64 { self.slots * ENTRY_BYTES }
    pub fn occupancy(self) -> u64 { self.hints() + 64 }
    pub fn bitmap_low(self) -> u64 { (self.occupancy() + self.quanta() + 7) & !7 }
    pub fn bitmap_high(self) -> u64 { self.bitmap_low() + self.heap_bytes() / 8 }
    pub fn metadata_pages(self) -> u32 { self.bitmap_high().div_ceil(65_536) as u32 }
}
static CHOSEN: std::sync::OnceLock<Heap> = std::sync::OnceLock::new();
thread_local! { static OVERRIDE: Cell<Option<Heap>> = const { Cell::new(None) }; }
/// Chosen once per process, before anything is emitted.
pub fn configure(heap: Heap) -> Result<(), String> {
    CHOSEN.set(heap.checked()?).map_err(|_| "the heap was already configured".to_owned())
}
pub(super) fn heap() -> Heap { OVERRIDE.with(Cell::get).or_else(|| CHOSEN.get().copied()).unwrap_or(Heap::DEFAULT) }
/// Run one request with its own validated memory layout.
pub(super) fn with_heap<T>(heap: Heap, body: impl FnOnce() -> T) -> T {
    let previous = OVERRIDE.with(|cell| cell.replace(Some(heap.checked().expect("validated heap"))));
    let result = body();
    OVERRIDE.with(|cell| cell.set(previous));
    result
}

pub(super) const STACK_HIGH: u32 = 66_560;
pub(super) const HEAP_LOW: u64 = 98_304;
pub(super) const QUANTUM: u64 = 32;
pub(super) const ENTRY_BYTES: u64 = 64;
pub(super) const HANDLE: u32 = 2;
/// Only the allocator's helpers name this memory, which the host checks on every module it runs.
pub(super) const METADATA_MEMORY: u8 = 1;
/// The helpers are the last defined functions of a module that has a heap.
pub(super) const HELPERS: u32 = 7;
pub(super) const ELEMENT_CARRIERS: usize = 30;
/// Private discriminator on the allocator type argument. A boxed allocation is always
/// one initialized T. It has no source Buffer handle or individually mutable cell bitmap.
pub(super) const BOX_TYPE: u64 = 1 << 30;
/// Allocation flag for dense storage. COUNT is the initialized length.
pub(super) const PREFIX_TYPE: u64 = 1 << 29;
const LIVE: u64 = 0;
const GENERATION: u64 = 4;
const TYPE: u64 = 8;
const BASE: u64 = 12;
const CAPACITY: u64 = 16;
const REQUESTED: u64 = 24;
const EXTENT: u64 = 28;
const STRIDE: u64 = 32;
/// Cells of the allocation that hold a value.
const COUNT: u64 = 40;
/// In a released entry whose generation can still advance: the next such entry, as slot + 1.
const NEXT: u64 = 48;
/// The allocation's first quantum, as its token carries it; zero for an allocation of no bytes.
const FIRST: u64 = 56;
/// Head of the list of released slots, as slot + 1; zero when there is none.
const FREE_SLOT: u64 = 0;
/// How many slots have ever been used. Slots at or above it have never held an allocation.
const FRESH_SLOT: u64 = 16;
const FREE_QUANTUM: u64 = 8;
/// The token and type of the last entry the access audit passed, or zero. `try_alloc` and
/// `try_release` are the only writers of an entry's LIVE, GENERATION, FIRST and TYPE, and each
/// clears this pair where it commits, so an access naming the same pair would pass the same
/// audit again and is not put through it (2026-11-22: 45 instructions of every range call).
/// (A proved `cell` call, modes 3 and 4, writes a bitmap byte at an index its proof bounds; with
/// that proof violated the byte could be an entry's, and a remembered token would not be audited
/// again. The audit never was a defence against a false proof: the same write could set LIVE.)
const AUDITED_TOKEN: u64 = 24;
const AUDITED_TYPE: u64 = 32;
/// Eight occupied quanta, read or written as one word.
const OCCUPIED: u64 = 0x0101_0101_0101_0101;
// i64 opcodes used below.
const EQZ: u8 = 0x50; const EQ: u8 = 0x51; const NE: u8 = 0x52; const LT: u8 = 0x54; const GT: u8 = 0x56;
const LE: u8 = 0x58; const GE: u8 = 0x5a; const ADD: u8 = 0x7c; const SUB: u8 = 0x7d; const MUL: u8 = 0x7e;
const DIV: u8 = 0x80; const REM: u8 = 0x82; const AND: u8 = 0x83; const OR: u8 = 0x84; const XOR: u8 = 0x85;
const SHL: u8 = 0x86; const SHR: u8 = 0x88;
const RETURN: u8 = 0x0f;

pub(super) fn receipt(program: &Program, indices: &Indices) -> String {
    let h = heap();
    let print = indices.print.map(|i| i.to_string()).unwrap_or_else(|| "null".into());
    let authorize = indices.authorize.expect("heap authorizer import");
    let types = program.layouts.buffers().iter().enumerate().map(|(id, buffer)| {
        format!("{{\"buffer_id\":{id},\"allocation_type_id\":{},\"element\":\"{}\",\"stride\":{},\"alignment\":{},\"initialized_operations\":{},\"element_carriers\":{}}}",
            id + 1, buffer.element_name, buffer.stride, buffer.alignment,
            program.layouts.flat(buffer.element).len() <= ELEMENT_CARRIERS, program.layouts.flat(buffer.element).len())
    }).collect::<Vec<_>>().join(",");
    format!("{{\"contract\":\"witchy.typed-buffer.v3\",\"profile\":\"witchy.buffer.v2\",\"memory_pages\":{},\"memory_bytes\":{},\"stack_low\":{},\"stack_high\":{STACK_HIGH},\"heap_low\":{HEAP_LOW},\"heap_high\":{},\"quantum\":{QUANTUM},\"metadata_memory\":{METADATA_MEMORY},\"metadata_pages\":{},\"metadata_slots\":{},\"metadata_entry_bytes\":{ENTRY_BYTES},\"metadata_base\":0,\"free_hints\":{},\"occupancy_base\":{},\"generation_max\":{},\"allocator_handle\":{HANDLE},\"initialization_bitmap_low\":{},\"initialization_bitmap_high\":{},\"element_carrier_limit\":{ELEMENT_CARRIERS},\"imports\":{{\"print\":{print},\"authorize\":{authorize}}},\"helpers\":{{\"type_info\":{},\"validate\":{},\"try_alloc\":{},\"capacity\":{},\"try_release\":{},\"slot_prepare\":{}}},\"type_ids\":[{types}]}}",
        h.pages, h.heap_high(), h.stack_low(), h.heap_high(), h.metadata_pages(), h.slots, h.hints(), h.occupancy(), h.generation_max(), h.bitmap_low(), h.bitmap_high(),
        indices.type_info, indices.validate, indices.try_alloc, indices.capacity, indices.try_release, indices.slot_prepare)
}
pub(super) fn bodies(program: &Program) -> Vec<Vec<u8>> {
    let h = heap();
    let types: Vec<(u64, u64)> = program.layouts.buffers().iter().map(|buffer| (buffer.stride, buffer.alignment)).collect();
    vec![type_info(program), validate(h), try_alloc(h, &types, program.layouts.boxed_operations), capacity(h), try_release(h, &types, program.layouts.boxed_operations), cell(h), range(h)]
}
/// `type_info` written in place: the stride and alignment of type `type_`, or a trap for an
/// unknown one. A call to the helper on every allocation and release cost measurable time.
fn type_lookup(c: &mut Code, types: &[(u64, u64)], type_: u32, stride: u32, align: u32) {
    c.i64(0); c.set(stride); c.i64(0); c.set(align);
    for (id, (size, alignment)) in types.iter().enumerate() {
        c.get(type_); c.i64(id as u64 + 1); c.op(EQ);
        c.if_(|c| { c.i64(*size); c.set(stride); c.i64(*alignment); c.set(align); });
    }
    // Every known type has an alignment of at least one: zero is an unknown ID.
    c.get(align); c.op(EQZ); c.trap_if();
}
fn type_info(program: &Program) -> Vec<u8> {
    let mut c = Code::new(0);
    for (id, buffer) in program.layouts.buffers().iter().enumerate() {
        // Buffer interning is the sole compiled stride/alignment/nominal-ID source.
        c.get(0); c.i64(id as u64 + 1); c.op(EQ);
        c.if_(|c| { c.i64(buffer.stride); c.i64(buffer.alignment); c.op(RETURN); });
    }
    c.op(0x00); // unknown IDs, including an empty table, ALWAYS trap
    c.finish()
}
/// What every buffer operation checks: the token names a live entry of this generation and type,
/// and its count of full cells is within its capacity. Leaves the entry's address in `entry`.
/// The entry's own equations were established by try_alloc and are audited again by try_release;
/// nothing between the two writes an entry, so an access does not re-derive them.
fn validated(c: &mut Code, h: Heap, token: u32, type_: u32, entry: u32, decoded: u32) {
    decoded_entry(c, h, token, entry, decoded);
    entry_audit(c, h, type_, entry, decoded);
    c.load(entry, COUNT, 0x29, 3); c.load(entry, CAPACITY, 0x29, 3); c.op(GT); c.trap_if();
}
/// A token that is not zero, less one, in `decoded`; the address of the entry it names in `entry`.
fn decoded_entry(c: &mut Code, h: Heap, token: u32, entry: u32, decoded: u32) {
    c.get(token); c.op(EQZ); c.trap_if(); // zero rejected before subtraction
    c.get(token); c.i64(1); c.op(SUB); c.set(decoded);
    c.get(decoded); c.i64(h.quantum_bits()); c.op(SHR); c.i64(h.slots - 1); c.op(AND); c.i64(6); c.op(SHL); c.set(entry);
}
/// The entry is live, of the token's generation and place, and of this type: the four facts
/// that only `try_alloc` and `try_release` change.
fn entry_audit(c: &mut Code, h: Heap, type_: u32, entry: u32, decoded: u32) {
    c.load(entry, LIVE, 0x35, 2); c.i64(1); c.op(NE); c.trap_if();
    // Full width: a generation beyond the stored bits equals no stored one.
    c.load(entry, GENERATION, 0x35, 2); c.get(decoded); c.i64(h.quantum_bits() + h.slot_bits()); c.op(SHR); c.i64(1); c.op(ADD); c.op(NE); c.trap_if();
    // The place the token names is the place the entry holds: emitted code forms the address
    // of a proved cell from the token alone.
    c.load(entry, FIRST, 0x29, 3); c.get(decoded); c.i64((1u64 << h.quantum_bits()) - 1); c.op(AND); c.op(NE); c.trap_if();
    c.load(entry, TYPE, 0x35, 2); c.i64(!PREFIX_TYPE); c.op(AND); c.get(type_); c.op(NE); c.trap_if();
}
/// `validated` for an access (not a release): the same zero test and decode; the entry audit
/// unless this token and type are the pair that last passed it with no allocation or release
/// since (`AUDITED_TOKEN`); and the count within the capacity, read every time, because cell
/// operations move the count.
fn accessed(c: &mut Code, h: Heap, token: u32, type_: u32, entry: u32, decoded: u32) {
    accessed_entry(c, h, token, type_, entry, decoded);
    c.load(entry, COUNT, 0x29, 3); c.load(entry, CAPACITY, 0x29, 3); c.op(GT); c.trap_if();
}
/// `accessed` without its last test, for a caller that reads the count and the capacity into
/// locals and makes that test on them.
fn accessed_entry(c: &mut Code, h: Heap, token: u32, type_: u32, entry: u32, decoded: u32) {
    decoded_entry(c, h, token, entry, decoded);
    c.load_at(h.hints() + AUDITED_TOKEN, 0x29, 3); c.get(token); c.op(NE);
    c.load_at(h.hints() + AUDITED_TYPE, 0x29, 3); c.get(type_); c.op(NE);
    c.op(0x72); // i32.or: either differs
    c.if_(|c| {
        entry_audit(c, h, type_, entry, decoded);
        // Remembered only after every test above passed (a failed one trapped).
        c.store_at(h.hints() + AUDITED_TOKEN, |c| c.get(token), 0x37, 3);
        c.store_at(h.hints() + AUDITED_TYPE, |c| c.get(type_), 0x37, 3);
    });
}
/// An entry is about to be written: no token is remembered as audited.
fn forget_audit(c: &mut Code, h: Heap) {
    c.store_at(h.hints() + AUDITED_TOKEN, |c| c.i64(0), 0x37, 3);
}
fn validate(h: Heap) -> Vec<u8> {
    let mut c = Code::new(2); // params token0,type1; entry2,decoded3
    accessed(&mut c, h, 0, 1, 2, 3);
    c.get(2); // ENTRY ADDRESS0 IS VALID SUCCESS, never a null sentinel
    c.finish()
}
/// `remaining` bytes from `address`, eight at a time while eight remain. Both locals are consumed.
fn span(c: &mut Code, address: u32, remaining: u32, word: impl Fn(&mut Code), byte: impl Fn(&mut Code)) {
    for (step, body) in [(8u64, &word as &dyn Fn(&mut Code)), (1, &byte)] {
        c.loop_(|c| {
            c.get(remaining); c.i64(step); c.op(LT); c.branch_if(1);
            body(c);
            c.get(address); c.i64(step); c.op(ADD); c.set(address);
            c.get(remaining); c.i64(step); c.op(SUB); c.set(remaining);
            c.branch(0);
        });
    }
}
/// The full audit of a validated entry, before release writes anything: every equation between
/// its fields, its place in the heap, and the occupancy of its whole extent.
/// `type_` is the type parameter, `entry` the validated entry; `l` is nine free i64 locals.
fn audit(c: &mut Code, h: Heap, types: &[(u64, u64)], type_: u32, entry: u32, l: u32) {
    let (stride, align, capacity, requested, quanta, base, first, remaining, address) = (l, l + 1, l + 2, l + 3, l + 4, l + 5, l + 6, l + 7, l + 8);
    type_lookup(c, types, type_, stride, align);
    c.load(entry, STRIDE, 0x29, 3); c.get(stride); c.op(NE); c.trap_if();
    c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
    c.get(capacity); c.i64(h.heap_bytes()); c.get(stride); c.op(DIV); c.op(GT); c.trap_if();
    c.load(entry, REQUESTED, 0x35, 2); c.set(requested);
    c.get(capacity); c.get(stride); c.op(MUL); c.get(requested); c.op(NE); c.trap_if();
    c.load(entry, EXTENT, 0x35, 2); c.set(quanta);
    c.get(requested); c.i64(QUANTUM - 1); c.op(ADD); c.i64(5); c.op(SHR);
    c.get(quanta); c.op(NE); c.trap_if();
    c.load(entry, BASE, 0x35, 2); c.set(base);
    c.get(capacity); c.op(EQZ);
    c.if_else(|c| {
        c.get(base); c.i64(0); c.op(NE); c.trap_if();
        c.get(quanta); c.i64(0); c.op(NE); c.trap_if();
        c.load(entry, FIRST, 0x29, 3); c.i64(0); c.op(NE); c.trap_if();
    }, |c| {
        c.get(base); c.i64(HEAP_LOW); c.op(LT); c.trap_if();
        c.get(base); c.i64(h.heap_high()); c.op(GE); c.trap_if();
        c.get(base); c.i64(HEAP_LOW); c.op(SUB); c.i64(QUANTUM - 1); c.op(AND);
        c.i64(0); c.op(NE); c.trap_if();
        c.get(base); c.get(align); c.op(REM); c.i64(0); c.op(NE); c.trap_if();
        c.get(quanta); c.i64(h.quanta()); c.op(GT); c.trap_if();
        c.get(base); c.get(quanta); c.i64(5); c.op(SHL); c.op(ADD);
        c.i64(h.heap_high()); c.op(GT); c.trap_if();
        c.get(base); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
        c.load(entry, FIRST, 0x29, 3); c.get(first); c.op(NE); c.trap_if();
        c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address);
        c.get(quanta); c.set(remaining);
        span(c, address, remaining,
            |c| { c.load(address, 0, 0x29, 0); c.i64(OCCUPIED); c.op(NE); c.trap_if(); },
            |c| { c.load(address, 0, 0x31, 0); c.i64(1); c.op(NE); c.trap_if(); });
    });
}
fn allocation_error(c: &mut Code, error: u64) {
    c.i32(1); c.i64(0); c.i64(error); c.op(RETURN);
}
fn try_alloc(h: Heap, types: &[(u64, u64)], boxed: bool) -> Vec<u8> {
    // params handle0,type1,capacity2; locals stride3,align4,requested5,quanta6,slot7,entry8,
    // oldGeneration9,first10,start11,cursor12,base13,newGeneration14,address15,remaining16,word17,lowest18.
    let (stride, align, requested, quanta, slot, entry, old, first, start, cursor, base, new, address, remaining, word, lowest, head) =
        (3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19);
    let mut c = Code::new(17);
    c.get(0); c.i32(HANDLE); c.op(0x47); c.trap_if();
    let allocation_flags = PREFIX_TYPE | if boxed { BOX_TYPE } else { 0 };
    c.get(1); c.i64(!allocation_flags); c.op(AND); c.set(word);
    type_lookup(&mut c, types, word, stride, align);
    c.get(2); c.i64(h.heap_bytes()); c.get(stride); c.op(DIV); c.op(GT);
    c.if_(|c| allocation_error(c, 1)); // before multiplication, widened capacity arithmetic
    c.get(2); c.get(stride); c.op(MUL); c.set(requested);
    c.get(requested); c.i64(QUANTUM - 1); c.op(ADD); c.i64(5); c.op(SHR); c.set(quanta);
    // READ-ONLY plan. A released slot if there is one, else the next never-used slot. Only
    // try_release puts a slot on the list, and only one whose generation can still advance;
    // an entry that is live or terminal here means the metadata is not the allocator's own.
    c.load_at(h.hints() + FREE_SLOT, 0x29, 3); c.set(head);
    c.get(head); c.op(EQZ);
    c.if_else(|c| {
        c.load_at(h.hints() + FRESH_SLOT, 0x29, 3); c.set(slot);
        c.get(slot); c.i64(h.slots); c.op(GE);
        c.if_(|c| allocation_error(c, 3)); // metadata failure precedes fragmentation
    }, |c| {
        c.get(head); c.i64(h.slots); c.op(GT); c.trap_if();
        c.get(head); c.i64(1); c.op(SUB); c.set(slot);
    });
    c.get(slot); c.i64(6); c.op(SHL); c.set(entry);
    c.load(entry, LIVE, 0x35, 2); c.i64(0); c.op(NE); c.trap_if();
    c.load(entry, GENERATION, 0x35, 2); c.set(old);
    c.get(old); c.i64(h.generation_max()); c.op(GE); c.trap_if();
    c.get(quanta); c.op(EQZ);
    c.if_else(|c| { c.i64(0); c.set(base); c.i64(0); c.set(first); }, |c| {
        let ordinary = |c: &mut Code| {
        // First fit. No quantum below the hint is free, so no run starts below it.
        // `start` is where the current free run began; `lowest` the first free quantum seen.
        c.i64(h.quanta()); c.set(first); c.i64(h.quanta()); c.set(lowest);
        c.load_at(h.hints() + FREE_QUANTUM, 0x29, 3); c.set(cursor);
        c.get(cursor); c.set(start);
        c.loop_(|c| {
            c.get(cursor); c.i64(h.quanta()); c.op(GE); c.branch_if(1);
            // Eight quanta at once when the cursor is word-aligned and eight remain.
            c.get(cursor); c.i64(7); c.op(AND); c.op(EQZ);
            c.get(cursor); c.i64(8); c.op(ADD); c.i64(h.quanta()); c.op(LE); c.op(0x71);
            c.if_(|c| {
                c.i64(h.occupancy()); c.get(cursor); c.op(ADD); c.set(address);
                c.load(address, 0, 0x29, 0); c.set(word);
                c.get(word); c.i64(OCCUPIED); c.op(EQ);
                c.if_(|c| {
                    c.get(cursor); c.i64(8); c.op(ADD); c.set(cursor); c.get(cursor); c.set(start);
                    c.branch(2); // if/if/loop
                });
                c.get(word); c.op(EQZ);
                c.if_(|c| {
                    c.get(lowest); c.i64(h.quanta()); c.op(EQ); c.if_(|c| { c.get(cursor); c.set(lowest); });
                    c.get(cursor); c.i64(8); c.op(ADD); c.set(cursor);
                    c.get(cursor); c.get(start); c.op(SUB); c.get(quanta); c.op(GE);
                    c.if_(|c| { c.get(start); c.set(first); c.branch(4); }); // if/if/if/loop/exit block
                    c.branch(2);
                });
            });
            c.i64(h.occupancy()); c.get(cursor); c.op(ADD); c.set(address);
            c.load(address, 0, 0x31, 0); c.op(EQZ);
            c.if_else(|c| {
                c.get(lowest); c.i64(h.quanta()); c.op(EQ); c.if_(|c| { c.get(cursor); c.set(lowest); });
                c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor);
                c.get(cursor); c.get(start); c.op(SUB); c.get(quanta); c.op(GE);
                c.if_(|c| { c.get(start); c.set(first); c.branch(3); }); // if/if-else/loop/exit block
            }, |c| { c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor); c.get(cursor); c.set(start); });
            c.branch(0);
        });
        };
        if boxed {
            c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ);
            c.if_else(ordinary, |c| {
                // Congruence makes the metadata slot recoverable from a live owning
                // address at release, without adding a back-pointer to each allocation.
                c.get(slot); c.set(start);
                c.i64(h.quanta()); c.set(first);
                c.loop_(|c| {
                    c.get(start); c.get(quanta); c.op(ADD); c.i64(h.quanta()); c.op(GT); c.branch_if(1);
                    c.i64(0); c.set(cursor);
                    c.loop_(|c| {
                        c.get(cursor); c.get(quanta); c.op(EQ); c.branch_if(1);
                        c.i64(h.occupancy()); c.get(start); c.op(ADD); c.get(cursor); c.op(ADD); c.set(address);
                        c.load(address, 0, 0x31, 0); c.i64(0); c.op(NE); c.branch_if(1);
                        c.get(cursor); c.i64(1); c.op(ADD); c.set(cursor); c.branch(0);
                    });
                    c.get(cursor); c.get(quanta); c.op(EQ);
                    c.if_(|c| { c.get(start); c.set(first); c.branch(2); });
                    c.get(start); c.i64(h.slots); c.op(ADD); c.set(start); c.branch(0);
                });
            });
        } else { ordinary(c); }
        c.get(first); c.i64(h.quanta()); c.op(EQ);
        c.if_(|c| allocation_error(c, 2));
        c.i64(HEAP_LOW); c.get(first); c.i64(5); c.op(SHL); c.op(ADD); c.set(base);
    });
    c.get(slot); c.i64(6); c.op(SHL); c.set(entry);
    c.get(old); c.i64(1); c.op(ADD); c.set(new);
    // COMMIT: all addresses/counts proven bounded above; no Err, call or callback below.
    forget_audit(&mut c, h);
    c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address); c.get(quanta); c.set(remaining);
    span(&mut c, address, remaining,
        |c| c.store(address, 0, |c| c.i64(OCCUPIED), 0x37, 0),
        |c| c.store(address, 0, |c| c.i64(1), 0x3c, 0));
    c.get(1); c.i64(PREFIX_TYPE); c.op(AND); c.op(EQZ);
    c.if_(|c| {
        // A sparse allocation starts with a clear bitmap. Quanta isolate bitmap bytes.
        c.i64(h.bitmap_low()); c.get(first); c.i64(2); c.op(SHL); c.op(ADD); c.set(address);
        c.get(quanta); c.i64(2); c.op(SHL); c.set(remaining);
        span(c, address, remaining,
            |c| c.store(address, 0, |c| c.i64(0), 0x37, 0),
            |c| c.store(address, 0, |c| c.i64(0), 0x3c, 0));
    });
    // Take the slot off the list it came from.
    c.get(head); c.op(EQZ);
    c.if_else(|c| c.store_at(h.hints() + FRESH_SLOT, |c| { c.get(slot); c.i64(1); c.op(ADD); }, 0x37, 3),
        |c| c.store_at(h.hints() + FREE_SLOT, |c| c.load(entry, NEXT, 0x29, 3), 0x37, 3));
    c.get(quanta); c.op(EQZ); c.op(0x45);
    if boxed { c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x71); }
    c.if_(|c| {
        // If the run we took began at the first free quantum seen, nothing below its end is free.
        c.get(lowest); c.get(first); c.op(EQ);
        c.if_else(|c| c.store_at(h.hints() + FREE_QUANTUM, |c| { c.get(first); c.get(quanta); c.op(ADD); }, 0x37, 3),
            |c| c.store_at(h.hints() + FREE_QUANTUM, |c| c.get(lowest), 0x37, 3));
    });
    for (offset, local) in [(GENERATION, new), (TYPE, 1), (BASE, base), (REQUESTED, requested), (EXTENT, quanta)] {
        c.store(entry, offset, |c| c.get(local), 0x3e, 2);
    }
    c.store(entry, CAPACITY, |c| c.get(2), 0x37, 3);
    c.store(entry, STRIDE, |c| c.get(stride), 0x37, 3);
    c.store(entry, COUNT, |c| c.i64(0), 0x37, 3);
    c.store(entry, FIRST, |c| c.get(first), 0x37, 3);
    c.store(entry, LIVE, |c| c.i64(1), 0x3e, 2);
    if boxed {
        c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x45);
        c.if_(|c| { c.i32(0); c.get(base); c.i64(0); c.op(RETURN); });
    }
    // Generation, slot, first quantum; plus one, so that no token is zero.
    c.i32(0);
    c.get(old); c.i64(h.slot_bits()); c.op(SHL); c.get(slot); c.op(ADD);
    c.i64(h.quantum_bits()); c.op(SHL); c.get(first); c.op(ADD);
    c.i64(1); c.op(ADD); c.i64(0);
    c.finish()
}
fn capacity(h: Heap) -> Vec<u8> {
    let mut c = Code::new(2); // params token0/type1; entry2,decoded3
    accessed(&mut c, h, 0, 1, 2, 3);
    c.load(2, CAPACITY, 0x29, 3);
    c.finish()
}
fn try_release(h: Heap, types: &[(u64, u64)], boxed: bool) -> Vec<u8> {
    // params token0/type1; entry2,quanta3,first4,remaining5,address6,decoded7; audit locals 8..17
    let (entry, quanta, first, remaining, address) = (2, 3, 4, 5, 6);
    let mut c = Code::new(15);
    if boxed {
        c.get(1); c.i64(BOX_TYPE); c.op(AND); c.op(EQZ); c.op(0x45);
        c.if_(|c| {
            c.get(0); c.i64(HEAP_LOW); c.op(LT); c.trap_if();
            c.get(0); c.i64(h.heap_high()); c.op(GE); c.trap_if();
            c.get(0); c.i64(QUANTUM - 1); c.op(AND); c.i64(0); c.op(NE); c.trap_if();
            c.get(0); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
            c.get(first); c.i64(h.slots - 1); c.op(AND); c.set(7);
            c.get(7); c.i64(6); c.op(SHL); c.set(entry);
            c.load(entry, GENERATION, 0x35, 2); c.i64(1); c.op(SUB);
            c.i64(h.slot_bits()); c.op(SHL); c.get(7); c.op(ADD);
            c.i64(h.quantum_bits()); c.op(SHL); c.get(first); c.op(ADD); c.i64(1); c.op(ADD); c.set(0);
        });
    }
    validated(&mut c, h, 0, 1, entry, 7);
    if boxed { c.get(1); c.i64(!BOX_TYPE); c.op(AND); c.set(1); }
    audit(&mut c, h, types, 1, entry, 8); // all validation BEFORE mutation
    c.load(entry, COUNT, 0x29, 3); c.i64(0); c.op(GT);
    c.if_(|c| { c.i32(1); c.get(0); c.op(RETURN); }); // exact owner, no writes on Err
    c.load(entry, EXTENT, 0x35, 2); c.set(quanta);
    c.get(quanta); c.i64(0); c.op(GT);
    c.if_(|c| {
        c.load(entry, BASE, 0x35, 2); c.i64(HEAP_LOW); c.op(SUB); c.i64(5); c.op(SHR); c.set(first);
        c.i64(h.occupancy()); c.get(first); c.op(ADD); c.set(address); c.get(quanta); c.set(remaining);
        span(c, address, remaining,
            |c| c.store(address, 0, |c| c.i64(0), 0x37, 0),
            |c| c.store(address, 0, |c| c.i64(0), 0x3c, 0));
        c.get(first); c.load_at(h.hints() + FREE_QUANTUM, 0x29, 3); c.op(LT);
        c.if_(|c| c.store_at(h.hints() + FREE_QUANTUM, |c| c.get(first), 0x37, 3));
    });
    // A slot at its last generation is never used again, so it stays off the list.
    c.load(entry, GENERATION, 0x35, 2); c.i64(h.generation_max()); c.op(LT);
    c.if_(|c| {
        c.store(entry, NEXT, |c| c.load_at(h.hints() + FREE_SLOT, 0x29, 3), 0x37, 3);
        c.store_at(h.hints() + FREE_SLOT, |c| { c.get(entry); c.i64(6); c.op(SHR); c.i64(1); c.op(ADD); }, 0x37, 3);
    });
    forget_audit(&mut c, h);
    c.store(entry, LIVE, |c| c.i64(0), 0x3e, 2); // preserves generation (including terminal MAX)
    c.i32(0); c.i64(0); // Unit Ok, inactive Err token only
    c.finish()
}

/// What emitted code adds to `HEAP_LOW` and to index times stride for the address of a cell
/// whose bound and state a proof has established and whose state the operation leaves as it
/// is: the token's first quantum, in bytes. `mask` and the shift are all it needs to know.
pub(super) fn first_quantum_mask(h: Heap) -> u64 { (1u64 << h.quantum_bits()) - 1 }
/// The one way emitted code reaches a cell it must test or change the state of: token, type, index and what the operation needs.
/// Returns (error, cell address in program memory). The occupancy bitmap and the full-cell count
/// live in the allocator's memory and are read and written only here.
///
/// Modes 0..2 test the index and the cell's state: 0 wants an empty cell and marks it full,
/// 1 wants a full cell and marks it empty, 2 wants a full cell and leaves it. Errors are
/// 1 (out of range) and 2 (wrong state), with nothing written. Modes 3..5 are the same three
/// without the tests, for operations whose bound and state a proof has established.
fn cell_error(c: &mut Code, error: u64) {
    c.i64(error); c.i64(0); c.op(RETURN);
}
/// One pass of a range mode over `cells` (param 4) cells from `element`, mode in param 3: the
/// check (`commit` false: each start bit clear for mode 6, set for 7, else trap) or the commit
/// (set for 6, clear for 7). Where the stride is one byte the run's bits are consecutive, so each
/// step takes as many as remain in the bitmap word holding the next one, under a mask, aligned or
/// not (an 8-byte append at an odd length was 16 single-bit steps: `str`'s `push_text`); other
/// strides go a bit at a time.
fn range_walk(c: &mut Code, h: Heap, commit: bool, (element, step, stride, bit, address, mask, byte): (u32, u32, u32, u32, u32, u32, u32)) {
    const SELECT: u8 = 0x1b;
    c.i64(0); c.set(step);
    c.loop_(|c| {
        c.get(step); c.get(4); c.op(GE); c.branch_if(1);
        c.get(element); c.get(step); c.get(stride); c.op(MUL); c.op(ADD); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.get(stride); c.i64(1); c.op(EQ);
        c.if_else(|c| {
            // At a word's first bit with 64 cells or more left: whole words, each compared with
            // (the check) or written as (the commit) one value, all ones or zero, as long as 64
            // cells remain. The masked step below does the same to one word with a mask of all
            // ones, and builds the mask each time: 1.75 instructions a cell against 0.6, for
            // every range of a long String (2026-11-22). What is left under a word, and a run
            // that starts inside one, go through the masked step.
            c.get(bit); c.i64(63); c.op(AND); c.op(EQZ);
            c.get(4); c.get(step); c.op(SUB); c.i64(64); c.op(GE); c.op(0x71); // i32.and
            c.if_(|c| {
                c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
                // Check: clear (6) or set (7). Commit: set (6) or clear (7).
                c.i64(if commit { u64::MAX } else { 0 }); c.i64(if commit { 0 } else { u64::MAX }); c.get(3); c.i64(6); c.op(EQ); c.op(SELECT); c.set(mask);
                c.loop_(|c| {
                    c.get(4); c.get(step); c.op(SUB); c.i64(64); c.op(LT); c.branch_if(1);
                    if commit { c.store(address, 0, |c| c.get(mask), 0x37, 0); }
                    else { c.load(address, 0, 0x29, 0); c.get(mask); c.op(NE); c.trap_if(); }
                    c.get(address); c.i64(8); c.op(ADD); c.set(address);
                    c.get(step); c.i64(64); c.op(ADD); c.set(step);
                    c.branch(0);
                });
                // Back to the head: it ends the walk, or takes what is left under a word.
                c.branch(2);
            });
            // The word holding `bit`; `byte` = n, the cells taken: those left, at most to the word's end.
            c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
            c.get(4); c.get(step); c.op(SUB); c.set(byte);
            c.i64(64); c.get(bit); c.i64(63); c.op(AND); c.op(SUB); c.set(mask);
            c.get(byte); c.get(mask); c.get(byte); c.get(mask); c.op(LT); c.op(SELECT); c.set(byte);
            // n ones from the bit's place in the word (n = 64 only at place 0; a shift by 64 is by 0).
            c.i64(u64::MAX); c.i64(1); c.get(byte); c.op(SHL); c.i64(1); c.op(SUB); c.get(byte); c.i64(64); c.op(EQ); c.op(SELECT);
            c.get(bit); c.i64(63); c.op(AND); c.op(SHL); c.set(mask);
            // The word, in `bit` (not needed again this step).
            c.load(address, 0, 0x29, 0); c.set(bit);
            if commit {
                // Set (6) or clear (7) the masked bits.
                c.store(address, 0, |c| {
                    c.get(bit); c.get(mask); c.op(OR);
                    c.get(bit); c.get(mask); c.i64(u64::MAX); c.op(XOR); c.op(AND);
                    c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
                }, 0x37, 0);
            } else {
                // All clear (6) or all set (7) under the mask, else a trap.
                c.get(bit); c.get(mask); c.op(AND);
                c.i64(0); c.get(mask); c.get(3); c.i64(6); c.op(EQ); c.op(SELECT);
                c.op(NE); c.trap_if();
            }
            c.get(step); c.get(byte); c.op(ADD); c.set(step);
        }, |c| {
            c.i64(h.bitmap_low()); c.get(bit); c.i64(3); c.op(SHR); c.op(ADD); c.set(address);
            c.i64(1); c.get(bit); c.i64(7); c.op(AND); c.op(SHL); c.set(mask);
            if commit {
                c.load(address, 0, 0x31, 0); c.set(byte);
                c.get(3); c.i64(6); c.op(EQ);
                c.if_else(|c| c.store(address, 0, |c| { c.get(byte); c.get(mask); c.op(OR); }, 0x3c, 0),
                    |c| c.store(address, 0, |c| { c.get(byte); c.get(mask); c.i64(255); c.op(XOR); c.op(AND); }, 0x3c, 0));
            } else {
                c.load(address, 0, 0x31, 0); c.get(mask); c.op(AND); c.op(EQZ);
                c.get(3); c.i64(6); c.op(EQ); c.op(0x47); c.trap_if(); // i32.ne: wrong state for the mode
            }
            c.get(step); c.i64(1); c.op(ADD); c.set(step);
        });
        c.branch(0);
    });
}
fn is_prefix(c: &mut Code, entry: u32) {
    c.load(entry, TYPE, 0x35, 2); c.i64(PREFIX_TYPE); c.op(AND); c.i64(0); c.op(NE);
}
/// The range modes, 6 and 7, of the `cell` helper's interface: the same parameters, the cells
/// `index..index + cells` all empty and marked full (6) or all full and marked empty (7).
fn range(h: Heap) -> Vec<u8> {
    let (entry, capacity, element, bit, address, mask, byte, count) = (5, 7, 8, 9, 10, 11, 12, 13);
    let (step, stride) = (14, 15);
    let _ = (bit, address, mask, byte);
    let mut c = Code::new(11);
    // Mode 6 or 7, in one test: any other mode less six is above one (it wraps below six).
    c.get(3); c.i64(6); c.op(SUB); c.i64(1); c.op(GT); c.trap_if();
    // Modes 6 and 7: `cells` cells from `index` on, all empty and marked full (6) or all full and
    // marked empty (7), for a typed range operation whose payloads the emitted code moves in
    // memory 0 (docs/proposal-cell-ranges.md). The token is audited (`accessed`: or is the one remembered from the audit before). Every bound and
    // every cell's state is checked before anything is written; a contradiction is a trap, as
    // the operation's proof says none can occur. Returns the first cell's address.
    {
        let c = &mut c;
        accessed_entry(c, h, 0, 1, entry, 6);
        c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
        c.load(entry, COUNT, 0x29, 3); c.set(count);
        // The audit's last test, on the two words just read.
        c.get(count); c.get(capacity); c.op(GT); c.trap_if();
        // index <= capacity and cells <= capacity - index: no wrap.
        c.get(2); c.get(capacity); c.op(GT); c.trap_if();
        c.get(4); c.get(capacity); c.get(2); c.op(SUB); c.op(GT); c.trap_if();
        c.load(entry, STRIDE, 0x29, 3); c.set(stride);
        c.load(entry, BASE, 0x35, 2); c.get(2); c.get(stride); c.op(MUL); c.op(ADD); c.set(element);
        // The full-cell count moves by `cells` without leaving 0..capacity.
        c.get(3); c.i64(6); c.op(EQ);
        c.if_else(|c| { c.get(4); c.get(capacity); c.get(count); c.op(SUB); c.op(GT); c.trap_if(); },
            |c| { c.get(4); c.get(count); c.op(GT); c.trap_if(); });
        is_prefix(c, entry);
        c.if_(|c| {
            c.get(4); c.op(EQZ); c.if_(|c| { c.i64(0); c.get(element); c.op(RETURN); });
            c.get(3); c.i64(6); c.op(EQ);
            c.if_else(|c| {
                c.get(2); c.get(count); c.op(NE); c.trap_if();
                c.store(entry, COUNT, |c| { c.get(count); c.get(4); c.op(ADD); }, 0x37, 3);
            }, |c| {
                c.get(2); c.get(4); c.op(ADD); c.get(count); c.op(NE); c.trap_if();
                c.store(entry, COUNT, |c| c.get(2), 0x37, 3);
            });
            c.i64(0); c.get(element); c.op(RETURN);
        });
        // Check: each cell's start bit is clear (6) or set (7). Then COMMIT: set (6) or clear (7)
        // each, then the count once. For one-byte cells the run's bits are consecutive, so an
        // aligned bitmap word (64 cells) or byte (8) is checked and written whole: the same test
        // as cell by cell, fewer steps (the 10 MB String's growth and close were a bit at a time).
        // A run of one-byte cells that lies in one bitmap word (an append of at most eight bytes
        // nearly always does): the walk's one step done once, the same test before the same
        // write. The two passes below each found the word and built the mask again, about 200
        // instructions of every such append (2026-11-22). No cell, no word: the walk does nothing.
        c.get(element); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.get(stride); c.i64(1); c.op(EQ);
        c.get(4); c.i64(0); c.op(NE); c.op(0x71); // i32.and
        c.get(bit); c.i64(63); c.op(AND); c.get(4); c.op(ADD); c.i64(64); c.op(LE); c.op(0x71);
        c.if_else(|c| {
            const SELECT: u8 = 0x1b;
            c.i64(h.bitmap_low()); c.get(bit); c.i64(6); c.op(SHR); c.i64(3); c.op(SHL); c.op(ADD); c.set(address);
            // `cells` ones from the bit's place in the word (64 only at place 0; a shift by 64 is by 0).
            c.i64(u64::MAX); c.i64(1); c.get(4); c.op(SHL); c.i64(1); c.op(SUB); c.get(4); c.i64(64); c.op(EQ); c.op(SELECT);
            c.get(bit); c.i64(63); c.op(AND); c.op(SHL); c.set(mask);
            c.load_from(address, 0x29, 0); c.set(byte);
            // All clear (6) or all set (7) under the mask, else a trap, before the write that
            // sets (6) or clears (7) the masked bits. Each mode's own test and write, not one
            // of each chosen by a select: 13 instructions fewer a call.
            c.get(3); c.i64(6); c.op(EQ);
            c.if_else(|c| {
                c.get(byte); c.get(mask); c.op(AND); c.i64(0); c.op(NE); c.trap_if();
                c.store_to(address, |c| { c.get(byte); c.get(mask); c.op(OR); }, 0x37, 0);
            }, |c| {
                c.get(byte); c.get(mask); c.op(AND); c.get(mask); c.op(NE); c.trap_if();
                c.store_to(address, |c| { c.get(byte); c.get(mask); c.i64(u64::MAX); c.op(XOR); c.op(AND); }, 0x37, 0);
            });
        }, |c| {
            for commit in [false, true] {
                range_walk(c, h, commit, (element, step, stride, bit, address, mask, byte));
            }
        });
        c.get(3); c.i64(6); c.op(EQ);
        c.if_else(|c| c.store(entry, COUNT, |c| { c.get(count); c.get(4); c.op(ADD); }, 0x37, 3),
            |c| c.store(entry, COUNT, |c| { c.get(count); c.get(4); c.op(SUB); }, 0x37, 3));
        c.i64(0); c.get(element);
    }
    c.finish()
}
fn cell(h: Heap) -> Vec<u8> {
    // params token0,type1,index2,mode3,cells4; locals entry5,decoded6,capacity7,element8,bit9,
    // byteAddress10,mask11,byte12,count13 (and two the range modes used, kept so the local count
    // stays). `cells` is read by the range modes only.
    let (entry, capacity, element, bit, address, mask, byte, count) = (5, 7, 8, 9, 10, 11, 12, 13);
    let mut c = Code::new(11);
    // Modes 0..5; the range modes are the `range` helper's, a function of their own so that this
    // one, which every push and take calls, stays as small as it was (binary-trees ran 6-25%
    // slower while the range code shared it).
    // Modes 3 and 4 first, each straight through: a proved init or take of one cell (every
    // `push` and `pop` of a Vec). They compute what the general path below computes for them:
    // the entry decoded from the token without the audit, the cell's address from the entry's
    // base and stride, its bit set (3) or cleared (4), the count moved by one. Neither has a
    // test to keep (their bound and state are the operation's proof). The general path spent
    // 110 instructions on one, most of them on tests of the mode (2026-11-22).
    for (mode, full) in [(3u64, true), (4, false)] {
        c.get(3); c.i64(mode); c.op(EQ);
        c.if_(|c| {
            c.get(0); c.i64(1); c.op(SUB); c.i64(h.quantum_bits()); c.op(SHR); c.i64(h.slots - 1); c.op(AND); c.i64(6); c.op(SHL); c.set(entry);
            c.load(entry, BASE, 0x35, 2); c.get(2); c.load(entry, STRIDE, 0x29, 3); c.op(MUL); c.op(ADD); c.set(element);
            is_prefix(c, entry);
            c.if_(|c| {
                c.get(2); c.load(entry, CAPACITY, 0x29, 3); c.op(GE); c.trap_if();
                c.load(entry, COUNT, 0x29, 3); c.get(2);
                if !full { c.i64(1); c.op(ADD); }
                c.op(NE); c.trap_if();
                c.store(entry, COUNT, |c| {
                    c.get(2);
                    if full { c.i64(1); c.op(ADD); }
                }, 0x37, 3);
                c.i64(0); c.get(element); c.op(RETURN);
            });
            c.get(element); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
            c.i64(h.bitmap_low()); c.get(bit); c.i64(3); c.op(SHR); c.op(ADD); c.set(address);
            c.store_to(address, |c| {
                c.load_from(address, 0x31, 0);
                c.i64(1); c.get(bit); c.i64(7); c.op(AND); c.op(SHL);
                if full { c.op(OR); } else { c.i64(255); c.op(XOR); c.op(AND); }
            }, 0x3c, 0);
            c.store(entry, COUNT, |c| { c.load(entry, COUNT, 0x29, 3); c.i64(1); c.op(if full { ADD } else { SUB }); }, 0x37, 3);
            c.i64(0); c.get(element); c.op(RETURN);
        });
    }
    c.get(3); c.i64(5); c.op(GT); c.trap_if();
    c.get(3); c.i64(6); c.op(LT);
    c.if_(|c| {
        // Modes 3..5 rest, as a proved read does in emitted code, on the borrow checker and the
        // operation's proof for the token being live and of this type: the entry is decoded from it
        // without the audit (decisions.md, "Proved writes skip the token audit"). Modes 0..2 audit it.
        // Mode 2, a checked reference (a lookup: `memory.ref`, the hash map's probes), writes nothing
        // and keeps the zero-token, bounds and occupancy checks below; the token audit's facts
        // (live, generation, place, type, count within capacity) rest on the borrow checker for the
        // owner it is lent from, as modes 3..5 do (decisions.md, "Lookup-only checked reference").
        c.get(3); c.i64(2); c.op(LT);
        c.if_else(|c| accessed(c, h, 0, 1, entry, 6), |c| {
            c.get(3); c.i64(2); c.op(EQ); c.if_(|c| { c.get(0); c.op(EQZ); c.trap_if(); });
            c.get(0); c.i64(1); c.op(SUB); c.i64(h.quantum_bits()); c.op(SHR); c.i64(h.slots - 1); c.op(AND); c.i64(6); c.op(SHL); c.set(entry);
        });
        c.get(3); c.i64(3); c.op(LT);
        c.if_(|c| {
            c.load(entry, CAPACITY, 0x29, 3); c.set(capacity);
            c.get(2); c.get(capacity); c.op(GE); c.if_(|c| cell_error(c, 1)); // full-width before multiply/wrap
        });
        c.load(entry, BASE, 0x35, 2); c.get(2); c.load(entry, STRIDE, 0x29, 3); c.op(MUL); c.op(ADD); c.set(element);
        c.get(3); c.i64(5); c.op(EQ);
        c.if_(|c| { c.i64(0); c.get(element); c.op(RETURN); }); // a proved read touches no cell state
        is_prefix(c, entry);
        c.if_(|c| {
            c.load(entry, COUNT, 0x29, 3); c.set(count);
            c.get(3); c.op(EQZ);
            c.if_else(|c| {
                c.get(2); c.get(count); c.op(NE); c.if_(|c| cell_error(c, 2));
                c.store(entry, COUNT, |c| { c.get(2); c.i64(1); c.op(ADD); }, 0x37, 3);
            }, |c| {
                c.get(3); c.i64(1); c.op(EQ);
                c.if_else(|c| {
                    c.get(2); c.i64(1); c.op(ADD); c.get(count); c.op(NE); c.if_(|c| cell_error(c, 2));
                    c.store(entry, COUNT, |c| c.get(2), 0x37, 3);
                }, |c| {
                    c.get(2); c.get(count); c.op(GE); c.if_(|c| cell_error(c, 2));
                });
            });
            c.i64(0); c.get(element); c.op(RETURN);
        });
        c.get(element); c.i64(HEAP_LOW); c.op(SUB); c.set(bit);
        c.i64(h.bitmap_low()); c.get(bit); c.i64(3); c.op(SHR); c.op(ADD); c.set(address);
        c.i64(1); c.get(bit); c.i64(7); c.op(AND); c.op(SHL); c.set(mask);
        c.load(address, 0, 0x31, 0); c.set(byte);
        c.load(entry, COUNT, 0x29, 3); c.set(count); // validated: count <= capacity
        // The proved modes 3, 4 and 5 have already returned. Only checked
        // init (0), take (1) and lookup (2) reach the bitmap path.
        c.get(byte); c.get(mask); c.op(AND); c.op(EQZ);
        c.if_else(|c| { c.get(count); c.get(capacity); c.op(EQ); c.trap_if(); },
            |c| { c.get(count); c.op(EQZ); c.trap_if(); });
        // Keep the corruption traps above distinct from the normal Empty/Full error.
        c.get(byte); c.get(mask); c.op(AND); c.op(EQZ); c.get(3); c.op(EQZ); c.op(0x47); c.if_(|c| cell_error(c, 2));

        c.get(3); c.op(EQZ);
        c.if_(|c| {
            c.store(address, 0, |c| { c.get(byte); c.get(mask); c.op(OR); }, 0x3c, 0);
            c.store(entry, COUNT, |c| { c.get(count); c.i64(1); c.op(ADD); }, 0x37, 3);
        });
        c.get(3); c.i64(1); c.op(EQ);
        c.if_(|c| {
            c.store(address, 0, |c| { c.get(byte); c.get(mask); c.i64(255); c.op(XOR); c.op(AND); }, 0x3c, 0);
            c.store(entry, COUNT, |c| { c.get(count); c.i64(1); c.op(SUB); }, 0x37, 3);
        });
        c.i64(0); c.get(element);
        c.op(RETURN);
    });
    c.op(0x00); // every mode returned above: unreachable
    c.finish()
}

#[cfg(test)]
#[path = "allocator_reference.rs"]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;

    struct Instruction {
        opcode: u8,
        operand: u64,
    }

    struct Block {
        opener: usize,
        exits: Vec<usize>,
    }

    struct DecodedHelper {
        instructions: Vec<Instruction>,
        local_count: usize,
    }

    fn read_leb(body: &[u8], at: &mut usize, signed: bool) -> u64 {
        let (mut value, mut shift) = (0u64, 0u32);
        loop {
            let byte = body[*at];
            *at += 1;
            if shift < 64 { value |= u64::from(byte & 127) << shift; }
            shift += 7;
            if byte & 128 == 0 {
                if signed && shift < 64 && byte & 64 != 0 { value |= u64::MAX << shift; }
                return value;
            }
        }
    }

    fn decode(body: Vec<u8>) -> DecodedHelper {
        let leb = |at: &mut usize, signed| read_leb(&body, at, signed);
        // Locals: the arguments, then the one declared run of i64.
        let mut at = 0;
        let groups = leb(&mut at, false);
        assert!(groups <= 1, "allocator helper declares at most one group of locals");
        let local_count = if groups == 1 {
            let count = leb(&mut at, false);
            assert_eq!(body[at], 0x7e);
            at += 1;
            count as usize
        } else {
            0
        };
        // Resolve every structured branch once, using the lexical block stack.
        let mut instructions: Vec<Instruction> = Vec::new();
        let mut open: Vec<Block> = Vec::new();
        while at < body.len() {
            let mut opcode = body[at];
            at += 1;
            let index = instructions.len();
            let next = (index + 1) as u64;
            let operand = match opcode {
                0x02..=0x04 => {
                    assert_eq!(body[at], 0x40, "allocator helper blocks have no results");
                    at += 1;
                    open.push(Block { opener: index, exits: Vec::new() });
                    0
                }
                0x05 => {
                    let block = open.last_mut().unwrap();
                    let opener = &mut instructions[block.opener];
                    assert_eq!((opener.opcode, opener.operand), (0x04, 0), "else requires a fresh if");
                    opener.operand = next;
                    block.exits.push(index);
                    0
                }
                0x0b => {
                    if let Some(Block { opener, exits }) = open.pop() {
                        let first = &mut instructions[opener];
                        if first.opcode == 0x04 && first.operand == 0 {
                            first.operand = next;
                        }
                        for exit in exits {
                            instructions[exit].operand = next;
                        }
                    } else {
                        assert_eq!(at, body.len(), "the function end is the final byte");
                        opcode = 0x0f;
                    }
                    0
                }
                0x0c | 0x0d => {
                    let depth = leb(&mut at, false) as usize;
                    let block_index = open.len() - depth - 1;
                    let block = &mut open[block_index];
                    if instructions[block.opener].opcode == 0x03 {
                        (block.opener + 1) as u64
                    } else {
                        block.exits.push(index);
                        0
                    }
                }
                0x20 | 0x21 => leb(&mut at, false),
                0x41 => leb(&mut at, true) & 0xffff_ffff,
                0x42 => leb(&mut at, true),
                0x29 | 0x31 | 0x35 | 0x37 | 0x3c | 0x3e => {
                    let align = leb(&mut at, false);
                    assert_eq!((align & 0x40, leb(&mut at, false), leb(&mut at, false)), (0x40, 1, 0), "a helper names memory 1 at offset 0");
                    0
                }
                _ => 0,
            };
            instructions.push(Instruction { opcode, operand });
        }
        assert!(open.is_empty(), "unclosed allocator helper block");
        DecodedHelper { instructions, local_count }
    }

    #[derive(Default)]
    struct Interpreter {
        locals: Vec<u64>,
        stack: Vec<u64>,
    }

    impl Interpreter {
        /// Run a decoded helper on metadata memory, returning `None` for a trap.
        /// The engine admits only the helper instructions below, all on memory 1.
        fn run(&mut self, helper: &DecodedHelper, arguments: &[u64], memory: &mut [u8]) -> Option<&[u64]> {
            let DecodedHelper { instructions, local_count } = helper;
            let Self { locals, stack } = self;
            locals.clear();
            locals.extend_from_slice(arguments);
            locals.resize(arguments.len() + *local_count, 0);
            stack.clear();
            let mut at = 0;
            let mut steps = 0u64;
            loop {
                steps += 1; assert!(steps < 50_000_000, "a helper that does not end");
                let Instruction { opcode: op, operand } = instructions[at];
                at += 1;
                macro_rules! two {
                    ($a:ident, $b:ident => $result:expr) => {{
                        let $b = stack.pop().unwrap();
                        let left = stack.last_mut().unwrap();
                        let $a = *left;
                        *left = $result;
                    }};
                }
                match op {
                    0x00 => return None,
                    0x02 | 0x03 | 0x0b => {}
                    0x04 => {
                        if stack.pop().unwrap() == 0 { at = operand as usize; }
                    }
                    0x05 | 0x0c => at = operand as usize,
                    0x0d => {
                        if stack.pop().unwrap() != 0 { at = operand as usize; }
                    }
                    0x0f => return Some(stack.as_slice()),
                    0x1b => {
                        let condition = stack.pop().unwrap();
                        let otherwise = stack.pop().unwrap();
                        let selected = stack.last_mut().unwrap();
                        if condition == 0 { *selected = otherwise; }
                    }
                    0x20 => stack.push(locals[operand as usize]),
                    0x21 => locals[operand as usize] = stack.pop().unwrap(),
                    0x41 | 0x42 => stack.push(operand),
                    0x29 | 0x31 | 0x35 => {
                        let top = stack.last_mut().unwrap();
                        let source = memory.get(*top as usize..)?;
                        *top = match op {
                            0x29 => u64::from_le_bytes(*source.first_chunk::<8>()?),
                            0x31 => u64::from(*source.first()?),
                            _ => u64::from(u32::from_le_bytes(*source.first_chunk::<4>()?)),
                        };
                    }
                    0x37 | 0x3c | 0x3e => {
                        let value = stack.pop().unwrap();
                        let address = stack.pop().unwrap() as usize;
                        let destination = memory.get_mut(address..)?;
                        match op {
                            0x37 => *destination.first_chunk_mut::<8>()? = value.to_le_bytes(),
                            0x3c => *destination.first_mut()? = value as u8,
                            _ => *destination.first_chunk_mut::<4>()? = (value as u32).to_le_bytes(),
                        }
                    }
                    0x45 | 0x50 => {
                        let top = stack.last_mut().unwrap();
                        *top = u64::from(*top == 0);
                    }
                    0x47 | 0x52 => two!(a, b => u64::from(a != b)),
                    0x51 => two!(a, b => u64::from(a == b)),
                    0x54 => two!(a, b => u64::from(a < b)),
                    0x56 => two!(a, b => u64::from(a > b)),
                    0x58 => two!(a, b => u64::from(a <= b)),
                    0x5a => two!(a, b => u64::from(a >= b)),
                    0x71 | 0x83 => two!(a, b => a & b),
                    0x72 | 0x84 => two!(a, b => a | b),
                    0x7c => two!(a, b => a.wrapping_add(b)),
                    0x7d => two!(a, b => a.wrapping_sub(b)),
                    0x7e => two!(a, b => a.wrapping_mul(b)),
                    0x80 | 0x82 => {
                        let right = stack.pop().unwrap();
                        let left = stack.last_mut().unwrap();
                        if right == 0 { return None; }
                        *left = if op == 0x80 { *left / right } else { *left % right };
                    }
                    0x85 => two!(a, b => a ^ b),
                    0x86 => two!(a, b => a << (b & 63)),
                    0x88 => two!(a, b => a >> (b & 63)),
                    0xa7 => *stack.last_mut().unwrap() &= 0xffff_ffff,
                    other => panic!("an instruction no helper has: {other:#x}"),
                }
            }
        }
    }
    #[test]
    fn dense_storage_checks_length_and_leaves_the_cell_bitmap_untouched() {
        let h = Heap::DEFAULT;
        let types = [(1, 1)];
        let allocate = decode(try_alloc(h, &types, false));
        let prepare = decode(cell(h));
        let prepare_range = decode(range(h));
        let release = decode(try_release(h, &types, false));
        let mut memory = vec![0; h.metadata_pages() as usize * 65_536];
        let bitmap = h.bitmap_low() as usize..h.bitmap_high() as usize;
        memory[bitmap.clone()].fill(0xa5);

        let mut interpreter = Interpreter::default();
        let result = interpreter.run(&allocate, &[u64::from(HANDLE), 1 | PREFIX_TYPE, 16], &mut memory).unwrap();
        assert_eq!(result[0], 0);
        let token = result[1];
        assert!(interpreter.run(&prepare, &[token, 1, 2, 3, 0], &mut memory).is_none());
        assert_eq!(interpreter.run(&prepare, &[token, 1, 0, 3, 0], &mut memory), Some([0, HEAP_LOW].as_slice()));
        assert_eq!(interpreter.run(&prepare_range, &[token, 1, 1, 6, 8], &mut memory), Some([0, HEAP_LOW + 1].as_slice()));
        assert_eq!(interpreter.run(&prepare, &[token, 1, 9, 2, 0], &mut memory), Some([2, 0].as_slice()));
        assert!(interpreter.run(&prepare, &[token, 1, 1, 4, 0], &mut memory).is_none());
        assert_eq!(interpreter.run(&prepare, &[token, 1, 8, 4, 0], &mut memory), Some([0, HEAP_LOW + 8].as_slice()));
        assert!(interpreter.run(&prepare_range, &[token, 1, 0, 7, 7], &mut memory).is_none());
        assert_eq!(interpreter.run(&release, &[token, 1], &mut memory), Some([1, token].as_slice()));
        assert_eq!(interpreter.run(&prepare_range, &[token, 1, 0, 7, 8], &mut memory), Some([0, HEAP_LOW].as_slice()));
        assert_eq!(interpreter.run(&release, &[token, 1], &mut memory), Some([0, 0].as_slice()));
        assert!(memory[bitmap].iter().all(|byte| *byte == 0xa5));

        let result = interpreter.run(&allocate, &[u64::from(HANDLE), 1, 16], &mut memory).unwrap();
        assert_eq!(result[0], 0);
        let sparse = result[1];
        assert_ne!(token, sparse);
        assert_eq!(interpreter.run(&prepare, &[sparse, 1, 7, 0, 0], &mut memory), Some([0, HEAP_LOW + 7].as_slice()));
        assert_eq!(interpreter.run(&prepare, &[sparse, 1, 0, 2, 0], &mut memory), Some([2, 0].as_slice()));
    }

    /// The helpers written here against the ones of de6ed12b (`allocator_reference.rs`), each on
    /// a memory of its own: 60,000 calls a heap of allocations, releases, cell and range
    /// operations in every mode, with tokens that are live, released, of a slot used again,
    /// of another type, made up or zero, and indices, counts and modes inside and outside what
    /// is allowed. Every call answers the same (the same results, or a trap in both), and after
    /// every call the two memories are equal byte for byte, but for the two words that remember
    /// the token last audited. A count of full cells made wrong in both is answered alike too. The proved single-cell modes, which test nothing, are called as
    /// a proof would allow: a live token, an index in range, a cell in the state they need.
    #[test]
    fn the_helpers_answer_and_write_as_the_reference_does() {
        std::thread::scope(|scope| {
            for (heap, seed) in [
                (Heap::DEFAULT, 0x9e37_79b9_7f4a_7c15),
                (Heap { pages: 5, slots: 128, stack: 4096 }, 0x2545_f491_4f6c_dd1d),
            ] {
                scope.spawn(move || compare_helpers(heap, seed));
            }
        });
    }

    fn compare_helpers(h: Heap, seed: u64) {
        let types = [(1u64, 1u64), (8, 8), (24, 8)];
        let new = [validate(h), try_alloc(h, &types, false), capacity(h), try_release(h, &types, false), cell(h), range(h)].map(decode);
        let old = [reference::validate(h), reference::try_alloc(h, &types, false), reference::capacity(h), reference::try_release(h, &types, false), reference::cell(h), reference::range(h)].map(decode);
        let bytes = h.metadata_pages() as usize * 65_536;
        let (mut ours, mut theirs) = (vec![0u8; bytes], vec![0u8; bytes]);
        let mut actual = Interpreter::default();
        let mut reference = Interpreter::default();
        let mut state = seed;
        let mut random = move |below: u64| { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state % below };
        // Live allocations: token, type, capacity, which cells are full. And tokens no longer live.
        let mut live: Vec<(u64, u64, u64, Vec<bool>)> = Vec::new();
        let mut stale: Vec<(u64, u64)> = Vec::new();
        let (mut traps, mut hits) = (0u32, 0u32);
        for call in 0..60_000u32 {
            // A token to be tried: mostly a live one, sometimes one that is not.
            let choice = random(20);
            let (token, type_, known) = if !live.is_empty() && choice < 14 {
                let at = random(live.len() as u64) as usize;
                (live[at].0, live[at].1, Some(at))
            } else if !stale.is_empty() && choice < 17 {
                let (token, type_) = stale[random(stale.len() as u64) as usize]; (token, type_, None)
            } else if choice == 17 { (random(1 << 20), 1 + random(3), None) }
            else if choice == 18 { (0, 1, None) }
            else if let Some(first) = live.first() { (first.0, 1 + first.1 % 3, None) } // a live token, another type
            else { (1, 1, None) };
            let (helper, arguments): (usize, Vec<u64>) = match random(16) {
                0 | 1 => (1, vec![u64::from(HANDLE), 1 + random(4), if random(8) == 0 { random(1 << 40) } else { random(300) }]),
                2 => (3, vec![token, type_]),
                3 => (if random(2) == 0 { 0 } else { 2 }, vec![token, type_]),
                4..=9 => {
                    let mode = if random(12) == 0 { random(10) } else { random(5) };
                    if mode == 3 || mode == 4 {
                        // As a proof allows only: a live allocation, a cell in range, empty for 3 and full for 4.
                        let Some(at) = known else { continue };
                        let cells = &live[at].3;
                        let Some(index) = (0..cells.len()).map(|k| (k + call as usize) % cells.len()).find(|k| cells[*k] == (mode == 4)) else { continue };
                        (4, vec![token, type_, index as u64, mode, 0])
                    } else {
                        let capacity = known.map_or(8, |at| live[at].2);
                        (4, vec![token, type_, random(capacity + 2), mode, 0])
                    }
                }
                _ => {
                    let capacity = known.map_or(8, |at| live[at].2);
                    let index = random(capacity + 2);
                    let cells = match random(6) { 0 => random(capacity + 3), 1 => capacity.saturating_sub(index), 2 => 0, _ => random(70) };
                    (5, vec![token, type_, index, if random(16) == 0 { random(10) } else { 6 + random(2) }, cells])
                }
            };
            // Now and then a live entry's count of full cells is made wrong in both memories
            // (no helper writes such a thing): the count is read on every access, whatever
            // is remembered of the token, so both must still answer alike.
            if let Some(at) = known && random(4000) == 0 {
                let entry = (((token - 1) >> h.quantum_bits()) & (h.slots - 1)) * ENTRY_BYTES;
                let wrong = (live[at].2 + random(3)).to_le_bytes();
                for memory in [&mut ours, &mut theirs] { memory[(entry + COUNT) as usize..(entry + COUNT) as usize + 8].copy_from_slice(&wrong); }
            }
            let answer = actual.run(&new[helper], &arguments, &mut ours);
            assert_eq!(answer, reference.run(&old[helper], &arguments, &mut theirs), "call {call}: helper {helper} {arguments:?}");
            let remembered = (h.hints() + AUDITED_TOKEN) as usize..(h.hints() + AUDITED_TYPE + 8) as usize;
            assert!(theirs[remembered.clone()].iter().all(|b| *b == 0), "the reference keeps no token");
            assert!(ours[..remembered.start] == theirs[..remembered.start] && ours[remembered.end..] == theirs[remembered.end..],
                "call {call}: helper {helper} {arguments:?} left another memory");
            let Some(answer) = answer else { traps += 1; continue };
            hits += 1;
            // The test's own account of what is live and full, from the answers.
            match (helper, known) {
                (1, _) if answer[0] == 0 => live.push((answer[1], arguments[1], arguments[2], vec![false; arguments[2] as usize])),
                (3, Some(at)) if answer[0] == 0 => { let gone = live.swap_remove(at); stale.push((gone.0, gone.1)); }
                (4, Some(at)) if answer[0] == 0 => match arguments[3] { 0 | 3 => live[at].3[arguments[2] as usize] = true, 1 | 4 => live[at].3[arguments[2] as usize] = false, _ => {} },
                (5, Some(at)) => for k in 0..arguments[4] { live[at].3[(arguments[2] + k) as usize] = arguments[3] == 6; },
                _ => {}
            }
            // Keep room: release an emptied allocation now and then.
            if live.len() > 40 { let gone = live.swap_remove(0); stale.push((gone.0, gone.1)); let arguments = [gone.0, gone.1];
                // It may hold full cells: then both refuse, and it stays live in both.
                let answer = actual.run(&new[3], &arguments, &mut ours);
                assert_eq!(answer, reference.run(&old[3], &arguments, &mut theirs));
                if answer.is_some_and(|a| a[0] != 0) { stale.pop(); live.push(gone); if live.len() > 60 { live.swap(0, 50); } }
            }
        }
        assert!(traps > 3_000 && hits > 20_000, "{traps} traps, {hits} answers");
    }
    #[test]
    fn layout_regions_are_ordered_and_fit_their_pages() {
        for heap in [Heap::DEFAULT, Heap { pages: 5, slots: 128, stack: 4096 }, Heap { pages: 16_384, slots: 1_048_576, stack: 65_536 }] {
            let h = heap.checked().expect("supported sizes");
            assert!(h.stack_low() >= 1024 && h.stack_low() < STACK_HIGH && (STACK_HIGH as u64) <= HEAP_LOW);
            assert_eq!(h.heap_bytes() % QUANTUM, 0);
            assert_eq!(h.hints(), h.slots * ENTRY_BYTES);
            assert!(h.occupancy() >= h.hints() + 16);
            assert!(h.bitmap_low() >= h.occupancy() + h.quanta() && h.bitmap_low() % 8 == 0);
            assert_eq!(h.bitmap_high() - h.bitmap_low(), h.quanta() * 4);
            assert!(h.bitmap_high() <= h.metadata_pages() as u64 * 65_536);
            // Every address a helper forms fits the i32 it is wrapped to.
            assert!(h.bitmap_high() < 1 << 31 && h.heap_high() <= 1 << 30);
        }
        assert_eq!((Heap::DEFAULT.occupancy(), Heap::DEFAULT.bitmap_low(), Heap::DEFAULT.bitmap_high(), Heap::DEFAULT.metadata_pages()), (4160, 7232, 19_520, 1));
    }
    #[test]
    fn unsupported_sizes_are_refused() {
        for heap in [Heap { pages: 2, ..Heap::DEFAULT }, Heap { pages: 16_385, ..Heap::DEFAULT }, Heap { slots: 96, ..Heap::DEFAULT },
            Heap { slots: 32, ..Heap::DEFAULT }, Heap { stack: 1020, ..Heap::DEFAULT }, Heap { stack: 65_544, ..Heap::DEFAULT }] {
            assert!(heap.checked().is_err(), "{heap:?}");
        }
    }
    #[test]
    fn a_thread_can_build_with_its_own_sizes() {
        let chosen = Heap { pages: 9, slots: 256, stack: 2048 };
        assert_eq!(with_heap(chosen, heap), chosen);
        assert_eq!(heap(), Heap::DEFAULT);
    }
}
